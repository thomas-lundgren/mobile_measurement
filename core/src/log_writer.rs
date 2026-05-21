use arrow::array::{RecordBatch, StringArray, UInt32Array, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::timestamp::Timestamp;

/// One packet log entry.
#[derive(Debug, Clone)]
pub struct PacketRow {
    pub flow_id: u32,
    pub sequence_number: u64,
    pub direction: Direction,
    pub timestamp: Timestamp,
    pub payload_size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Sent,
    Received,
}

impl Direction {
    fn as_str(&self) -> &'static str {
        match self {
            Direction::Sent => "SENT",
            Direction::Received => "RECEIVED",
        }
    }
}

fn packets_schema() -> Schema {
    Schema::new(vec![
        Field::new("flow_id", DataType::UInt32, false),
        Field::new("sequence_number", DataType::UInt64, false),
        Field::new("direction", DataType::Utf8, false),
        Field::new("mono_ns", DataType::UInt64, false),
        Field::new("utc_ns", DataType::UInt64, false),
        Field::new("payload_size", DataType::UInt32, false),
    ])
}

/// Segmented Parquet writer: closes the current file every `segment_rows`
/// rows so that a crash kills at most one open segment.
pub struct SegmentedPacketWriter {
    log_dir: PathBuf,
    segment_rows: usize,
    schema: Arc<Schema>,

    // current segment buffer
    flow_ids: Vec<u32>,
    seqs: Vec<u64>,
    directions: Vec<String>,
    mono_ns: Vec<u64>,
    utc_ns: Vec<u64>,
    sizes: Vec<u32>,

    segment_counter: AtomicUsize,
    current_writer: Option<ArrowWriter<File>>,
    current_path: Option<PathBuf>,
    rows_in_segment: usize,
}

impl SegmentedPacketWriter {
    pub fn new(log_dir: impl AsRef<Path>, segment_rows: usize) -> std::io::Result<Self> {
        let log_dir = log_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&log_dir)?;
        let schema = Arc::new(packets_schema());
        let mut w = Self {
            log_dir,
            segment_rows,
            schema,
            flow_ids: Vec::with_capacity(segment_rows),
            seqs: Vec::with_capacity(segment_rows),
            directions: Vec::with_capacity(segment_rows),
            mono_ns: Vec::with_capacity(segment_rows),
            utc_ns: Vec::with_capacity(segment_rows),
            sizes: Vec::with_capacity(segment_rows),
            segment_counter: AtomicUsize::new(0),
            current_writer: None,
            current_path: None,
            rows_in_segment: 0,
        };
        w.start_new_segment()?;
        Ok(w)
    }

    pub fn append(&mut self, row: &PacketRow) -> std::io::Result<()> {
        self.flow_ids.push(row.flow_id);
        self.seqs.push(row.sequence_number);
        self.directions.push(row.direction.as_str().to_owned());
        self.mono_ns.push(row.timestamp.mono_ns);
        self.utc_ns.push(row.timestamp.utc_ns);
        self.sizes.push(row.payload_size);
        self.rows_in_segment += 1;

        if self.rows_in_segment >= self.segment_rows {
            self.flush_segment()?;
            self.start_new_segment()?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        if self.rows_in_segment > 0 {
            self.flush_segment()?;
        }
        Ok(())
    }

    fn start_new_segment(&mut self) -> std::io::Result<()> {
        let idx = self.segment_counter.fetch_add(1, Ordering::SeqCst);
        let filename = format!("packets_{:04}.parquet", idx);
        let path = self.log_dir.join(&filename);
        let file = File::create(&path)?;
        let props = WriterProperties::builder()
            .set_compression(parquet::basic::Compression::ZSTD(Default::default()))
            .build();
        let writer = ArrowWriter::try_new(file, self.schema.clone(), Some(props))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        self.current_writer = Some(writer);
        self.current_path = Some(path);
        self.rows_in_segment = 0;
        Ok(())
    }

    fn flush_segment(&mut self) -> std::io::Result<()> {
        if self.rows_in_segment == 0 {
            return Ok(());
        }
        let batch = RecordBatch::try_new(
            self.schema.clone(),
            vec![
                Arc::new(UInt32Array::from(self.flow_ids.clone())) as _,
                Arc::new(UInt64Array::from(self.seqs.clone())) as _,
                Arc::new(StringArray::from(self.directions.clone())) as _,
                Arc::new(UInt64Array::from(self.mono_ns.clone())) as _,
                Arc::new(UInt64Array::from(self.utc_ns.clone())) as _,
                Arc::new(UInt32Array::from(self.sizes.clone())) as _,
            ],
        )
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        if let Some(writer) = self.current_writer.take() {
            let mut writer = writer;
            writer
                .write(&batch)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            writer
                .close()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        }

        self.flow_ids.clear();
        self.seqs.clear();
        self.directions.clear();
        self.mono_ns.clear();
        self.utc_ns.clear();
        self.sizes.clear();
        self.rows_in_segment = 0;
        Ok(())
    }
}

impl Drop for SegmentedPacketWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn dummy_ts() -> Timestamp {
        let utc = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        Timestamp {
            mono_ns: 1_000_000,
            utc_ns: utc,
        }
    }

    #[test]
    fn segmented_flush() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = SegmentedPacketWriter::new(tmp.path(), 3).unwrap();
        for i in 0..7u64 {
            w.append(&PacketRow {
                flow_id: 1,
                sequence_number: i,
                direction: Direction::Sent,
                timestamp: dummy_ts(),
                payload_size: 64,
            })
            .unwrap();
        }
        // flush the remainder
        drop(w);

        let files: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "parquet").unwrap_or(false))
            .collect();
        assert_eq!(files.len(), 3, "expected 3 segments (3+3+1)");
    }
}

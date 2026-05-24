//! Segmented Parquet packet logger plus the real-time SPSC handoff.
//!
//! The schema mirrors `contracts/parquet/packets.parquet.yaml`: constant
//! per-file join columns (device_id/session_id/test_id/node) alongside per-row
//! data. Files are segmented so a crash loses at most one open segment
//! (invariant 1). The generator never writes Parquet directly; it pushes
//! `PacketRow`s onto a wait-free SPSC ring (`LogSink`) drained by a writer
//! thread (`spawn_packet_writer`), dropping and counting on overflow rather
//! than blocking (see `contracts/internal/logger_sink.md`).

use arrow::array::{RecordBatch, StringArray, UInt32Array, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;
use rtrb::{Consumer, Producer, RingBuffer};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::timestamp::Timestamp;

/// Constant-per-file join keys (remediation 2.1). Stored once on the writer;
/// stamped onto every row.
#[derive(Debug, Clone)]
pub struct LogContext {
    pub device_id: String,
    pub session_id: String,
    pub test_id: String,
    pub node: String, // "mobile" | "server"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Sent,
    Received,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Direction::Sent => "SENT",
            Direction::Received => "RECEIVED",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Udp,
    Tcp,
}

impl Protocol {
    fn as_str(self) -> &'static str {
        match self {
            Protocol::Udp => "UDP",
            Protocol::Tcp => "TCP",
        }
    }
}

/// One packet event. `Copy` (no heap) so it lives in preallocated ring slots.
#[derive(Debug, Clone, Copy)]
pub struct PacketRow {
    pub flow_id: u32,
    pub sequence_number: u64,
    pub direction: Direction,
    pub timestamp: Timestamp,
    pub payload_size: u32,
    pub protocol: Protocol,
    pub local_port: Option<u32>,
    pub remote_port: Option<u32>,
    /// Embedded sender timestamps (received realtime packets); None otherwise.
    pub sender: Option<Timestamp>,
    pub expected_interval_ns: Option<u64>,
    pub actual_interval_ns: Option<u64>,
}

impl PacketRow {
    /// Minimal probe-style row (UDP, no ports/sender/intervals).
    pub fn probe(
        flow_id: u32,
        sequence_number: u64,
        direction: Direction,
        timestamp: Timestamp,
        payload_size: u32,
    ) -> Self {
        Self {
            flow_id,
            sequence_number,
            direction,
            timestamp,
            payload_size,
            protocol: Protocol::Udp,
            local_port: None,
            remote_port: None,
            sender: None,
            expected_interval_ns: None,
            actual_interval_ns: None,
        }
    }
}

fn packets_schema() -> Schema {
    Schema::new(vec![
        // join columns (constant per file)
        Field::new("device_id", DataType::Utf8, false),
        Field::new("session_id", DataType::Utf8, false),
        Field::new("test_id", DataType::Utf8, false),
        Field::new("node", DataType::Utf8, false),
        // data
        Field::new("flow_id", DataType::UInt32, false),
        Field::new("sequence_number", DataType::UInt64, false),
        Field::new("direction", DataType::Utf8, false),
        Field::new("mono_ns", DataType::UInt64, false),
        Field::new("utc_ns", DataType::UInt64, false),
        Field::new("payload_size", DataType::UInt32, false),
        Field::new("protocol", DataType::Utf8, false),
        Field::new("local_port", DataType::UInt32, true),
        Field::new("remote_port", DataType::UInt32, true),
        Field::new("sender_mono_ns", DataType::UInt64, true),
        Field::new("sender_utc_ns", DataType::UInt64, true),
        Field::new("expected_interval_ns", DataType::UInt64, true),
        Field::new("actual_interval_ns", DataType::UInt64, true),
    ])
}

/// Synchronous segmented writer. Buffers rows, closes a segment every
/// `segment_rows`. Used by the writer thread; usable standalone in tests.
pub struct SegmentedPacketWriter {
    log_dir: PathBuf,
    segment_rows: usize,
    schema: Arc<Schema>,
    ctx: LogContext,
    /// Filename stem: segments are `<stem>_NNNN.parquet`. Defaults to "packets"
    /// (one writer per node); the server runs one writer per flow and uses a
    /// flow-scoped stem (`packets_fN`) so concurrent flows don't collide.
    stem: String,
    rows: Vec<PacketRow>,
    segment_counter: usize,
    current: Option<(ArrowWriter<File>, PathBuf)>,
}

impl SegmentedPacketWriter {
    pub fn new(
        log_dir: impl AsRef<Path>,
        segment_rows: usize,
        ctx: LogContext,
    ) -> std::io::Result<Self> {
        Self::with_stem(log_dir, segment_rows, ctx, "packets")
    }

    pub fn with_stem(
        log_dir: impl AsRef<Path>,
        segment_rows: usize,
        ctx: LogContext,
        stem: &str,
    ) -> std::io::Result<Self> {
        let log_dir = log_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&log_dir)?;
        let mut w = Self {
            log_dir,
            segment_rows,
            schema: Arc::new(packets_schema()),
            ctx,
            stem: stem.to_string(),
            rows: Vec::with_capacity(segment_rows),
            segment_counter: 0,
            current: None,
        };
        w.start_new_segment()?;
        Ok(w)
    }

    pub fn append(&mut self, row: &PacketRow) -> std::io::Result<()> {
        self.rows.push(*row);
        if self.rows.len() >= self.segment_rows {
            self.flush_segment()?;
            self.start_new_segment()?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        if !self.rows.is_empty() {
            self.flush_segment()?;
        }
        Ok(())
    }

    fn start_new_segment(&mut self) -> std::io::Result<()> {
        let filename = format!("{}_{:04}.parquet", self.stem, self.segment_counter);
        self.segment_counter += 1;
        let path = self.log_dir.join(&filename);
        let file = File::create(&path)?;
        let props = WriterProperties::builder()
            .set_compression(parquet::basic::Compression::ZSTD(Default::default()))
            .build();
        let writer = ArrowWriter::try_new(file, self.schema.clone(), Some(props))
            .map_err(std::io::Error::other)?;
        self.current = Some((writer, path));
        Ok(())
    }

    fn flush_segment(&mut self) -> std::io::Result<()> {
        if self.rows.is_empty() {
            return Ok(());
        }
        let n = self.rows.len();
        let rep = |s: &str| StringArray::from(vec![s; n]);
        let batch = RecordBatch::try_new(
            self.schema.clone(),
            vec![
                Arc::new(rep(&self.ctx.device_id)) as _,
                Arc::new(rep(&self.ctx.session_id)) as _,
                Arc::new(rep(&self.ctx.test_id)) as _,
                Arc::new(rep(&self.ctx.node)) as _,
                Arc::new(UInt32Array::from(
                    self.rows.iter().map(|r| r.flow_id).collect::<Vec<_>>(),
                )) as _,
                Arc::new(UInt64Array::from(
                    self.rows
                        .iter()
                        .map(|r| r.sequence_number)
                        .collect::<Vec<_>>(),
                )) as _,
                Arc::new(StringArray::from(
                    self.rows
                        .iter()
                        .map(|r| r.direction.as_str())
                        .collect::<Vec<_>>(),
                )) as _,
                Arc::new(UInt64Array::from(
                    self.rows
                        .iter()
                        .map(|r| r.timestamp.mono_ns)
                        .collect::<Vec<_>>(),
                )) as _,
                Arc::new(UInt64Array::from(
                    self.rows
                        .iter()
                        .map(|r| r.timestamp.utc_ns)
                        .collect::<Vec<_>>(),
                )) as _,
                Arc::new(UInt32Array::from(
                    self.rows.iter().map(|r| r.payload_size).collect::<Vec<_>>(),
                )) as _,
                Arc::new(StringArray::from(
                    self.rows
                        .iter()
                        .map(|r| r.protocol.as_str())
                        .collect::<Vec<_>>(),
                )) as _,
                Arc::new(UInt32Array::from(
                    self.rows.iter().map(|r| r.local_port).collect::<Vec<_>>(),
                )) as _,
                Arc::new(UInt32Array::from(
                    self.rows.iter().map(|r| r.remote_port).collect::<Vec<_>>(),
                )) as _,
                Arc::new(UInt64Array::from(
                    self.rows
                        .iter()
                        .map(|r| r.sender.map(|t| t.mono_ns))
                        .collect::<Vec<_>>(),
                )) as _,
                Arc::new(UInt64Array::from(
                    self.rows
                        .iter()
                        .map(|r| r.sender.map(|t| t.utc_ns))
                        .collect::<Vec<_>>(),
                )) as _,
                Arc::new(UInt64Array::from(
                    self.rows
                        .iter()
                        .map(|r| r.expected_interval_ns)
                        .collect::<Vec<_>>(),
                )) as _,
                Arc::new(UInt64Array::from(
                    self.rows
                        .iter()
                        .map(|r| r.actual_interval_ns)
                        .collect::<Vec<_>>(),
                )) as _,
            ],
        )
        .map_err(std::io::Error::other)?;

        if let Some((mut writer, _)) = self.current.take() {
            writer.write(&batch).map_err(std::io::Error::other)?;
            writer.close().map_err(std::io::Error::other)?;
        }
        self.rows.clear();
        Ok(())
    }
}

impl Drop for SegmentedPacketWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

// --- Real-time SPSC handoff ------------------------------------------------

/// Producer end held by a generator thread. `push` is wait-free; on a full
/// ring it drops the row and bumps `dropped` (never blocks).
pub struct LogSink {
    tx: Producer<PacketRow>,
    dropped: Arc<AtomicU64>,
}

impl LogSink {
    /// Record a row. Returns false if the row was dropped (ring full).
    pub fn push(&mut self, row: PacketRow) -> bool {
        match self.tx.push(row) {
            Ok(()) => true,
            Err(_) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Rows dropped to overflow so far. Surfaced in summary.json + an ERROR
    /// event; a non-zero value marks the test PARTIAL.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Create the SPSC channel: a `LogSink` producer + the raw consumer for a
/// writer thread.
pub fn packet_channel(capacity: usize) -> (LogSink, Consumer<PacketRow>, Arc<AtomicU64>) {
    let (tx, rx) = RingBuffer::new(capacity);
    let dropped = Arc::new(AtomicU64::new(0));
    (
        LogSink {
            tx,
            dropped: dropped.clone(),
        },
        rx,
        dropped,
    )
}

/// Handle to a running writer thread.
pub struct WriterHandle {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl WriterHandle {
    /// Signal stop, drain the remainder, finalize segments, join.
    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Spawn the writer thread: drains `rx` into segmented Parquet until `shutdown`
/// is called and the ring is empty.
pub fn spawn_packet_writer(
    mut rx: Consumer<PacketRow>,
    log_dir: impl AsRef<Path>,
    segment_rows: usize,
    ctx: LogContext,
    stem: &str,
) -> std::io::Result<WriterHandle> {
    let dir = log_dir.as_ref().to_path_buf();
    let mut writer = SegmentedPacketWriter::with_stem(&dir, segment_rows, ctx, stem)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let handle = thread::spawn(move || {
        loop {
            let mut drained = false;
            while let Ok(row) = rx.pop() {
                let _ = writer.append(&row);
                drained = true;
            }
            if stop_thread.load(Ordering::Acquire) && rx.is_empty() {
                break;
            }
            if !drained {
                thread::sleep(Duration::from_millis(2));
            }
        }
        let _ = writer.flush();
    });
    Ok(WriterHandle {
        stop,
        handle: Some(handle),
    })
}

/// Spawn a writer that drains **several** SPSC rings into one segmented file
/// (the fan-in case: many flows → one node `packets.parquet`, each ring still
/// strictly single-producer/single-consumer). See `contracts/internal/logger_sink.md`.
pub fn spawn_packet_writer_multi(
    mut rxs: Vec<Consumer<PacketRow>>,
    log_dir: impl AsRef<Path>,
    segment_rows: usize,
    ctx: LogContext,
) -> std::io::Result<WriterHandle> {
    let dir = log_dir.as_ref().to_path_buf();
    let mut writer = SegmentedPacketWriter::new(&dir, segment_rows, ctx)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let handle = thread::spawn(move || {
        loop {
            let mut drained = false;
            for rx in rxs.iter_mut() {
                while let Ok(row) = rx.pop() {
                    let _ = writer.append(&row);
                    drained = true;
                }
            }
            if stop_thread.load(Ordering::Acquire) && rxs.iter().all(|r| r.is_empty()) {
                break;
            }
            if !drained {
                thread::sleep(Duration::from_millis(2));
            }
        }
        let _ = writer.flush();
    });
    Ok(WriterHandle {
        stop,
        handle: Some(handle),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    fn ctx() -> LogContext {
        LogContext {
            device_id: "dev_1".into(),
            session_id: "ses_1".into(),
            test_id: "tst_1".into(),
            node: "server".into(),
        }
    }

    fn ts(i: u64) -> Timestamp {
        Timestamp {
            mono_ns: 1_000 + i,
            utc_ns: 1_747_900_000_000_000_000 + i,
        }
    }

    fn parquet_files(dir: &Path) -> Vec<PathBuf> {
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "parquet").unwrap_or(false))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn distinct_stems_do_not_collide_in_one_dir() {
        // The server runs one writer per flow into the shared server/ dir; a
        // flow-scoped stem keeps their segments from clobbering each other.
        let tmp = tempfile::tempdir().unwrap();
        let mut a = SegmentedPacketWriter::with_stem(tmp.path(), 100, ctx(), "packets_f1").unwrap();
        let mut b = SegmentedPacketWriter::with_stem(tmp.path(), 100, ctx(), "packets_f2").unwrap();
        a.append(&PacketRow::probe(1, 0, Direction::Received, ts(0), 64))
            .unwrap();
        b.append(&PacketRow::probe(2, 0, Direction::Received, ts(1), 64))
            .unwrap();
        drop(a);
        drop(b);

        let names: Vec<String> = parquet_files(tmp.path())
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["packets_f1_0000.parquet", "packets_f2_0000.parquet"]
        );
    }

    #[test]
    fn segments_and_full_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = SegmentedPacketWriter::new(tmp.path(), 3, ctx()).unwrap();
        for i in 0..7u64 {
            w.append(&PacketRow::probe(1, i, Direction::Sent, ts(i), 64))
                .unwrap();
        }
        drop(w);

        let files = parquet_files(tmp.path());
        assert_eq!(files.len(), 3, "3+3+1 → 3 segments");

        // read back the first segment: full schema + correct join columns
        let file = File::open(&files[0]).unwrap();
        let mut reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap();
        let batch = reader.next().unwrap().unwrap();
        assert_eq!(batch.num_columns(), 17, "join + data columns");
        let names: Vec<String> = batch
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        assert_eq!(names[0], "device_id");
        assert!(names.iter().any(|n| n == "actual_interval_ns"));
        let dev = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(dev.value(0), "dev_1");
    }

    #[test]
    fn ring_overflow_drops_and_counts() {
        // capacity 4, consumer never drains → 4 fit, the rest drop
        let (mut sink, _rx, dropped) = packet_channel(4);
        let mut accepted = 0;
        for i in 0..10u64 {
            if sink.push(PacketRow::probe(1, i, Direction::Sent, ts(i), 64)) {
                accepted += 1;
            }
        }
        assert_eq!(accepted, 4);
        assert_eq!(sink.dropped(), 6);
        assert_eq!(dropped.load(Ordering::Relaxed), 6);
    }

    #[test]
    fn ring_to_writer_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut sink, rx, _dropped) = packet_channel(1024);
        let writer = spawn_packet_writer(rx, tmp.path(), 1000, ctx(), "packets").unwrap();
        for i in 0..500u64 {
            assert!(sink.push(PacketRow::probe(2, i, Direction::Received, ts(i), 30)));
        }
        drop(sink); // close the producer
        writer.shutdown();

        // count rows across segments
        let mut total = 0;
        for f in parquet_files(tmp.path()) {
            let file = File::open(&f).unwrap();
            let reader = ParquetRecordBatchReaderBuilder::try_new(file)
                .unwrap()
                .build()
                .unwrap();
            for b in reader {
                total += b.unwrap().num_rows();
            }
        }
        assert_eq!(total, 500);
    }
}

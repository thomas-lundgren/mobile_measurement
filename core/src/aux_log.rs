//! Low-rate auxiliary logs: gps / clock (radio lands next). Unlike `packets`
//! these are ~1 Hz, single-sampler, fed from the Java shell via JNI push — so
//! they don't need the wait-free SPSC ring. Rows are buffered in memory during
//! the test and written as one Parquet file at stop (each is "once per test").
//!
//! Schemas mirror `contracts/parquet/{gps,clock}.parquet.yaml`. Join columns
//! come from the test context. gps sits at the test root; clock under `mobile/`.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{Float32Array, Float64Array, Int64Array, RecordBatch, StringArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;
use serde::Deserialize;

/// Constant per-file join keys.
#[derive(Debug, Clone)]
pub struct AuxContext {
    pub device_id: String,
    pub session_id: String,
    pub test_id: String,
    pub node: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GpsRow {
    pub mono_ns: u64,
    pub utc_ns: u64,
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default)]
    pub altitude_m: Option<f64>,
    #[serde(default)]
    pub speed_mps: Option<f32>,
    #[serde(default)]
    pub heading_deg: Option<f32>,
    #[serde(default)]
    pub accuracy_m: Option<f32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClockRow {
    pub mono_ns: u64,
    pub utc_ns: u64,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub uncertainty_ns: Option<u64>,
    #[serde(default)]
    pub estimated_offset_ns: Option<i64>,
}

fn zstd_props() -> WriterProperties {
    WriterProperties::builder()
        .set_compression(parquet::basic::Compression::ZSTD(Default::default()))
        .build()
}

fn write_batch(path: &Path, schema: Arc<Schema>, batch: RecordBatch) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut w = ArrowWriter::try_new(File::create(path)?, schema, Some(zstd_props()))
        .map_err(std::io::Error::other)?;
    w.write(&batch).map_err(std::io::Error::other)?;
    w.close().map_err(std::io::Error::other)?;
    Ok(())
}

fn rep(ctx_field: &str, n: usize) -> StringArray {
    StringArray::from(vec![ctx_field; n])
}

fn gps_schema() -> Schema {
    Schema::new(vec![
        Field::new("device_id", DataType::Utf8, false),
        Field::new("session_id", DataType::Utf8, false),
        Field::new("test_id", DataType::Utf8, false),
        Field::new("mono_ns", DataType::UInt64, false),
        Field::new("utc_ns", DataType::UInt64, false),
        Field::new("latitude", DataType::Float64, false),
        Field::new("longitude", DataType::Float64, false),
        Field::new("altitude_m", DataType::Float64, true),
        Field::new("speed_mps", DataType::Float32, true),
        Field::new("heading_deg", DataType::Float32, true),
        Field::new("accuracy_m", DataType::Float32, true),
    ])
}

pub fn write_gps(path: &Path, ctx: &AuxContext, rows: &[GpsRow]) -> std::io::Result<()> {
    let n = rows.len();
    let schema = Arc::new(gps_schema());
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(rep(&ctx.device_id, n)) as _,
            Arc::new(rep(&ctx.session_id, n)) as _,
            Arc::new(rep(&ctx.test_id, n)) as _,
            Arc::new(UInt64Array::from(
                rows.iter().map(|r| r.mono_ns).collect::<Vec<_>>(),
            )) as _,
            Arc::new(UInt64Array::from(
                rows.iter().map(|r| r.utc_ns).collect::<Vec<_>>(),
            )) as _,
            Arc::new(Float64Array::from(
                rows.iter().map(|r| r.latitude).collect::<Vec<_>>(),
            )) as _,
            Arc::new(Float64Array::from(
                rows.iter().map(|r| r.longitude).collect::<Vec<_>>(),
            )) as _,
            Arc::new(Float64Array::from(
                rows.iter().map(|r| r.altitude_m).collect::<Vec<_>>(),
            )) as _,
            Arc::new(Float32Array::from(
                rows.iter().map(|r| r.speed_mps).collect::<Vec<_>>(),
            )) as _,
            Arc::new(Float32Array::from(
                rows.iter().map(|r| r.heading_deg).collect::<Vec<_>>(),
            )) as _,
            Arc::new(Float32Array::from(
                rows.iter().map(|r| r.accuracy_m).collect::<Vec<_>>(),
            )) as _,
        ],
    )
    .map_err(std::io::Error::other)?;
    write_batch(path, schema, batch)
}

fn clock_schema() -> Schema {
    Schema::new(vec![
        Field::new("device_id", DataType::Utf8, false),
        Field::new("session_id", DataType::Utf8, false),
        Field::new("test_id", DataType::Utf8, false),
        Field::new("node", DataType::Utf8, false),
        Field::new("mono_ns", DataType::UInt64, false),
        Field::new("utc_ns", DataType::UInt64, false),
        Field::new("source", DataType::Utf8, true),
        Field::new("uncertainty_ns", DataType::UInt64, true),
        Field::new("estimated_offset_ns", DataType::Int64, true),
    ])
}

pub fn write_clock(path: &Path, ctx: &AuxContext, rows: &[ClockRow]) -> std::io::Result<()> {
    let n = rows.len();
    let schema = Arc::new(clock_schema());
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(rep(&ctx.device_id, n)) as _,
            Arc::new(rep(&ctx.session_id, n)) as _,
            Arc::new(rep(&ctx.test_id, n)) as _,
            Arc::new(rep(&ctx.node, n)) as _,
            Arc::new(UInt64Array::from(
                rows.iter().map(|r| r.mono_ns).collect::<Vec<_>>(),
            )) as _,
            Arc::new(UInt64Array::from(
                rows.iter().map(|r| r.utc_ns).collect::<Vec<_>>(),
            )) as _,
            Arc::new(StringArray::from(
                rows.iter().map(|r| r.source.clone()).collect::<Vec<_>>(),
            )) as _,
            Arc::new(UInt64Array::from(
                rows.iter().map(|r| r.uncertainty_ns).collect::<Vec<_>>(),
            )) as _,
            Arc::new(Int64Array::from(
                rows.iter()
                    .map(|r| r.estimated_offset_ns)
                    .collect::<Vec<_>>(),
            )) as _,
        ],
    )
    .map_err(std::io::Error::other)?;
    write_batch(path, schema, batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    fn ctx() -> AuxContext {
        AuxContext {
            device_id: "dev_1".into(),
            session_id: "ses_1".into(),
            test_id: "tst_1".into(),
            node: "mobile".into(),
        }
    }

    fn rows_in(path: &Path) -> usize {
        let r = ParquetRecordBatchReaderBuilder::try_new(File::open(path).unwrap())
            .unwrap()
            .build()
            .unwrap();
        r.map(|b| b.unwrap().num_rows()).sum()
    }

    #[test]
    fn gps_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gps.parquet");
        let rows = vec![
            GpsRow {
                mono_ns: 1,
                utc_ns: 2,
                latitude: 59.33,
                longitude: 18.06,
                altitude_m: Some(12.0),
                speed_mps: Some(1.5),
                heading_deg: None,
                accuracy_m: Some(4.0),
            },
            GpsRow {
                mono_ns: 3,
                utc_ns: 4,
                latitude: 59.34,
                longitude: 18.05,
                altitude_m: None,
                speed_mps: None,
                heading_deg: Some(90.0),
                accuracy_m: None,
            },
        ];
        write_gps(&p, &ctx(), &rows).unwrap();
        assert_eq!(rows_in(&p), 2);
    }

    #[test]
    fn clock_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("clock.parquet");
        let rows = vec![
            ClockRow {
                mono_ns: 1,
                utc_ns: 2,
                source: Some("gps".into()),
                uncertainty_ns: Some(500),
                estimated_offset_ns: Some(-30),
            },
            ClockRow {
                mono_ns: 3,
                utc_ns: 4,
                source: None,
                uncertainty_ns: None,
                estimated_offset_ns: None,
            },
        ];
        write_clock(&p, &ctx(), &rows).unwrap();
        assert_eq!(rows_in(&p), 2);
    }
}

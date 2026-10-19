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

use arrow::array::{
    Array, ArrayRef, Float32Array, Float64Array, Int32Array, Int32Builder, Int64Array, ListBuilder,
    RecordBatch, StringArray, StructBuilder, UInt32Array, UInt32Builder, UInt64Array,
};
use arrow::datatypes::{DataType, Field, Fields, Schema};
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

/// Radio metric sample (`contracts/parquet/radio.parquet.yaml`). All metric
/// fields nullable (capability-dependent); neighbor_cells is a list of structs.
#[derive(Debug, Clone, Deserialize)]
pub struct RadioRow {
    pub mono_ns: u64,
    pub utc_ns: u64,
    #[serde(default)]
    pub pci: Option<u32>,
    #[serde(default)]
    pub nci: Option<u64>,
    #[serde(default)]
    pub tac: Option<u32>,
    #[serde(default)]
    pub nrarfcn: Option<u32>,
    #[serde(default)]
    pub rsrp: Option<i32>,
    #[serde(default)]
    pub rsrq: Option<i32>,
    #[serde(default)]
    pub sinr: Option<i32>,
    #[serde(default)]
    pub earfcn: Option<u32>,
    #[serde(default)]
    pub lte_pci: Option<u32>,
    #[serde(default)]
    pub lte_rsrp: Option<i32>,
    #[serde(default)]
    pub bandwidth_mhz: Option<u32>,
    #[serde(default)]
    pub cell_connection_status: Option<String>,
    /// Radio mode the device reports (LTE, LTE_CA, NR_NSA, NR_ADVANCED, NR_SA, ...).
    #[serde(default)]
    pub network_type: Option<String>,
    /// Network carrying the data: CELLULAR, WIFI, ETHERNET, OTHER, NONE.
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub wifi_freq_mhz: Option<u32>,
    #[serde(default)]
    pub wifi_channel: Option<u32>,
    #[serde(default)]
    pub wifi_bw_mhz: Option<u32>,
    #[serde(default)]
    pub wifi_standard: Option<String>,
    #[serde(default)]
    pub wifi_rssi: Option<i32>,
    #[serde(default)]
    pub wifi_tx_mbps: Option<u32>,
    #[serde(default)]
    pub wifi_rx_mbps: Option<u32>,
    #[serde(default)]
    pub neighbor_cells: Option<Vec<Neighbor>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Neighbor {
    pub pci: u32,
    #[serde(default)]
    pub rsrp: Option<i32>,
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

fn neighbor_fields() -> Fields {
    Fields::from(vec![
        Field::new("pci", DataType::UInt32, false),
        Field::new("rsrp", DataType::Int32, true),
    ])
}

/// Build the `neighbor_cells` list<struct{pci,rsrp}> column.
fn build_neighbors(rows: &[RadioRow]) -> ArrayRef {
    let fields = neighbor_fields();
    let mut b = ListBuilder::new(StructBuilder::new(
        fields,
        vec![
            Box::new(UInt32Builder::new()),
            Box::new(Int32Builder::new()),
        ],
    ));
    for r in rows {
        match &r.neighbor_cells {
            Some(list) => {
                let sb = b.values();
                for n in list {
                    sb.field_builder::<UInt32Builder>(0)
                        .unwrap()
                        .append_value(n.pci);
                    sb.field_builder::<Int32Builder>(1)
                        .unwrap()
                        .append_option(n.rsrp);
                    sb.append(true);
                }
                b.append(true);
            }
            None => b.append(false),
        }
    }
    Arc::new(b.finish())
}

pub fn write_radio(path: &Path, ctx: &AuxContext, rows: &[RadioRow]) -> std::io::Result<()> {
    let n = rows.len();
    let neighbors = build_neighbors(rows);
    let schema = Arc::new(Schema::new(vec![
        Field::new("device_id", DataType::Utf8, false),
        Field::new("session_id", DataType::Utf8, false),
        Field::new("test_id", DataType::Utf8, false),
        Field::new("mono_ns", DataType::UInt64, false),
        Field::new("utc_ns", DataType::UInt64, false),
        Field::new("pci", DataType::UInt32, true),
        Field::new("nci", DataType::UInt64, true),
        Field::new("tac", DataType::UInt32, true),
        Field::new("nrarfcn", DataType::UInt32, true),
        Field::new("rsrp", DataType::Int32, true),
        Field::new("rsrq", DataType::Int32, true),
        Field::new("sinr", DataType::Int32, true),
        Field::new("earfcn", DataType::UInt32, true),
        Field::new("lte_pci", DataType::UInt32, true),
        Field::new("lte_rsrp", DataType::Int32, true),
        Field::new("bandwidth_mhz", DataType::UInt32, true),
        Field::new("cell_connection_status", DataType::Utf8, true),
        Field::new("network_type", DataType::Utf8, true),
        Field::new("transport", DataType::Utf8, true),
        Field::new("wifi_freq_mhz", DataType::UInt32, true),
        Field::new("wifi_channel", DataType::UInt32, true),
        Field::new("wifi_bw_mhz", DataType::UInt32, true),
        Field::new("wifi_standard", DataType::Utf8, true),
        Field::new("wifi_rssi", DataType::Int32, true),
        Field::new("wifi_tx_mbps", DataType::UInt32, true),
        Field::new("wifi_rx_mbps", DataType::UInt32, true),
        Field::new("neighbor_cells", neighbors.data_type().clone(), true),
    ]));
    let u32c =
        |f: fn(&RadioRow) -> Option<u32>| UInt32Array::from(rows.iter().map(f).collect::<Vec<_>>());
    let i32c =
        |f: fn(&RadioRow) -> Option<i32>| Int32Array::from(rows.iter().map(f).collect::<Vec<_>>());
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
            Arc::new(u32c(|r| r.pci)) as _,
            Arc::new(UInt64Array::from(
                rows.iter().map(|r| r.nci).collect::<Vec<_>>(),
            )) as _,
            Arc::new(u32c(|r| r.tac)) as _,
            Arc::new(u32c(|r| r.nrarfcn)) as _,
            Arc::new(i32c(|r| r.rsrp)) as _,
            Arc::new(i32c(|r| r.rsrq)) as _,
            Arc::new(i32c(|r| r.sinr)) as _,
            Arc::new(u32c(|r| r.earfcn)) as _,
            Arc::new(u32c(|r| r.lte_pci)) as _,
            Arc::new(i32c(|r| r.lte_rsrp)) as _,
            Arc::new(u32c(|r| r.bandwidth_mhz)) as _,
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|r| r.cell_connection_status.clone())
                    .collect::<Vec<_>>(),
            )) as _,
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|r| r.network_type.clone())
                    .collect::<Vec<_>>(),
            )) as _,
            Arc::new(StringArray::from(
                rows.iter().map(|r| r.transport.clone()).collect::<Vec<_>>(),
            )) as _,
            Arc::new(u32c(|r| r.wifi_freq_mhz)) as _,
            Arc::new(u32c(|r| r.wifi_channel)) as _,
            Arc::new(u32c(|r| r.wifi_bw_mhz)) as _,
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|r| r.wifi_standard.clone())
                    .collect::<Vec<_>>(),
            )) as _,
            Arc::new(i32c(|r| r.wifi_rssi)) as _,
            Arc::new(u32c(|r| r.wifi_tx_mbps)) as _,
            Arc::new(u32c(|r| r.wifi_rx_mbps)) as _,
            neighbors,
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
    fn radio_roundtrip_with_neighbors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("radio.parquet");
        let rows = vec![
            RadioRow {
                mono_ns: 1,
                utc_ns: 2,
                pci: Some(301),
                nci: Some(12345678901),
                tac: Some(7),
                nrarfcn: Some(632448),
                rsrp: Some(-95),
                rsrq: Some(-11),
                sinr: Some(14),
                earfcn: None,
                lte_pci: None,
                lte_rsrp: None,
                bandwidth_mhz: Some(100),
                cell_connection_status: Some("PRIMARY".into()),
                network_type: Some("NR_SA".into()),
                transport: Some("WIFI".into()),
                wifi_freq_mhz: Some(5240),
                wifi_channel: Some(48),
                wifi_bw_mhz: Some(80),
                wifi_standard: Some("802.11ax".into()),
                wifi_rssi: Some(-51),
                wifi_tx_mbps: Some(540),
                wifi_rx_mbps: Some(480),
                neighbor_cells: Some(vec![
                    Neighbor {
                        pci: 302,
                        rsrp: Some(-101),
                    },
                    Neighbor {
                        pci: 303,
                        rsrp: None,
                    },
                ]),
            },
            RadioRow {
                mono_ns: 3,
                utc_ns: 4,
                pci: None,
                nci: None,
                tac: None,
                nrarfcn: None,
                rsrp: None,
                rsrq: None,
                sinr: None,
                earfcn: None,
                lte_pci: None,
                lte_rsrp: None,
                bandwidth_mhz: None,
                cell_connection_status: None,
                network_type: None,
                transport: None,
                wifi_freq_mhz: None,
                wifi_channel: None,
                wifi_bw_mhz: None,
                wifi_standard: None,
                wifi_rssi: None,
                wifi_tx_mbps: None,
                wifi_rx_mbps: None,
                neighbor_cells: None,
            },
        ];
        write_radio(&p, &ctx(), &rows).unwrap();
        // read back: 2 rows, neighbor_cells column present as a list
        use arrow::array::ListArray;
        let mut r = ParquetRecordBatchReaderBuilder::try_new(File::open(&p).unwrap())
            .unwrap()
            .build()
            .unwrap();
        let batch = r.next().unwrap().unwrap();
        assert_eq!(batch.num_rows(), 2);
        let nb = batch.column_by_name("neighbor_cells").unwrap();
        let list = nb.as_any().downcast_ref::<ListArray>().unwrap();
        assert!(
            list.is_valid(0) && list.value(0).len() == 2,
            "row0 has 2 neighbors"
        );
        assert!(list.is_null(1), "row1 has null neighbor_cells");
        let nt = batch
            .column_by_name("network_type")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(nt.value(0), "NR_SA");
        assert!(nt.is_null(1));
        let bw = batch
            .column_by_name("wifi_bw_mhz")
            .unwrap()
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        assert_eq!(bw.value(0), 80);
        assert!(bw.is_null(1));
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

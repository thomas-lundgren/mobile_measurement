//! The Parquet files the core writes match `contracts/parquet/*.parquet.yaml`
//! column for column: name, order, type and nullability. Each file is written
//! for real and its schema read back, so a column added to a writer without
//! the contract (or the other way round) fails here.

use std::fs::File;
use std::path::{Path, PathBuf};

use arrow::datatypes::{DataType, Field};
use fieldtest_core::aux_log::{self, AuxContext, EventRow};
use fieldtest_core::log_writer::{
    Direction, LogContext, PacketRow, Protocol, SegmentedPacketWriter,
};
use fieldtest_core::timestamp::Timestamp;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_yaml::Value;

/// Contract tables no core writer produces (yet).
const NOT_WRITTEN: [&str; 2] = ["pings", "traffic"];

fn contracts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../contracts/parquet")
}

/// "name:type[?]" per column, `?` when nullable; list/struct spelled out.
fn yaml_column(c: &Value) -> String {
    let name = c["name"].as_str().unwrap_or("item");
    let null = if c["nullable"].as_bool().unwrap_or(true) {
        "?"
    } else {
        ""
    };
    format!("{name}:{}{null}", yaml_type(c))
}

fn yaml_type(c: &Value) -> String {
    match c["type"].as_str().unwrap() {
        "list" => format!("list<{}>", yaml_type(&c["item"])),
        "struct" => {
            let fields: Vec<String> = c["fields"]
                .as_sequence()
                .unwrap()
                .iter()
                .map(yaml_column)
                .collect();
            format!("struct<{}>", fields.join(","))
        }
        t => t.to_string(),
    }
}

fn contract(table: &str) -> Vec<String> {
    let path = contracts().join(format!("{table}.parquet.yaml"));
    let doc: Value = serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    ["join_columns", "columns"]
        .iter()
        .filter_map(|k| doc[*k].as_sequence())
        .flatten()
        .map(yaml_column)
        .collect()
}

fn arrow_column(f: &Field) -> String {
    let null = if f.is_nullable() { "?" } else { "" };
    format!("{}:{}{null}", f.name(), arrow_type(f.data_type()))
}

fn arrow_type(t: &DataType) -> String {
    match t {
        DataType::Utf8 => "utf8".into(),
        DataType::Boolean => "bool".into(),
        DataType::UInt8 => "uint8".into(),
        DataType::UInt32 => "uint32".into(),
        DataType::UInt64 => "uint64".into(),
        DataType::Int32 => "int32".into(),
        DataType::Int64 => "int64".into(),
        DataType::Float32 => "float32".into(),
        DataType::Float64 => "float64".into(),
        DataType::Dictionary(_, v) => arrow_type(v),
        // the list's item field is the builder's ("item", nullable): the
        // contract describes only the item's type
        DataType::List(item) => format!("list<{}>", arrow_type(item.data_type())),
        DataType::Struct(fields) => {
            let f: Vec<String> = fields.iter().map(|f| arrow_column(f)).collect();
            format!("struct<{}>", f.join(","))
        }
        other => format!("{other:?}"),
    }
}

fn written(path: &Path) -> Vec<String> {
    let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(path).unwrap()).unwrap();
    reader
        .schema()
        .fields()
        .iter()
        .map(|f| arrow_column(f))
        .collect()
}

fn aux_ctx() -> AuxContext {
    AuxContext {
        device_id: "dev_1".into(),
        session_id: "ses_1".into(),
        test_id: "tst_1".into(),
        node: "mobile".into(),
    }
}

/// One file per table the core writes, as the logger writes it.
fn write_all(dir: &Path) -> Vec<(&'static str, PathBuf)> {
    let ctx = aux_ctx();
    let t = r#"{"mono_ns":1,"utc_ns":2}"#;
    let gps = serde_json::from_str(r#"{"mono_ns":1,"utc_ns":2,"latitude":59.3,"longitude":18.0}"#)
        .unwrap();
    let mut out = Vec::new();

    let p = dir.join("gps.parquet");
    aux_log::write_gps(&p, &ctx, &[gps]).unwrap();
    out.push(("gps", p));

    let p = dir.join("clock.parquet");
    aux_log::write_clock(&p, &ctx, &[serde_json::from_str(t).unwrap()]).unwrap();
    out.push(("clock", p));

    let p = dir.join("radio.parquet");
    aux_log::write_radio(&p, &ctx, &[serde_json::from_str(t).unwrap()]).unwrap();
    out.push(("radio", p));

    let p = dir.join("events.parquet");
    let ev = EventRow {
        mono_ns: 1,
        utc_ns: 2,
        event_type: "CLIENT_CONNECTED".into(),
        flow_id: None,
        event_metadata: None,
    };
    aux_log::write_events(&p, &ctx, &[ev]).unwrap();
    out.push(("events", p));

    let log_ctx = LogContext {
        device_id: ctx.device_id.clone(),
        session_id: ctx.session_id.clone(),
        test_id: ctx.test_id.clone(),
        node: ctx.node.clone(),
    };
    let mut w = SegmentedPacketWriter::new(dir, 1000, log_ctx).unwrap();
    let ts = Timestamp {
        mono_ns: 1,
        utc_ns: 2,
    };
    w.append(&PacketRow::new(
        1,
        0,
        Direction::Sent,
        ts,
        64,
        Protocol::Udp,
    ))
    .unwrap();
    w.flush().unwrap();
    drop(w);
    out.push(("packets", dir.join("packets_0000.parquet")));
    out
}

#[test]
fn written_files_match_the_parquet_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let files = write_all(tmp.path());
    let mut failures = Vec::new();
    for (table, path) in &files {
        let want = contract(table);
        let got = written(path);
        if want != got {
            failures.push(format!(
                "{table}:\n  contract: {want:?}\n  written:  {got:?}"
            ));
        }
    }
    // every contract table is either written above or listed as not written
    for entry in std::fs::read_dir(contracts()).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if let Some(table) = name.strip_suffix(".parquet.yaml") {
            if !files.iter().any(|(t, _)| *t == table) && !NOT_WRITTEN.contains(&table) {
                failures.push(format!(
                    "{table}: in contracts/parquet but no writer checked"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

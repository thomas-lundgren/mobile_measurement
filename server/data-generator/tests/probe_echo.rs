//! data-generator probe-echo integration test: prepare a generator, act as the
//! UE (send probe packets to its port), confirm echoes + counters + STOPPED +
//! server packets.parquet written and readable.

use std::net::UdpSocket;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use fieldtest_core::packet::ProbePacket;
use fieldtest_core::timestamp::Timestamp;
use mp_data_generator::{build_router, AppState};
use serde_json::json;
use tower::ServiceExt;

async fn send(
    state: &AppState,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let req = match body {
        Some(v) => Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap(),
    };
    let resp = build_router(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let val = serde_json::from_slice(&bytes).unwrap_or(json!(null));
    (status, val)
}

fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn probe_echo_end_to_end() {
    let st = AppState::default();
    let dir = tempfile::tempdir().unwrap();
    let log_dir = dir.path().join("server");
    let port = free_port();

    // prepare a probe generator
    let (s, body) = send(
        &st,
        "POST",
        "/internal/generators",
        Some(json!({
            "test_id": "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5",
            "device_id": "dev_01HZ8PQRS0000000000000001",
            "session_id": "ses_2026_05_23_001",
            "flow": { "flow_id": 1, "type": "probe", "rate_pps": 10 },
            "server_port": port,
            "node_log_dir": log_dir.to_str().unwrap()
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "prepare: {body}");
    let gen_id = body["generator_id"].as_str().unwrap().to_string();

    // act as the UE: send 20 probes, expect echoes
    let ue = UdpSocket::bind("127.0.0.1:0").unwrap();
    ue.set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let server = format!("127.0.0.1:{port}");
    let mut echoes = 0;
    for seq in 1..=20u64 {
        let pkt = ProbePacket {
            flow_id: 1,
            sequence_number: seq,
            send_mono_ns: Timestamp::now().mono_ns,
        };
        ue.send_to(&pkt.to_bytes(), &server).unwrap();
        let mut buf = [0u8; 2048];
        if let Ok((n, _)) = ue.recv_from(&mut buf) {
            if ProbePacket::decode(&buf[..n]).map(|p| p.sequence_number) == Ok(seq) {
                echoes += 1;
            }
        }
    }
    assert!(echoes >= 18, "expected most echoes back, got {echoes}/20");

    // status shows RUNNING + counters
    let (s, status) = send(&st, "GET", &format!("/internal/generators/{gen_id}"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(status["state"], "RUNNING");
    assert!(status["counters"]["packets_received"].as_u64().unwrap() >= 18);
    assert!(status["counters"]["packets_sent"].as_u64().unwrap() >= 18);
    assert_eq!(status["counters"]["dropped_rows"], 0);

    // stop → STOPPED
    let (s, status) = send(
        &st,
        "POST",
        &format!("/internal/generators/{gen_id}/stop"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(status["state"], "STOPPED");

    // server packets.parquet written and non-empty
    let files: Vec<_> = std::fs::read_dir(&log_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "parquet").unwrap_or(false))
        .collect();
    assert!(
        !files.is_empty(),
        "expected server packets.parquet segments"
    );
    // the server's RECEIVED rows carry the kernel receive time
    let mut stamped = 0;
    for f in files.iter().filter(|f| f.file_name().unwrap().to_string_lossy().starts_with("packets")) {
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
            std::fs::File::open(f).unwrap(),
        )
        .unwrap()
        .build()
        .unwrap();
        for b in reader {
            let b = b.unwrap();
            let dir = b.column_by_name("direction").unwrap().as_any()
                .downcast_ref::<arrow::array::StringArray>().unwrap().clone();
            let k = b.column_by_name("kernel_rx_utc_ns").unwrap().as_any()
                .downcast_ref::<arrow::array::UInt64Array>().unwrap().clone();
            for i in 0..b.num_rows() {
                if dir.value(i) == "RECEIVED" {
                    assert!(arrow::array::Array::is_valid(&k, i), "RECEIVED row without kernel time");
                    stamped += 1;
                }
            }
        }
    }
    assert!(stamped >= 18, "{stamped} stamped RECEIVED rows");

    // CLIENT_CONNECTED records the UE's address as the server saw it.
    let events = std::fs::read_dir(&log_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("events_f")
        })
        .expect("events_f<flow>.parquet written at stop");
    let mut reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
        std::fs::File::open(events).unwrap(),
    )
    .unwrap()
    .build()
    .unwrap();
    let batch = reader.next().unwrap().unwrap();
    let col = |name: &str| {
        batch
            .column_by_name(name)
            .unwrap()
            .as_any()
            .downcast_ref::<arrow::array::StringArray>()
            .unwrap()
            .value(0)
            .to_string()
    };
    assert_eq!(col("event_type"), "CLIENT_CONNECTED");
    let meta: serde_json::Value = serde_json::from_str(&col("event_metadata")).unwrap();
    assert_eq!(meta["peer_ip"], "127.0.0.1");
    assert_eq!(meta["protocol"], "UDP");
}

#[tokio::test]
async fn unknown_generator_404() {
    let st = AppState::default();
    let (s, _) = send(&st, "GET", "/internal/generators/gen_missing", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

//! test-control ↔ data-generator coordination, end to end: a real
//! data-generator server is started; test-control prepares a probe test against
//! it; we act as the UE to the allocated port; then stop and confirm teardown.

use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use fieldtest_core::packet::ProbePacket;
use fieldtest_core::timestamp::Timestamp;
use mp_test_control::{build_router, AppState, Config};
use serde_json::json;
use tower::ServiceExt;

async fn call(
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!(null)),
    )
}

/// Start a real data-generator on an ephemeral loopback port.
async fn spawn_generator() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = mp_data_generator::build_router(mp_data_generator::AppState::default());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn coordinate_probe_test() {
    let gen_url = spawn_generator().await;
    let logs = tempfile::tempdir().unwrap();
    let st = AppState::new(Config {
        generator_base_url: gen_url,
        log_root: logs.path().to_path_buf(),
        port_base: 45000,
        port_count: 50,
    });

    // start a probe-only test
    let (s, _) = call(
        &st,
        "POST",
        "/internal/tests",
        Some(json!({
            "test_id": "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5",
            "device_id": "dev_01HZ8PQRS0000000000000001",
            "session_id": "ses_2026_05_23_001",
            "server_host": "127.0.0.1",
            "plan": { "name": "probe_only_v1", "version": 1, "duration_sec": 60,
                      "flows": [{ "flow_id": 1, "type": "probe", "rate_pps": 10 }] }
        })),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);

    // poll until WAITING_FOR_CLIENT with a binding
    let mut port = 0u16;
    for _ in 0..50 {
        let (_, body) = call(
            &st,
            "GET",
            "/internal/tests/tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5",
            None,
        )
        .await;
        if body["state"] == "WAITING_FOR_CLIENT" {
            port = body["flow_bindings"][0]["server_port"].as_u64().unwrap() as u16;
            break;
        }
        if body["state"] == "FAILED" {
            panic!("test FAILED: {}", body["error"]);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        (45000..45050).contains(&port),
        "expected an allocated binding, got {port}"
    );

    // act as the UE: probe the allocated port, expect echoes
    let ue = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    ue.set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let server = format!("127.0.0.1:{port}");
    let mut echoes = 0;
    for seq in 1..=10u64 {
        let pkt = ProbePacket {
            flow_id: 1,
            sequence_number: seq,
            send_mono_ns: Timestamp::now().mono_ns,
        };
        ue.send_to(&pkt.to_bytes(), &server).unwrap();
        let mut buf = [0u8; 2048];
        if ue.recv_from(&mut buf).is_ok() {
            echoes += 1;
        }
    }
    assert!(
        echoes >= 8,
        "expected echoes via the coordinated generator, got {echoes}"
    );

    // status now reflects RUNNING (generator saw the client)
    let (_, body) = call(
        &st,
        "GET",
        "/internal/tests/tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5",
        None,
    )
    .await;
    assert_eq!(body["state"], "RUNNING");

    // stop → DONE, COMPLETED
    let (s, body) = call(
        &st,
        "POST",
        "/internal/tests/tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5/stop",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["state"], "DONE");
    assert_eq!(body["result"], "COMPLETED");
}

#[tokio::test]
async fn unknown_test_404() {
    let st = AppState::new(Config {
        generator_base_url: "http://127.0.0.1:1".into(),
        log_root: std::env::temp_dir(),
        port_base: 46000,
        port_count: 10,
    });
    let (s, _) = call(&st, "GET", "/internal/tests/tst_nope", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

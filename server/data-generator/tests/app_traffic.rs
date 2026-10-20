//! app_traffic end to end on localhost: the core UE agents against the
//! generator's object server, both nodes' logs checked per object.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use fieldtest_core::app_traffic::{self, AppAction, AppTraffic, Tally};
use fieldtest_core::log_writer::{packet_channel, Direction};
use mp_data_generator::{build_router, AppState};
use serde_json::json;
use tower::ServiceExt;

async fn send(st: &AppState, method: &str, path: &str, body: Option<serde_json::Value>) -> serde_json::Value {
    let b = Request::builder().method(method).uri(path).header("content-type", "application/json");
    let req = b.body(body.map(|v| Body::from(v.to_string())).unwrap_or_else(Body::empty)).unwrap();
    let resp = build_router(st.clone()).oneshot(req).await.unwrap();
    assert!(resp.status() == StatusCode::OK || resp.status() == StatusCode::NO_CONTENT, "{path}: {}", resp.status());
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(json!(null))
}

#[derive(Default)]
struct Bytes {
    sent: AtomicU64,
    received: AtomicU64,
}

impl Tally for Bytes {
    fn sent(&self, n: u64) {
        self.sent.fetch_add(n, Ordering::Relaxed);
    }
    fn received(&self, n: u64) {
        self.received.fetch_add(n, Ordering::Relaxed);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn scripted_user_downloads_and_uploads_objects() {
    let st = AppState::default();
    let dir = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let prep = send(&st, "POST", "/internal/generators", Some(json!({
        "test_id": "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5", "device_id": "dev_x", "session_id": "ses_x",
        "flow": {"flow_id": 7, "type": "app_traffic", "profile": "script"},
        "server_port": port, "node_log_dir": dir.path().to_str().unwrap()
    }))).await;
    let gid = prep["generator_id"].as_str().unwrap().to_string();

    // a page (HTML + 12 objects over 6 connections), then a photo upload
    let act = |at_ms, kind: &str, bytes, count, parallel, activity: &str| AppAction {
        at_ms, kind: kind.into(), bytes, count: Some(count), parallel: Some(parallel), activity: Some(activity.into()),
    };
    let cfg = AppTraffic {
        server: format!("127.0.0.1:{port}"),
        flow_id: 7,
        profile: "script".into(),
        workers: 1,
        seed: 1,
        scale: 1.0,
        actions: vec![
            act(0, "down", 50_000, 1, 1, "web_page"),
            act(0, "down", 30_000, 12, 6, "web_object"),
            act(300, "up", 400_000, 1, 1, "photo_upload"),
        ],
    };
    let (sink, mut rx, _) = packet_channel(1 << 16);
    let stop = AtomicBool::new(false);
    let tally = Bytes::default();
    std::thread::scope(|s| {
        s.spawn(|| app_traffic::run(&cfg, sink, &tally, &stop));
        let t0 = std::time::Instant::now();
        while tally.sent.load(Ordering::Relaxed) < 400_000 + 14 * 24 || tally.received.load(Ordering::Relaxed) < 50_000 + 12 * 30_000 + 24 {
            assert!(t0.elapsed() < Duration::from_secs(10), "transfers did not finish");
            std::thread::sleep(Duration::from_millis(20));
        }
        stop.store(true, Ordering::Release);
    });

    // UE rows: per object a request, then its data
    let mut rows = vec![];
    while let Ok(r) = rx.pop() {
        rows.push(r);
    }
    let objects: std::collections::BTreeSet<u64> = rows.iter().map(|r| r.sequence_number).collect();
    assert_eq!(objects.len(), 14);
    for oid in &objects {
        let of: Vec<_> = rows.iter().filter(|r| r.sequence_number == *oid).collect();
        assert_eq!(of[0].direction, Direction::Sent, "object starts with its request");
        assert_eq!(of[0].payload_size, 24);
        let act = of[0].timing.app_activity.unwrap();
        let size = of[0].timing.app_object_bytes.unwrap();
        let data: u64 = of[1..].iter().filter(|r| r.payload_size != 24 || size == 24).map(|r| r.payload_size as u64).sum();
        if act == "photo_upload" {
            assert_eq!(data, 400_000);
            assert_eq!(of.last().unwrap().direction, Direction::Received, "upload ends with the server's ack");
        } else {
            assert_eq!(data, size, "{act} bytes");
        }
    }

    let st_body = send(&st, "GET", &format!("/internal/generators/{gid}"), None).await;
    assert_eq!(st_body["state"], "RUNNING");
    assert!(st_body["counters"]["bytes_sent"].as_u64().unwrap() >= 50_000 + 12 * 30_000);
    assert!(st_body["counters"]["bytes_received"].as_u64().unwrap() >= 400_000);
    send(&st, "POST", &format!("/internal/generators/{gid}/stop"), None).await;
}

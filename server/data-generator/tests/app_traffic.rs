//! app_traffic end to end on localhost: the core UE agents against the
//! generator's object server, both nodes' logs checked per object.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use fieldtest_core::app_traffic::{self, AppAction, AppLog, AppTraffic};
use fieldtest_core::aux_log::AuxContext;
use fieldtest_core::endpoint::Counters;
use fieldtest_core::log_writer::{packet_channel, Direction};
use mp_data_generator::{build_router, AppState};
use serde_json::json;
use tower::ServiceExt;

async fn send(
    st: &AppState,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> serde_json::Value {
    let b = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    let req = b
        .body(
            body.map(|v| Body::from(v.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    let resp = build_router(st.clone()).oneshot(req).await.unwrap();
    assert!(
        resp.status() == StatusCode::OK || resp.status() == StatusCode::NO_CONTENT,
        "{path}: {}",
        resp.status()
    );
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(json!(null))
}

#[tokio::test(flavor = "multi_thread")]
async fn scripted_user_downloads_and_uploads_objects() {
    let st = AppState::default();
    let dir = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let prep = send(&st, "POST", "/internal/generators", Some(json!({
        "test_id": "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5", "device_id": "dev_x", "session_id": "ses_x",
        "flow": {"flow_id": 7, "type": "app_traffic", "profile": "script"},
        "server_port": port, "node_log_dir": dir.path().to_str().unwrap()
    }))).await;
    let gid = prep["generator_id"].as_str().unwrap().to_string();

    // a page (HTML + 12 objects over 6 connections), then a photo upload
    let act = |at_ms, kind: &str, bytes, count, parallel, activity: &str| AppAction {
        at_ms,
        kind: kind.into(),
        bytes,
        count: Some(count),
        parallel: Some(parallel),
        activity: Some(activity.into()),
        ..AppAction::default()
    };
    // an LLM call: 20 kB context up, 300 ms to first token, 6 kB streamed at
    // 48 kbit/s in 140-B events (~1 s)
    let llm = AppAction {
        at_ms: 400,
        kind: "down".into(),
        bytes: 6_000,
        up_bytes: Some(20_000),
        delay_ms: Some(300),
        rate_kbps: Some(48),
        chunk_bytes: Some(140),
        activity: Some("llm_call".into()),
        ..AppAction::default()
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
            llm,
        ],
        placement: app_traffic::Placement::Device,
        log: Some(app_log(dir.path())),
    };
    let (sink, mut rx, _) = packet_channel(1 << 16);
    let stop = AtomicBool::new(false);
    let tally = Counters::default();
    std::thread::scope(|s| {
        s.spawn(|| app_traffic::run(&cfg, sink, &tally, &stop));
        let t0 = std::time::Instant::now();
        while tally.bytes_sent.load(Ordering::Relaxed) < 420_000 + 15 * 44
            || tally.bytes_received.load(Ordering::Relaxed) < 50_000 + 12 * 30_000 + 44 + 6_000
        {
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "transfers did not finish"
            );
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
    assert_eq!(objects.len(), 15);
    for oid in &objects {
        let of: Vec<_> = rows.iter().filter(|r| r.sequence_number == *oid).collect();
        assert_eq!(
            of[0].direction,
            Direction::Sent,
            "object starts with its request"
        );
        assert_eq!(of[0].payload_size, 44);
        let act = of[0].timing.app_activity.unwrap();
        let size = of[0].timing.app_object_bytes.unwrap();
        let up: u64 = of[1..]
            .iter()
            .filter(|r| r.direction == Direction::Sent)
            .map(|r| r.payload_size as u64)
            .sum();
        let down: u64 = of[1..]
            .iter()
            .filter(|r| r.direction == Direction::Received)
            .map(|r| r.payload_size as u64)
            .sum();
        assert_eq!(up, of[0].timing.app_up_bytes.unwrap(), "{act} up bytes");
        if act == "photo_upload" {
            assert_eq!(
                (up, down, size),
                (400_000, 44, 0),
                "upload, then the server's ack"
            );
        } else {
            assert_eq!(down, size, "{act} bytes");
        }
        if act == "llm_call" {
            let got: Vec<_> = of
                .iter()
                .filter(|r| r.direction == Direction::Received)
                .collect();
            let t = |r: &&fieldtest_core::log_writer::PacketRow| r.timestamp.utc_ns as f64 / 1e6;
            let last_sent = of
                .iter()
                .filter(|r| r.direction == Direction::Sent)
                .map(t)
                .fold(0.0, f64::max);
            let (first, last) = (t(got[0]), t(got.last().unwrap()));
            assert!(
                first - last_sent >= 290.0,
                "time to first token {}",
                first - last_sent
            );
            assert!(
                (800.0..2000.0).contains(&(last - first)),
                "stream length {} ms",
                last - first
            );
            assert!(
                got.len() >= 20,
                "streamed in events, got {} reads",
                got.len()
            );
        }
    }

    // the intended-operations log: one row per object, each script action a
    // task of its own, every object ok with the bytes the plan asked for
    let ev = app_events(&dir.path().join("app_events_0007.parquet"));
    assert_eq!(ev.len(), 15);
    let tasks: [(u64, &str, usize); 4] = [
        (1, "web_page", 1),
        (2, "web_object", 12),
        (3, "photo_upload", 1),
        (4, "llm_call", 1),
    ];
    for (task, act, n) in tasks {
        let of: Vec<_> = ev.iter().filter(|r| r.u("task") == task).collect();
        assert_eq!(of.len(), n, "task {task}");
        for r in of {
            assert_eq!(
                (r.s("activity"), r.s("step_kind"), r.u("step")),
                (act, "script_action", 1)
            );
            assert_eq!(r.s("outcome"), "ok", "{act}");
            assert_eq!(r.u("bytes_up"), r.u("plan_up_bytes") + 44, "{act} up");
            let down = if r.u("plan_down_bytes") == 0 {
                44
            } else {
                r.u("plan_down_bytes")
            };
            assert_eq!(r.u("bytes_down"), down, "{act} down");
            assert!(r.u("planned_mono_ns") <= r.u("start_mono_ns"));
            assert!(r.u("start_mono_ns") <= r.u("first_byte_mono_ns"));
            assert!(r.u("first_byte_mono_ns") <= r.u("done_mono_ns"));
            if act == "llm_call" {
                // the server's 300 ms think time sits between start and first byte
                let ttfb = (r.u("first_byte_mono_ns") - r.u("start_mono_ns")) as f64 / 1e6;
                assert!(ttfb >= 290.0, "ttfb {ttfb} ms");
            }
        }
    }
    // the bursts in order, each waiting for the one before (closed loop)
    let mut eps: Vec<(u64, Option<u64>)> = ev
        .iter()
        .map(|r| (r.u("episode"), r.opt("after_episode")))
        .collect();
    eps.dedup();
    assert_eq!(eps, [(1, None), (2, Some(1)), (3, Some(2)), (4, Some(3))]);
    // 12 objects over 6 connections: the first burst opened one, the second more
    assert!(ev.iter().filter(|r| r.b("new_connection")).count() >= 2);

    let st_body = send(&st, "GET", &format!("/internal/generators/{gid}"), None).await;
    assert_eq!(st_body["state"], "RUNNING");
    assert!(st_body["counters"]["bytes_sent"].as_u64().unwrap() >= 50_000 + 12 * 30_000);
    assert!(st_body["counters"]["bytes_received"].as_u64().unwrap() >= 400_000);
    send(
        &st,
        "POST",
        &format!("/internal/generators/{gid}/stop"),
        None,
    )
    .await;
}

fn app_log(dir: &std::path::Path) -> AppLog {
    AppLog {
        dir: dir.to_path_buf(),
        ctx: AuxContext {
            device_id: "dev_x".into(),
            session_id: "ses_x".into(),
            test_id: "tst_x".into(),
            node: "mobile".into(),
        },
    }
}

/// One app_events row, read back by column name.
struct Ev(std::collections::HashMap<String, serde_json::Value>);

impl Ev {
    fn u(&self, k: &str) -> u64 {
        self.0[k]
            .as_u64()
            .unwrap_or_else(|| panic!("{k} = {}", self.0[k]))
    }
    fn opt(&self, k: &str) -> Option<u64> {
        self.0[k].as_u64()
    }
    fn s(&self, k: &str) -> &str {
        self.0[k].as_str().unwrap()
    }
    fn b(&self, k: &str) -> bool {
        self.0[k].as_bool().unwrap_or(false)
    }
}

fn app_events(path: &std::path::Path) -> Vec<Ev> {
    use arrow::array::{Array, BooleanArray, ListArray, StringArray, UInt32Array, UInt64Array};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    let reader = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(path).unwrap())
        .unwrap()
        .build()
        .unwrap();
    let mut out = vec![];
    for batch in reader {
        let batch = batch.unwrap();
        for i in 0..batch.num_rows() {
            let mut m = std::collections::HashMap::new();
            for (f, col) in batch.schema().fields().iter().zip(batch.columns()) {
                let v = if col.is_null(i) {
                    json!(null)
                } else if let Some(a) = col.as_any().downcast_ref::<UInt64Array>() {
                    json!(a.value(i))
                } else if let Some(a) = col.as_any().downcast_ref::<UInt32Array>() {
                    json!(a.value(i))
                } else if let Some(a) = col.as_any().downcast_ref::<StringArray>() {
                    json!(a.value(i))
                } else if let Some(a) = col.as_any().downcast_ref::<BooleanArray>() {
                    json!(a.value(i))
                } else if let Some(a) = col.as_any().downcast_ref::<ListArray>() {
                    let v = a.value(i);
                    let v = v.as_any().downcast_ref::<UInt64Array>().unwrap();
                    json!(v.values().to_vec())
                } else {
                    json!(null)
                };
                m.insert(f.name().clone(), v);
            }
            out.push(Ev(m));
        }
    }
    out
}

/// A websearch agent's first task runs as a graph against the object
/// server: plan → search → parallel fetches → read, in dependency order,
/// each node logged with the object ids it waited for.
#[tokio::test(flavor = "multi_thread")]
async fn websearch_task_runs_as_a_graph() {
    let st = AppState::default();
    let dir = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let prep = send(&st, "POST", "/internal/generators", Some(json!({
        "test_id": "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X8", "device_id": "dev_x", "session_id": "ses_x",
        "flow": {"flow_id": 5, "type": "app_traffic", "profile": "websearch"},
        "server_port": port, "node_log_dir": dir.path().to_str().unwrap()
    }))).await;
    let gid = prep["generator_id"].as_str().unwrap().to_string();
    let cfg = AppTraffic {
        server: format!("127.0.0.1:{port}"),
        flow_id: 5,
        profile: "websearch".into(),
        workers: 1,
        seed: 2,
        scale: 0.2,
        actions: vec![],
        placement: app_traffic::Placement::Device,
        log: Some(app_log(dir.path())),
    };
    let (sink, _rx, _) = packet_channel(1 << 18);
    let stop = AtomicBool::new(false);
    let tally = Counters::default();
    std::thread::scope(|s| {
        s.spawn(|| app_traffic::run(&cfg, sink, &tally, &stop));
        // the first task ends when the user starts reading (>= 5 s idle)
        let t0 = std::time::Instant::now();
        let (mut last, mut since) = (0, std::time::Instant::now());
        loop {
            let moved = tally.bytes_received.load(Ordering::Relaxed)
                + tally.bytes_sent.load(Ordering::Relaxed);
            if moved != last {
                (last, since) = (moved, std::time::Instant::now());
            } else if last > 0 && since.elapsed() > Duration::from_millis(4500) {
                break;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(60),
                "the task did not finish"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        stop.store(true, Ordering::Release);
    });
    let ev = app_events(&dir.path().join("app_events_0005.parquet"));
    let task: Vec<&Ev> = ev.iter().filter(|r| r.u("task") == 1).collect();
    assert!(
        task.iter().all(|r| r.s("outcome") == "ok"),
        "every node of the task ok"
    );
    let acts: Vec<&str> = task.iter().map(|r| r.s("activity")).collect();
    assert_eq!((acts[0], acts[1]), ("llm_call", "web_search"));
    // the first round: search (step 2), its fetches (3), the read (4)
    let fetches: Vec<&&Ev> = task.iter().filter(|r| r.u("step") == 3).collect();
    assert!(fetches.len() >= 2 && fetches.iter().all(|r| r.s("activity") == "web_fetch"));
    let search = task[1].u("object_id");
    let read = task.iter().find(|r| r.u("step") == 4).unwrap();
    assert_eq!(read.s("activity"), "llm_call");
    for f in &fetches {
        assert_eq!(f.0["after_objects"], json!([search]));
        assert!(f.u("start_mono_ns") >= task[1].u("done_mono_ns"));
        assert!(
            read.u("start_mono_ns") >= f.u("done_mono_ns"),
            "the read waits for every fetch"
        );
    }
    send(
        &st,
        "POST",
        &format!("/internal/generators/{gid}/stop"),
        None,
    )
    .await;
}

/// A test that ends mid-object logs it as `stopped`: started, never done.
#[tokio::test(flavor = "multi_thread")]
async fn stopping_mid_object_logs_stopped() {
    let st = AppState::default();
    let dir = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let prep = send(&st, "POST", "/internal/generators", Some(json!({
        "test_id": "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X7", "device_id": "dev_x", "session_id": "ses_x",
        "flow": {"flow_id": 3, "type": "app_traffic", "profile": "script"},
        "server_port": port, "node_log_dir": dir.path().to_str().unwrap()
    }))).await;
    let gid = prep["generator_id"].as_str().unwrap().to_string();
    // 50 kB streamed at 8 kbit/s: 50 s, far longer than the test
    let cfg = AppTraffic {
        server: format!("127.0.0.1:{port}"),
        flow_id: 3,
        profile: "script".into(),
        workers: 1,
        seed: 1,
        scale: 1.0,
        actions: vec![AppAction {
            kind: "down".into(),
            bytes: 50_000,
            rate_kbps: Some(8),
            activity: Some("llm_call".into()),
            ..AppAction::default()
        }],
        placement: app_traffic::Placement::Device,
        log: Some(app_log(dir.path())),
    };
    let (sink, _rx, _) = packet_channel(1 << 16);
    let stop = AtomicBool::new(false);
    let tally = Counters::default();
    std::thread::scope(|s| {
        s.spawn(|| app_traffic::run(&cfg, sink, &tally, &stop));
        std::thread::sleep(Duration::from_millis(1500));
        stop.store(true, Ordering::Release);
    });
    let ev = app_events(&dir.path().join("app_events_0003.parquet"));
    assert_eq!(ev.len(), 1);
    let r = &ev[0];
    assert_eq!(r.s("outcome"), "stopped");
    assert!(r.opt("start_mono_ns").is_some() && r.opt("done_mono_ns").is_none());
    assert!(r.u("bytes_down") > 0 && r.u("bytes_down") < 50_000);
    send(
        &st,
        "POST",
        &format!("/internal/generators/{gid}/stop"),
        None,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn v1_requests_still_served() {
    use fieldtest_core::packet::AppRequest;
    use std::io::{Read, Write};
    let st = AppState::default();
    let dir = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let prep = send(&st, "POST", "/internal/generators", Some(json!({
        "test_id": "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X6", "device_id": "dev_x", "session_id": "ses_x",
        "flow": {"flow_id": 7, "type": "app_traffic", "profile": "web"},
        "server_port": port, "node_log_dir": dir.path().to_str().unwrap()
    }))).await;
    let gid = prep["generator_id"].as_str().unwrap().to_string();
    let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let down = AppRequest {
        kind: AppRequest::DOWNLOAD,
        activity: 1,
        flow_id: 7,
        object_id: 1,
        size: 5000,
    };
    c.write_all(&down.to_bytes()).unwrap();
    let mut buf = vec![0u8; 5000];
    c.read_exact(&mut buf).unwrap();
    let up = AppRequest {
        kind: AppRequest::UPLOAD,
        activity: 8,
        flow_id: 7,
        object_id: 2,
        size: 3000,
    };
    c.write_all(&up.to_bytes()).unwrap();
    c.write_all(&[0u8; 3000]).unwrap();
    let mut ack = [0u8; AppRequest::WIRE_LEN];
    c.read_exact(&mut ack).unwrap();
    assert_eq!(ack, up.to_bytes());
    send(
        &st,
        "POST",
        &format!("/internal/generators/{gid}/stop"),
        None,
    )
    .await;
}

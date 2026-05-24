//! RustCore handle API — the pure logic behind the JNI shims (no JVM needed).
//! Drives a probe test against a raw echo peer the test plays.

use std::net::UdpSocket;
use std::thread;
use std::time::Duration;

use fieldtest_core::RustCore;
use serde_json::json;

#[test]
fn start_poll_stop_sealed_cycle() {
    // echo peer
    let port = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let sock = UdpSocket::bind(("127.0.0.1", port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let peer = thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if let Ok((n, from)) = sock.recv_from(&mut buf) {
                let _ = sock.send_to(&buf[..n], from);
            }
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let config = json!({
        "test_id": "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5",
        "device_id": "dev_x",
        "session_id": "ses_x",
        "server_host": "127.0.0.1",
        "log_dir": dir.path().to_str().unwrap(),
        "flows": [ { "flow_id": 1, "type": "probe", "server_port": port, "rate_pps": 100 } ]
    })
    .to_string();

    let core = RustCore::new();
    core.start_test(&config).expect("start_test");
    thread::sleep(Duration::from_millis(300));

    // poll_status is a non-empty JSON array with counters
    let status: serde_json::Value = serde_json::from_str(&core.poll_status()).unwrap();
    assert!(status.as_array().unwrap().len() == 1, "one flow");
    assert!(status[0]["packets_sent"].as_u64().unwrap() >= 5);

    // stop returns summaries; sealed_files lists the written parquet
    let summary: serde_json::Value = serde_json::from_str(&core.stop_test()).unwrap();
    assert!(
        summary[0]["packets_received"].as_u64().unwrap() >= 3,
        "echoes: {summary}"
    );
    let files: Vec<String> = serde_json::from_str(&core.sealed_files()).unwrap();
    assert!(
        files.iter().any(|f| f.ends_with(".parquet")),
        "sealed parquet: {files:?}"
    );

    peer.join().unwrap();
}

#[test]
fn aux_gps_clock_sealed() {
    let port = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let sock = UdpSocket::bind(("127.0.0.1", port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let peer = thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            if let Ok((n, from)) = sock.recv_from(&mut buf) {
                let _ = sock.send_to(&buf[..n], from);
            }
        }
    });

    // log_dir is the test's mobile/ dir so gps/radio land at the test root
    let root = tempfile::tempdir().unwrap();
    let mobile = root.path().join("mobile");
    let config = json!({
        "test_id": "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5", "device_id": "dev_x", "session_id": "ses_x",
        "server_host": "127.0.0.1", "log_dir": mobile.to_str().unwrap(),
        "flows": [ { "flow_id": 1, "type": "probe", "server_port": port, "rate_pps": 50 } ]
    })
    .to_string();

    let core = RustCore::new();
    core.start_test(&config).unwrap();
    core.push_gps(
        &json!({"mono_ns":1,"utc_ns":2,"latitude":59.33,"longitude":18.06,"accuracy_m":5.0})
            .to_string(),
    )
    .unwrap();
    core.push_clock(
        &json!({"mono_ns":1,"utc_ns":2,"source":"gps","uncertainty_ns":400}).to_string(),
    )
    .unwrap();
    thread::sleep(Duration::from_millis(150));
    core.stop_test();
    peer.join().unwrap();

    let sealed: Vec<String> = serde_json::from_str(&core.sealed_files()).unwrap();
    assert!(
        sealed.iter().any(|f| f.ends_with("/gps.parquet")),
        "gps sealed: {sealed:?}"
    );
    assert!(
        sealed.iter().any(|f| f.ends_with("/mobile/clock.parquet")),
        "clock sealed: {sealed:?}"
    );
    assert!(
        sealed.iter().any(|f| f.contains("/mobile/packets")),
        "packets sealed: {sealed:?}"
    );
    assert!(root.path().join("gps.parquet").exists());
    assert!(mobile.join("clock.parquet").exists());
}

#[test]
fn idempotent_and_empty_when_idle() {
    let core = RustCore::new();
    assert_eq!(core.poll_status(), "[]");
    assert_eq!(core.stop_test(), "[]"); // safe with no test running
    assert_eq!(core.sealed_files(), "[]");
    assert!(core.start_test("not json").is_err());
}

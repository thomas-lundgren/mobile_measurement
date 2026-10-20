//! realtime UDP + bulk TCP generator integration tests. Act as the UE against a
//! prepared generator and assert the server's side of each flow.

use std::io::{Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use fieldtest_core::packet::RealtimePacket;
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!(null)),
    )
}

fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn prepare(
    st: &AppState,
    flow: serde_json::Value,
    port: u16,
    dir: &std::path::Path,
) -> String {
    let (s, body) = send(
        st,
        "POST",
        "/internal/generators",
        Some(json!({
            "test_id": "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5",
            "device_id": "dev_x", "session_id": "ses_x",
            "flow": flow, "server_port": port, "node_log_dir": dir.to_str().unwrap()
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "prepare: {body}");
    body["generator_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn realtime_to_server_logs_sender_timestamps() {
    let st = AppState::default();
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let gid = prepare(&st, json!({"flow_id":2,"type":"realtime_udp","direction":"ToServer","rate_pps":50,"packet_size_bytes":64}), port, dir.path()).await;

    // UE sends a realtime stream to the server
    let ue = UdpSocket::bind("127.0.0.1:0").unwrap();
    for seq in 0..30u64 {
        let ts = Timestamp::now();
        let pkt = RealtimePacket {
            flow_id: 2,
            sequence_number: seq,
            send_mono_ns: ts.mono_ns,
            send_utc_ns: ts.utc_ns,
        };
        ue.send_to(&pkt.to_bytes_padded(64), format!("127.0.0.1:{port}"))
            .unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
    std::thread::sleep(Duration::from_millis(100));

    let (_, body) = send(&st, "GET", &format!("/internal/generators/{gid}"), None).await;
    assert_eq!(body["state"], "RUNNING");
    assert!(
        body["counters"]["packets_received"].as_u64().unwrap() >= 25,
        "received: {body}"
    );

    let (_, stopped) = send(
        &st,
        "POST",
        &format!("/internal/generators/{gid}/stop"),
        None,
    )
    .await;
    assert_eq!(stopped["state"], "STOPPED");
}

#[tokio::test]
async fn realtime_from_server_streams_after_keepalive() {
    let st = AppState::default();
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let gid = prepare(&st, json!({"flow_id":3,"type":"realtime_udp","direction":"FromServer","rate_pps":200,"packet_size_bytes":80}), port, dir.path()).await;

    // UE sends one keepalive, then receives the downlink stream
    let ue = UdpSocket::bind("127.0.0.1:0").unwrap();
    ue.set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    ue.send_to(b"keepalive", format!("127.0.0.1:{port}"))
        .unwrap();

    let mut received = 0;
    let mut buf = [0u8; 2048];
    for _ in 0..20 {
        match ue.recv_from(&mut buf) {
            Ok((n, _)) => {
                if let Ok(p) = RealtimePacket::decode(&buf[..n]) {
                    assert_eq!(p.flow_id, 3);
                    received += 1;
                }
            }
            Err(_) => break,
        }
    }
    assert!(
        received >= 5,
        "downlink realtime packets received: {received}"
    );

    let (_, body) = send(&st, "GET", &format!("/internal/generators/{gid}"), None).await;
    assert_eq!(body["state"], "RUNNING");
    assert!(body["counters"]["packets_sent"].as_u64().unwrap() >= 5);
    send(
        &st,
        "POST",
        &format!("/internal/generators/{gid}/stop"),
        None,
    )
    .await;
}

#[tokio::test]
async fn realtime_from_server_follows_ue_to_new_address() {
    let st = AppState::default();
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let gid = prepare(&st, json!({"flow_id":3,"type":"realtime_udp","direction":"FromServer","rate_pps":200,"packet_size_bytes":80}), port, dir.path()).await;
    // datagrams arriving on `ue` within `ms`
    let count = |ue: &UdpSocket, ms: u64| {
        ue.set_nonblocking(true).unwrap();
        let mut buf = [0u8; 2048];
        let mut n = 0;
        let end = std::time::Instant::now() + Duration::from_millis(ms);
        while std::time::Instant::now() < end {
            match ue.recv_from(&mut buf) {
                Ok(_) => n += 1,
                Err(_) => std::thread::sleep(Duration::from_millis(1)),
            }
        }
        n
    };

    // the UE's first socket gets the stream ...
    let old = UdpSocket::bind("127.0.0.1:0").unwrap();
    old.send_to(b"keepalive", format!("127.0.0.1:{port}"))
        .unwrap();
    assert!(count(&old, 200) >= 10);

    // ... until it reopens (network switch) and keepalives come from a new port
    let new = UdpSocket::bind("127.0.0.1:0").unwrap();
    new.send_to(b"keepalive", format!("127.0.0.1:{port}"))
        .unwrap();
    count(&old, 100); // in flight before the switch was seen
    let on_new = count(&new, 300);
    assert!(on_new >= 20, "packets on the new address: {on_new}");
    assert_eq!(count(&old, 100), 0, "stream still going to the old address");

    send(
        &st,
        "POST",
        &format!("/internal/generators/{gid}/stop"),
        None,
    )
    .await;
}

#[tokio::test]
async fn bulk_from_server_streams_bytes() {
    let st = AppState::default();
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let gid = prepare(
        &st,
        json!({"flow_id":4,"type":"bulk_tcp","direction":"FromServer"}),
        port,
        dir.path(),
    )
    .await;

    // UE connects and reads a few hundred KB
    let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut total = 0usize;
    let mut buf = vec![0u8; 64 * 1024];
    while total < 256 * 1024 {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(_) => break,
        }
    }
    assert!(total >= 256 * 1024, "bulk bytes received: {total}");
    drop(stream);

    let (_, body) = send(&st, "GET", &format!("/internal/generators/{gid}"), None).await;
    assert!(body["counters"]["bytes_sent"].as_u64().unwrap() >= 256 * 1024);
    send(
        &st,
        "POST",
        &format!("/internal/generators/{gid}/stop"),
        None,
    )
    .await;
}

#[tokio::test]
async fn bulk_from_server_keeps_to_rate_cap() {
    let st = AppState::default();
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let gid = prepare(
        &st,
        json!({"flow_id":6,"type":"bulk_tcp","direction":"FromServer","rate_kbps":2000}),
        port,
        dir.path(),
    )
    .await;

    // 2 Mbit/s = 250 KB/s; read for 1 s
    let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut total = 0usize;
    let mut buf = vec![0u8; 64 * 1024];
    let t0 = std::time::Instant::now();
    while t0.elapsed() < Duration::from_secs(1) {
        if let Ok(n) = stream.read(&mut buf) {
            total += n;
        }
    }
    assert!(
        (200_000..=300_000).contains(&total),
        "bytes in 1 s at 2 Mbit/s: {total}"
    );
    drop(stream);
    send(
        &st,
        "POST",
        &format!("/internal/generators/{gid}/stop"),
        None,
    )
    .await;
}

#[tokio::test]
async fn bulk_to_server_drains_bytes() {
    let st = AppState::default();
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let gid = prepare(
        &st,
        json!({"flow_id":5,"type":"bulk_tcp","direction":"ToServer"}),
        port,
        dir.path(),
    )
    .await;

    // UE connects and uploads bytes
    let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    let chunk = vec![7u8; 64 * 1024];
    for _ in 0..4 {
        stream.write_all(&chunk).unwrap();
    }
    stream.flush().unwrap();
    std::thread::sleep(Duration::from_millis(150));

    let (_, body) = send(&st, "GET", &format!("/internal/generators/{gid}"), None).await;
    assert!(
        body["counters"]["bytes_received"].as_u64().unwrap() >= 256 * 1024,
        "received: {body}"
    );
    drop(stream);
    send(
        &st,
        "POST",
        &format!("/internal/generators/{gid}/stop"),
        None,
    )
    .await;
}

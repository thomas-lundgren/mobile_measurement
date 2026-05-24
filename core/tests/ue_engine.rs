//! UE-engine tests: the test itself plays the server peer (raw sockets) for
//! each flow type/direction, then asserts the engine's counters + that
//! mobile/packets.parquet was written. (The full server interop is 2b.)

use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::thread;
use std::time::Duration;

use fieldtest_core::packet::RealtimePacket;
use fieldtest_core::ue::{UeEngine, UeFlow, UeTestConfig};

fn udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn cfg(dir: &std::path::Path, flow: UeFlow) -> UeTestConfig {
    UeTestConfig {
        test_id: "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5".into(),
        device_id: "dev_x".into(),
        session_id: "ses_x".into(),
        server_host: "127.0.0.1".into(),
        log_dir: dir.to_str().unwrap().into(),
        segment_rows: 1000,
        flows: vec![flow],
    }
}

fn parquet_rows(dir: &std::path::Path) -> usize {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    let mut total = 0;
    for e in std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
        let p = e.path();
        if p.extension().map(|x| x == "parquet").unwrap_or(false) {
            let r = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(&p).unwrap())
                .unwrap()
                .build()
                .unwrap();
            for b in r {
                total += b.unwrap().num_rows();
            }
        }
    }
    total
}

#[test]
fn probe_against_echo_peer() {
    let port = udp_port();
    // echo peer
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
    let eng = UeEngine::start(cfg(
        dir.path(),
        UeFlow {
            flow_id: 1,
            flow_type: "probe".into(),
            direction: None,
            server_port: port,
            rate_pps: Some(100),
            packet_size_bytes: None,
            target_bytes: None,
        },
    ))
    .unwrap();
    thread::sleep(Duration::from_millis(300));
    let summ = eng.stop();
    peer.join().unwrap();

    assert!(summ[0].packets_sent >= 10, "sent {}", summ[0].packets_sent);
    assert!(
        summ[0].packets_received >= 8,
        "received {}",
        summ[0].packets_received
    );
    assert!(
        parquet_rows(dir.path()) >= 16,
        "rows: SENT + RECEIVED expected"
    );
}

#[test]
fn realtime_to_server_send() {
    let port = udp_port();
    let sock = UdpSocket::bind(("127.0.0.1", port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let counter = thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let mut got = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if let Ok((n, _)) = sock.recv_from(&mut buf) {
                if RealtimePacket::decode(&buf[..n]).is_ok() {
                    got += 1;
                }
            }
        }
        got
    });

    let dir = tempfile::tempdir().unwrap();
    let eng = UeEngine::start(cfg(
        dir.path(),
        UeFlow {
            flow_id: 2,
            flow_type: "realtime_udp".into(),
            direction: Some("ToServer".into()),
            server_port: port,
            rate_pps: Some(100),
            packet_size_bytes: Some(64),
            target_bytes: None,
        },
    ))
    .unwrap();
    thread::sleep(Duration::from_millis(300));
    let summ = eng.stop();
    let server_got = counter.join().unwrap();

    assert!(summ[0].packets_sent >= 10, "sent {}", summ[0].packets_sent);
    assert!(server_got >= 8, "server received {server_got}");
}

#[test]
fn realtime_from_server_recv() {
    let port = udp_port();
    // peer waits for the keepalive, then streams downlink RealtimePackets back
    let sock = UdpSocket::bind(("127.0.0.1", port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let peer = thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let from = loop {
            if let Ok((_, from)) = sock.recv_from(&mut buf) {
                break from;
            }
        };
        for seq in 0..40u64 {
            let ts = fieldtest_core::timestamp::Timestamp::now();
            let pkt = RealtimePacket {
                flow_id: 3,
                sequence_number: seq,
                send_mono_ns: ts.mono_ns,
                send_utc_ns: ts.utc_ns,
            };
            let _ = sock.send_to(&pkt.to_bytes_padded(80), from);
            thread::sleep(Duration::from_millis(5));
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let eng = UeEngine::start(cfg(
        dir.path(),
        UeFlow {
            flow_id: 3,
            flow_type: "realtime_udp".into(),
            direction: Some("FromServer".into()),
            server_port: port,
            rate_pps: Some(200),
            packet_size_bytes: Some(80),
            target_bytes: None,
        },
    ))
    .unwrap();
    thread::sleep(Duration::from_millis(400));
    let summ = eng.stop();
    peer.join().unwrap();

    assert!(
        summ[0].packets_received >= 10,
        "downlink received {}",
        summ[0].packets_received
    );
}

#[test]
fn bulk_to_server_upload() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let counter = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut total = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => total += n as u64,
                Err(_) => {}
            }
        }
        total
    });

    let dir = tempfile::tempdir().unwrap();
    let eng = UeEngine::start(cfg(
        dir.path(),
        UeFlow {
            flow_id: 4,
            flow_type: "bulk_tcp".into(),
            direction: Some("ToServer".into()),
            server_port: port,
            rate_pps: None,
            packet_size_bytes: None,
            target_bytes: None,
        },
    ))
    .unwrap();
    thread::sleep(Duration::from_millis(300));
    let summ = eng.stop();
    let server_bytes = counter.join().unwrap();

    assert!(
        summ[0].bytes_sent >= 256 * 1024,
        "uploaded {}",
        summ[0].bytes_sent
    );
    assert!(server_bytes >= 256 * 1024, "server got {server_bytes}");
}

#[test]
fn bulk_from_server_download() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let chunk = vec![9u8; 64 * 1024];
        for _ in 0..8 {
            if stream.write_all(&chunk).is_err() {
                break;
            }
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let eng = UeEngine::start(cfg(
        dir.path(),
        UeFlow {
            flow_id: 5,
            flow_type: "bulk_tcp".into(),
            direction: Some("FromServer".into()),
            server_port: port,
            rate_pps: None,
            packet_size_bytes: None,
            target_bytes: None,
        },
    ))
    .unwrap();
    thread::sleep(Duration::from_millis(300));
    let summ = eng.stop();
    peer.join().unwrap();

    assert!(
        summ[0].bytes_received >= 256 * 1024,
        "downloaded {}",
        summ[0].bytes_received
    );
}

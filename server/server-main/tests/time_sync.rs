//! UDP time responder wire-format test: nonce echoed, t2 <= t3, plausible UTC.

use std::time::Duration;

use mp_server_main::time_sync;
use tokio::net::UdpSocket;
use tokio::time::timeout;

fn be_i64(b: &[u8]) -> i64 {
    i64::from_be_bytes(b.try_into().unwrap())
}

#[tokio::test]
async fn udp_time_responder_roundtrip() {
    let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = server.local_addr().unwrap().port();
    tokio::spawn(time_sync::serve(server));

    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let nonce: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
    client.send_to(&nonce, ("127.0.0.1", port)).await.unwrap();

    let mut buf = [0u8; 24];
    let (n, _) = timeout(Duration::from_secs(2), client.recv_from(&mut buf))
        .await
        .expect("reply timed out")
        .unwrap();

    assert_eq!(n, 24, "reply is nonce(8)+t2(8)+t3(8)");
    assert_eq!(&buf[..8], &nonce, "nonce echoed");
    let t2 = be_i64(&buf[8..16]);
    let t3 = be_i64(&buf[16..24]);
    assert!(t2 <= t3, "recv stamp precedes send stamp");
    assert!(t2 > 1_700_000_000_000_000_000, "plausible UTC nanoseconds");
}

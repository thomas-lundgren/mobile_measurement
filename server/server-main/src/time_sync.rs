//! Low-overhead UDP time responder for device clock sync (NTP-style).
//!
//! A datagram exchange avoids TCP/HTTP overhead, so the round-trip — and hence
//! the OWD-offset uncertainty the UE records in `clock.parquet` — is much
//! tighter than the `GET /api/v1/time` fallback. The four timestamps
//! (t1 client-send, t2 server-recv, t3 server-send, t4 client-recv) let the UE
//! subtract the server's own turnaround (t3−t2), unlike the single-timestamp
//! HTTP form.
//!
//! Wire format (loopback/LAN, no auth — it reveals only the clock):
//!   request : client → server, ≥0 bytes; first ≤8 bytes are an opaque nonce.
//!   reply   : server → client, 24 bytes =
//!             nonce(8, zero-padded) ++ t2_recv_utc_ns(i64 BE) ++ t3_send_utc_ns(i64 BE)

use tokio::net::UdpSocket;

use crate::util::now_utc_ns;

/// Bind and serve the UDP time responder forever. Errors binding are returned;
/// per-datagram errors are ignored (a dropped sync just costs one calibration).
pub async fn run(bind: &str) -> std::io::Result<()> {
    let sock = UdpSocket::bind(bind).await?;
    println!("time-sync UDP responder on {bind}");
    serve(sock).await
}

/// Serve the time responder on an already-bound socket (forever).
pub async fn serve(sock: UdpSocket) -> std::io::Result<()> {
    let mut buf = [0u8; 64];
    loop {
        let Ok((n, peer)) = sock.recv_from(&mut buf).await else {
            continue;
        };
        let t2 = now_utc_ns();
        let mut nonce = [0u8; 8];
        let take = n.min(8);
        nonce[..take].copy_from_slice(&buf[..take]);

        let t3 = now_utc_ns();
        let mut reply = [0u8; 24];
        reply[..8].copy_from_slice(&nonce);
        reply[8..16].copy_from_slice(&t2.to_be_bytes());
        reply[16..24].copy_from_slice(&t3.to_be_bytes());
        let _ = sock.send_to(&reply, peer).await;
    }
}

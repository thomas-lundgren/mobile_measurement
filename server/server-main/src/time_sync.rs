//! Low-overhead UDP time responder for device clock sync (NTP-style).
//!
//! A datagram exchange avoids TCP/HTTP overhead, so the round-trip — and hence
//! the OWD-offset uncertainty the UE records in `clock.parquet` — is much
//! tighter than the `GET /api/v1/time` fallback. The four timestamps
//! (t1 client-send, t2 server-recv, t3 server-send, t4 client-recv) let the UE
//! subtract the server's own turnaround (t3−t2), unlike the single-timestamp
//! HTTP form.
//!
//! Wire format: `contracts/packets/time_sync.md` (golden vectors in
//! `contracts/testvectors/packets/time_sync_v1.json`).

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
        let reply = reply(&buf[..n], t2, now_utc_ns());
        let _ = sock.send_to(&reply, peer).await;
    }
}

/// The 24-byte reply to `request`: its nonce (first 8 bytes, zero-padded),
/// then the receive and send times, big-endian.
fn reply(request: &[u8], t2_recv_utc_ns: i64, t3_send_utc_ns: i64) -> [u8; 24] {
    let mut out = [0u8; 24];
    let take = request.len().min(8);
    out[..take].copy_from_slice(&request[..take]);
    out[8..16].copy_from_slice(&t2_recv_utc_ns.to_be_bytes());
    out[16..24].copy_from_slice(&t3_send_utc_ns.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn golden_time_sync_v1() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/testvectors/packets/time_sync_v1.json"
        );
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        for c in doc["cases"].as_array().unwrap() {
            let got = reply(
                &hex(c["request_hex"].as_str().unwrap()),
                c["t2_recv_utc_ns"].as_i64().unwrap(),
                c["t3_send_utc_ns"].as_i64().unwrap(),
            );
            assert_eq!(
                got.to_vec(),
                hex(c["reply_hex"].as_str().unwrap()),
                "{}",
                c["name"]
            );
        }
    }
}

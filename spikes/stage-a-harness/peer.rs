use fieldtest_core::packet::ProbePacket;
use std::net::UdpSocket;

fn main() -> std::io::Result<()> {
    let bind_addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:41000".to_string());
    let socket = UdpSocket::bind(&bind_addr)?;
    println!("spike-peer listening on {}", bind_addr);
    let mut buf = [0u8; 2048];
    loop {
        let (n, peer) = socket.recv_from(&mut buf)?;
        if ProbePacket::decode(&buf[..n]).is_ok() {
            socket.send_to(&buf[..n], peer)?;
        }
    }
}

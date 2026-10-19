//! Reach responders for the device's reachability check
//! (`contracts/packets/reach.md`): on each configured port, answer
//! `MPREACH1 <nonce>` with `MPREACH1 OK <nonce> <ip>:<port>` — the device's
//! address as seen here, which shows NAT and proxies in the path.
//!
//! Configured by `MP_REACH_LISTEN`, e.g. `tcp:8443,udp:8443,tcp:3478`; the
//! router maps the public ports the device dials (443, 80, …) onto these.
//! Low volume and stateless, so plain threads with blocking sockets.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::thread;
use std::time::Duration;

const MAX_LINE: usize = 64;
const TCP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
}

/// Parse `tcp:8443,udp:8443` (whitespace and empty items ignored).
pub fn parse_listen(spec: &str) -> Result<Vec<(Proto, u16)>, String> {
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|item| {
            let (proto, port) = item
                .split_once(':')
                .ok_or_else(|| format!("`{item}`: expected proto:port"))?;
            let proto = match proto {
                "tcp" => Proto::Tcp,
                "udp" => Proto::Udp,
                other => return Err(format!("`{item}`: unknown protocol {other}")),
            };
            let port = port
                .parse::<u16>()
                .map_err(|e| format!("`{item}`: bad port: {e}"))?;
            Ok((proto, port))
        })
        .collect()
}

/// The reply for one request line, or None if it is not a valid request.
pub fn reply_for(request: &[u8], peer: SocketAddr) -> Option<String> {
    let text = std::str::from_utf8(request)
        .ok()?
        .trim_end_matches(['\r', '\n']);
    let nonce = text.strip_prefix("MPREACH1 ")?;
    let valid =
        !nonce.is_empty() && nonce.len() <= 32 && nonce.bytes().all(|b| b.is_ascii_alphanumeric());
    valid.then(|| format!("MPREACH1 OK {nonce} {peer}\n"))
}

/// Bind every configured port and serve each on its own thread. Returns the
/// bound addresses (port 0 resolves to the real port, for tests).
pub fn spawn(bind_ip: &str, ports: &[(Proto, u16)]) -> std::io::Result<Vec<(Proto, SocketAddr)>> {
    let mut bound = Vec::new();
    for &(proto, port) in ports {
        match proto {
            Proto::Udp => {
                let sock = UdpSocket::bind((bind_ip, port))?;
                bound.push((proto, sock.local_addr()?));
                thread::spawn(move || serve_udp(sock));
            }
            Proto::Tcp => {
                let listener = TcpListener::bind((bind_ip, port))?;
                bound.push((proto, listener.local_addr()?));
                thread::spawn(move || serve_tcp(listener));
            }
        }
    }
    Ok(bound)
}

fn serve_udp(sock: UdpSocket) {
    let mut buf = [0u8; 256];
    loop {
        let Ok((n, peer)) = sock.recv_from(&mut buf) else {
            continue;
        };
        if let Some(reply) = reply_for(&buf[..n.min(MAX_LINE)], peer) {
            let _ = sock.send_to(reply.as_bytes(), peer);
        }
    }
}

fn serve_tcp(listener: TcpListener) {
    for stream in listener.incoming().flatten() {
        thread::spawn(move || {
            let _ = answer_tcp(stream);
        });
    }
}

fn answer_tcp(stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(TCP_TIMEOUT))?;
    stream.set_write_timeout(Some(TCP_TIMEOUT))?;
    let peer = stream.peer_addr()?;
    let mut line = Vec::with_capacity(MAX_LINE);
    BufReader::new(stream.try_clone()?)
        .take(MAX_LINE as u64)
        .read_until(b'\n', &mut line)?;
    if let Some(reply) = reply_for(&line, peer) {
        (&stream).write_all(reply.as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn parses_listen_spec() {
        assert_eq!(
            parse_listen(" tcp:8443, udp:3478 ,").unwrap(),
            vec![(Proto::Tcp, 8443), (Proto::Udp, 3478)]
        );
        assert!(parse_listen("sctp:1").is_err());
        assert!(parse_listen("tcp:99999").is_err());
    }

    #[test]
    fn reply_echoes_nonce_and_peer() {
        let peer: SocketAddr = "203.0.113.9:4242".parse().unwrap();
        assert_eq!(
            reply_for(b"MPREACH1 abc123\n", peer).as_deref(),
            Some("MPREACH1 OK abc123 203.0.113.9:4242\n")
        );
        assert_eq!(reply_for(b"MPREACH1 \n", peer), None);
        assert_eq!(reply_for(b"MPREACH1 bad-nonce\n", peer), None);
        assert_eq!(reply_for(b"GET / HTTP/1.1\r\n", peer), None);
    }

    #[test]
    fn udp_and_tcp_responders_answer() {
        let bound = spawn("127.0.0.1", &[(Proto::Udp, 0), (Proto::Tcp, 0)]).unwrap();

        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        udp.send_to(b"MPREACH1 n1\n", bound[0].1).unwrap();
        let mut buf = [0u8; 128];
        let (n, _) = udp.recv_from(&mut buf).unwrap();
        let want = format!("MPREACH1 OK n1 {}\n", udp.local_addr().unwrap());
        assert_eq!(std::str::from_utf8(&buf[..n]).unwrap(), want);

        let mut tcp = TcpStream::connect(bound[1].1).unwrap();
        tcp.write_all(b"MPREACH1 n2\n").unwrap();
        let mut reply = String::new();
        tcp.read_to_string(&mut reply).unwrap();
        assert_eq!(
            reply,
            format!("MPREACH1 OK n2 {}\n", tcp.local_addr().unwrap())
        );
    }
}

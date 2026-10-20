//! Flow endpoints shared by the UE engine (`ue`) and the server's
//! data-generator — both nodes are symmetric peers (invariant 6), so the
//! realtime UDP sender and receiver, the bulk TCP sender and receiver and the
//! per-flow counters exist once. Each side supplies only how its socket
//! reaches the other: the UE a connected socket it reopens after a network
//! switch, the server an unbound-peer socket that follows the UE's newest
//! address (`DatagramOut` / `DatagramIn`).
//!
//! Hot-path order per `contracts/internal/logger_sink.md`: timestamp → send /
//! receive → push a row onto the wait-free ring. No allocation per packet.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::log_writer::{Direction, LogSink, PacketRow, PacketTiming, Protocol};
use crate::packet::RealtimePacket;
use crate::timestamp::Timestamp;
use crate::udp_timing::{self, Pacer, SendStatus};

/// Bytes per bulk write / read (and the cap on a paced write).
pub const BULK_CHUNK: usize = 64 * 1024;

/// Per-flow counters, read live for status and at the end for the summary.
#[derive(Default)]
pub struct Counters {
    pub sent: AtomicU64,
    pub received: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub bytes_received: AtomicU64,
    /// The other node's address as first seen on this flow (the server sees
    /// the UE after NAT: which network the test really ran over).
    pub peer: Mutex<Option<(Timestamp, SocketAddr)>>,
}

impl Counters {
    pub fn sent(&self, bytes: u64) {
        self.sent.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn received(&self, bytes: u64) {
        self.received.fetch_add(1, Ordering::Relaxed);
        self.bytes_received.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Record the peer's address on first contact (later calls are no-ops).
    pub fn note_peer(&self, peer: SocketAddr) {
        let mut p = self.peer.lock().unwrap();
        if p.is_none() {
            *p = Some((Timestamp::now(), peer));
        }
    }
}

/// A read or write that timed out (socket timeout or non-blocking).
pub fn would_block(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

/// Back-pressure mode for a realtime sender (`sndbuf_bytes`): a small send
/// buffer and non-blocking sends, so a queue the link does not drain shows as
/// EAGAIN. True when applied.
pub fn back_pressure_mode(socket: &UdpSocket, sndbuf_bytes: Option<u32>) -> bool {
    match sndbuf_bytes {
        Some(bytes) => {
            udp_timing::limit_send_buffer(socket.as_raw_fd(), bytes).is_ok() && socket.set_nonblocking(true).is_ok()
        }
        None => false,
    }
}

// --- realtime UDP ------------------------------------------------------------

/// Where a realtime sender's datagrams go.
pub trait DatagramOut {
    fn socket(&self) -> &UdpSocket;
    fn send(&mut self, buf: &[u8]) -> io::Result<usize>;
    /// (local_port, remote_port) logged on the SENT rows.
    fn ports(&self) -> (Option<u32>, Option<u32>);
    /// Before each send (the server looks for a new UE address here).
    fn before_send(&mut self) {}
    /// After each send, with its outcome (the UE reopens its socket after
    /// sends keep failing).
    fn after_send(&mut self, _status: SendStatus) {}
}

/// One realtime stream: padded `RealtimePacket`s of `size` bytes every `period`.
#[derive(Debug, Clone, Copy)]
pub struct RealtimeStream {
    pub flow_id: u32,
    pub period: Duration,
    pub size: usize,
}

/// Fixed-rate sender. Logs one SENT row per attempt, tagged with its outcome,
/// the syscall time and the bytes still queued below the socket.
pub fn realtime_send(
    out: &mut impl DatagramOut,
    stream: RealtimeStream,
    sink: &mut LogSink,
    counters: &Counters,
    stop: &AtomicBool,
) {
    let period_ns = stream.period.as_nanos() as u64;
    let mut buf = vec![0u8; stream.size.max(RealtimePacket::WIRE_LEN)];
    let _ = udp_timing::raise_thread_priority();
    let mut pacer = Pacer::new(stream.period);
    let mut seq = 0u64;
    while !stop.load(Ordering::Acquire) {
        out.before_send();
        let ts = Timestamp::now();
        let head = RealtimePacket {
            flow_id: stream.flow_id,
            sequence_number: seq,
            send_mono_ns: ts.mono_ns,
            send_utc_ns: ts.utc_ns,
        }
        .to_bytes();
        buf[..head.len()].copy_from_slice(&head);
        let t0 = Instant::now();
        let result = out.send(&buf);
        let call_ns = t0.elapsed().as_nanos().min(u32::MAX as u128) as u32;
        let status = SendStatus::of(&result);
        if status == SendStatus::Ok {
            counters.sent(buf.len() as u64);
        }
        let (local_port, remote_port) = out.ports();
        sink.push(PacketRow {
            local_port,
            remote_port,
            expected_interval_ns: Some(period_ns),
            timing: PacketTiming {
                send_status: Some(status),
                send_call_ns: Some(call_ns),
                sock_outq_bytes: udp_timing::outq_bytes(out.socket().as_raw_fd()),
                ..PacketTiming::default()
            },
            ..PacketRow::new(stream.flow_id, seq, Direction::Sent, ts, buf.len() as u32, Protocol::Udp)
        });
        seq += 1;
        out.after_send(status);
        pacer.wait();
    }
}

/// Where a realtime receiver's datagrams come from.
pub trait DatagramIn {
    /// The socket to read; may change between calls (the UE reopens it).
    fn fd(&self) -> RawFd;
    /// local_port logged on the RECEIVED rows.
    fn local_port(&self) -> Option<u32>;
    /// Before each receive (the UE sends its keepalives here).
    fn before_recv(&mut self) {}
    /// Each datagram's source, as the kernel reported it.
    fn on_datagram(&mut self, _from: Option<SocketAddr>) {}
}

/// Receiver: logs RECEIVED with the sender's embedded timestamps, the kernel
/// receive time and the inter-arrival. The socket needs a read timeout so
/// the loop sees `stop`.
pub fn realtime_recv(
    inp: &mut impl DatagramIn,
    flow_id: u32,
    period_ns: u64,
    sink: &mut LogSink,
    counters: &Counters,
    stop: &AtomicBool,
) {
    let mut buf = [0u8; 2048];
    let mut last_recv_mono: Option<u64> = None;
    let mut stamped_fd: Option<RawFd> = None;
    while !stop.load(Ordering::Acquire) {
        inp.before_recv();
        let fd = inp.fd();
        if stamped_fd != Some(fd) {
            let _ = udp_timing::enable_rx_timestamps(fd);
            stamped_fd = Some(fd);
        }
        let Ok(r) = udp_timing::recv_stamped(fd, &mut buf) else {
            continue; // timeout (re-check stop) or a transient error
        };
        let recv_ts = Timestamp::now();
        inp.on_datagram(r.from);
        counters.received(r.n as u64);
        let (sender, seq) = match RealtimePacket::decode(&buf[..r.n]) {
            Ok(p) => (Some(Timestamp { mono_ns: p.send_mono_ns, utc_ns: p.send_utc_ns }), p.sequence_number),
            Err(_) => (None, 0),
        };
        let actual = last_recv_mono.map(|l| recv_ts.mono_ns.saturating_sub(l));
        last_recv_mono = Some(recv_ts.mono_ns);
        sink.push(PacketRow {
            local_port: inp.local_port(),
            sender,
            expected_interval_ns: Some(period_ns),
            actual_interval_ns: actual,
            timing: PacketTiming { kernel_rx_utc_ns: r.kernel_utc_ns, ..PacketTiming::default() },
            ..PacketRow::new(flow_id, seq, Direction::Received, recv_ts, r.n as u32, Protocol::Udp)
        });
    }
}

// --- bulk TCP ----------------------------------------------------------------

/// Bulk sender: as fast as TCP takes it, or paced to `rate_kbps`; stops after
/// `target_bytes`. One SENT row per write. The stream needs a write timeout.
pub fn bulk_send(
    stream: &mut TcpStream,
    flow_id: u32,
    rate_kbps: Option<u32>,
    target_bytes: Option<u64>,
    sink: &mut LogSink,
    counters: &Counters,
    stop: &AtomicBool,
) {
    let (chunk, mut pacer) = match rate_kbps {
        Some(kbps) => {
            let (chunk, period) = udp_timing::bulk_pacing(kbps, BULK_CHUNK);
            (chunk, Some(Pacer::new(period)))
        }
        None => (BULK_CHUNK, None),
    };
    let payload = vec![0u8; chunk];
    let mut seq = 0u64;
    let mut total = 0u64;
    while !stop.load(Ordering::Acquire) {
        if let Some(p) = pacer.as_mut() {
            p.wait();
        }
        let ts = Timestamp::now();
        match stream.write(&payload) {
            Ok(0) => break,
            Ok(n) => {
                counters.sent(n as u64);
                total += n as u64;
                sink.push(PacketRow::tcp(flow_id, seq, Direction::Sent, ts, n as u32));
                seq += 1;
                if target_bytes.is_some_and(|t| total >= t) {
                    break;
                }
            }
            Err(ref e) if would_block(e) => {}
            Err(_) => break,
        }
    }
}

/// Bulk receiver: one RECEIVED row per read. The stream needs a read timeout.
pub fn bulk_recv(stream: &mut TcpStream, flow_id: u32, sink: &mut LogSink, counters: &Counters, stop: &AtomicBool) {
    let mut buf = vec![0u8; BULK_CHUNK];
    let mut seq = 0u64;
    while !stop.load(Ordering::Acquire) {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let ts = Timestamp::now();
                counters.received(n as u64);
                sink.push(PacketRow::tcp(flow_id, seq, Direction::Received, ts, n as u32));
                seq += 1;
            }
            Err(ref e) if would_block(e) => {}
            Err(_) => break,
        }
    }
}

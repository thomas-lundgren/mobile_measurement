//! UE-side traffic engine (Phase 2). The mirror of the server's
//! `data-generator` from the *initiating* side (invariant 5: the UE always
//! opens the connection / sends first). One real-time thread per flow, each
//! feeding its own wait-free SPSC ring; one writer drains them all into the
//! per-test `mobile/packets.parquet`.
//!
//! Config is a **lean** serde subset (no `mp-contracts` dependency — keeps the
//! Android `.so` small). The Java shell / interop test merges the contract
//! `RunTestPayload` (plan flows + flow_bindings + server_host) into this shape.

use std::io::{Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::log_writer::{
    packet_channel, spawn_packet_writer_multi, Direction, LogContext, LogSink, PacketRow, Protocol,
    WriterHandle,
};
use crate::packet::{ProbePacket, RealtimePacket};
use crate::timestamp::Timestamp;

const RING_CAPACITY: usize = 8192;
const BULK_CHUNK: usize = 64 * 1024;
const RECV_TIMEOUT: Duration = Duration::from_millis(100);
const KEEPALIVE_PERIOD: Duration = Duration::from_secs(1);

/// What the UE engine needs to run one test (lean merge of RunTestPayload +
/// bindings). `mobile/` log dir is `log_dir`.
#[derive(Debug, Clone, Deserialize)]
pub struct UeTestConfig {
    pub test_id: String,
    pub device_id: String,
    pub session_id: String,
    pub server_host: String,
    pub log_dir: String,
    #[serde(default = "default_segment_rows")]
    pub segment_rows: usize,
    pub flows: Vec<UeFlow>,
}

fn default_segment_rows() -> usize {
    1000
}

#[derive(Debug, Clone, Deserialize)]
pub struct UeFlow {
    pub flow_id: u32,
    /// "probe" | "realtime_udp" | "bulk_tcp"
    #[serde(rename = "type")]
    pub flow_type: String,
    /// "FromServer" | "ToServer" (absent for probe)
    #[serde(default)]
    pub direction: Option<String>,
    /// None for gateway_ping flows (no server endpoint; first-hop probed locally).
    #[serde(default)]
    pub server_port: Option<u16>,
    #[serde(default)]
    pub rate_pps: Option<u32>,
    #[serde(default)]
    pub packet_size_bytes: Option<u32>,
    #[serde(default)]
    pub target_bytes: Option<u64>,
}

#[derive(Default)]
struct Counters {
    sent: AtomicU64,
    received: AtomicU64,
    bytes_sent: AtomicU64,
    bytes_received: AtomicU64,
}

/// Per-flow result counters (for status / summary).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FlowSummary {
    pub flow_id: u32,
    pub packets_sent: u64,
    pub packets_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub dropped_rows: u64,
}

struct FlowHandle {
    flow_id: u32,
    counters: Arc<Counters>,
    dropped: Arc<AtomicU64>,
    join: Option<JoinHandle<()>>,
}

/// A running test: one flow thread per flow + one writer fanning their rings
/// into `mobile/packets.parquet`. Drop or `stop()` to finish.
pub struct UeEngine {
    stop: Arc<AtomicBool>,
    flows: Vec<FlowHandle>,
    writer: Option<WriterHandle>,
}

impl UeEngine {
    pub fn start(cfg: UeTestConfig) -> std::io::Result<Self> {
        let ctx = LogContext {
            device_id: cfg.device_id.clone(),
            session_id: cfg.session_id.clone(),
            test_id: cfg.test_id.clone(),
            node: "mobile".to_string(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let mut rxs = Vec::with_capacity(cfg.flows.len());
        let mut flows = Vec::with_capacity(cfg.flows.len());

        for flow in &cfg.flows {
            let (sink, rx, dropped) = packet_channel(RING_CAPACITY);
            rxs.push(rx);
            let counters = Arc::new(Counters::default());
            let join = spawn_flow(
                flow.clone(),
                &cfg.server_host,
                sink,
                counters.clone(),
                stop.clone(),
            )?;
            flows.push(FlowHandle {
                flow_id: flow.flow_id,
                counters,
                dropped,
                join: Some(join),
            });
        }

        let writer = spawn_packet_writer_multi(rxs, &cfg.log_dir, cfg.segment_rows, ctx)?;
        Ok(Self {
            stop,
            flows,
            writer: Some(writer),
        })
    }

    /// Live per-flow counters (without stopping).
    pub fn status(&self) -> Vec<FlowSummary> {
        self.flows.iter().map(|f| f.summary()).collect()
    }

    /// Stop all flows, flush + seal Parquet, return per-flow summaries.
    pub fn stop(mut self) -> Vec<FlowSummary> {
        self.stop.store(true, Ordering::Release);
        for f in self.flows.iter_mut() {
            if let Some(h) = f.join.take() {
                let _ = h.join();
            }
        }
        if let Some(w) = self.writer.take() {
            w.shutdown();
        }
        self.flows.iter().map(|f| f.summary()).collect()
    }
}

impl FlowHandle {
    fn summary(&self) -> FlowSummary {
        FlowSummary {
            flow_id: self.flow_id,
            packets_sent: self.counters.sent.load(Ordering::Relaxed),
            packets_received: self.counters.received.load(Ordering::Relaxed),
            bytes_sent: self.counters.bytes_sent.load(Ordering::Relaxed),
            bytes_received: self.counters.bytes_received.load(Ordering::Relaxed),
            dropped_rows: self.dropped.load(Ordering::Relaxed),
        }
    }
}

fn spawn_flow(
    flow: UeFlow,
    server_host: &str,
    sink: LogSink,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    let dir = flow.direction.as_deref();

    match (flow.flow_type.as_str(), dir) {
        ("probe", _) | ("realtime_udp", _) | ("bulk_tcp", _) => {
            let port = flow.server_port.ok_or_else(|| {
                std::io::Error::other(format!("flow {} missing server_port", flow.flow_id))
            })?;
            let server = format!("{server_host}:{port}");
            match (flow.flow_type.as_str(), dir) {
                ("probe", _) => {
                    let socket = connected_udp(&server)?;
                    Ok(thread::spawn(move || probe_ue(socket, sink, flow, counters, stop)))
                }
                ("realtime_udp", Some("ToServer")) => {
                    let socket = connected_udp(&server)?;
                    Ok(thread::spawn(move || realtime_send(socket, sink, flow, counters, stop)))
                }
                ("realtime_udp", _) => {
                    let socket = connected_udp(&server)?;
                    Ok(thread::spawn(move || realtime_recv(socket, sink, flow, counters, stop)))
                }
                ("bulk_tcp", Some("ToServer")) => {
                    let stream = TcpStream::connect(&server)?;
                    Ok(thread::spawn(move || bulk_send(stream, sink, flow, counters, stop)))
                }
                _ => {
                    let stream = TcpStream::connect(&server)?;
                    Ok(thread::spawn(move || bulk_recv(stream, sink, flow, counters, stop)))
                }
            }
        }
        // gateway_ping: local probe to auto-discovered first-hop gateway.
        // Full implementation is a later milestone; stub thread exits immediately.
        ("gateway_ping", _) => Ok(thread::spawn(move || {
            drop((sink, counters, stop));
        })),
        // dscp_probe: DSCP transparency measurement via IP_RECVTOS / NDK recvmsg.
        // Full implementation is a later milestone; stub exits immediately.
        ("dscp_probe", _) => Ok(thread::spawn(move || {
            drop((sink, counters, stop));
        })),
        (other, _) => Err(std::io::Error::other(format!("unknown flow type: {other}"))),
    }
}

fn connected_udp(server: &str) -> std::io::Result<UdpSocket> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(RECV_TIMEOUT))?;
    socket.connect(server)?;
    Ok(socket)
}

fn would_block(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

fn rate_period(rate_pps: Option<u32>) -> Duration {
    Duration::from_nanos(1_000_000_000u64 / rate_pps.unwrap_or(10).max(1) as u64)
}

// --- flow behaviors --------------------------------------------------------

/// Probe: UE originates, server echoes. Log SENT, then RECEIVED on echo.
fn probe_ue(
    socket: UdpSocket,
    mut sink: LogSink,
    flow: UeFlow,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) {
    let period = rate_period(flow.rate_pps);
    let size = flow
        .packet_size_bytes
        .unwrap_or(ProbePacket::WIRE_LEN as u32) as usize;
    let mut recv_buf = [0u8; 2048];
    let mut seq = 1u64;
    let mut next = Instant::now();

    while !stop.load(Ordering::Acquire) {
        let ts = Timestamp::now();
        let mut bytes = ProbePacket {
            flow_id: flow.flow_id,
            sequence_number: seq,
            send_mono_ns: ts.mono_ns,
        }
        .to_bytes()
        .to_vec();
        if size > bytes.len() {
            bytes.resize(size, 0);
        }
        if socket.send(&bytes).is_ok() {
            counters.sent.fetch_add(1, Ordering::Relaxed);
            counters
                .bytes_sent
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
            sink.push(PacketRow::probe(
                flow.flow_id,
                seq,
                Direction::Sent,
                ts,
                bytes.len() as u32,
            ));
        }
        if let Ok(n) = socket.recv(&mut recv_buf) {
            let recv_ts = Timestamp::now();
            if let Ok(echo) = ProbePacket::decode(&recv_buf[..n]) {
                counters.received.fetch_add(1, Ordering::Relaxed);
                counters
                    .bytes_received
                    .fetch_add(n as u64, Ordering::Relaxed);
                sink.push(PacketRow::probe(
                    flow.flow_id,
                    echo.sequence_number,
                    Direction::Received,
                    recv_ts,
                    n as u32,
                ));
            }
        }
        seq += 1;
        next += period;
        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}

/// Realtime ToServer: UE sends the fixed-rate padded stream. Log SENT.
fn realtime_send(
    socket: UdpSocket,
    mut sink: LogSink,
    flow: UeFlow,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) {
    let period = rate_period(flow.rate_pps);
    let period_ns = period.as_nanos() as u64;
    let size = flow
        .packet_size_bytes
        .unwrap_or(RealtimePacket::WIRE_LEN as u32) as usize;
    let mut seq = 0u64;
    let mut next = Instant::now();

    while !stop.load(Ordering::Acquire) {
        let ts = Timestamp::now();
        let bytes = RealtimePacket {
            flow_id: flow.flow_id,
            sequence_number: seq,
            send_mono_ns: ts.mono_ns,
            send_utc_ns: ts.utc_ns,
        }
        .to_bytes_padded(size);
        if socket.send(&bytes).is_ok() {
            counters.sent.fetch_add(1, Ordering::Relaxed);
            counters
                .bytes_sent
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
            sink.push(PacketRow {
                flow_id: flow.flow_id,
                sequence_number: seq,
                direction: Direction::Sent,
                timestamp: ts,
                payload_size: bytes.len() as u32,
                protocol: Protocol::Udp,
                local_port: None,
                remote_port: None,
                sender: None,
                expected_interval_ns: Some(period_ns),
                actual_interval_ns: None,
                dscp_sent: None,
                dscp_recv: None,
            });
        }
        seq += 1;
        next += period;
        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}

/// Realtime FromServer: UE sends ~1 Hz keepalives (NAT punch-through) and
/// receives the downlink, logging RECEIVED with sender timestamps + interval.
fn realtime_recv(
    socket: UdpSocket,
    mut sink: LogSink,
    flow: UeFlow,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) {
    let period_ns = rate_period(flow.rate_pps).as_nanos() as u64;
    let mut buf = [0u8; 2048];
    let mut last_keepalive = Instant::now() - KEEPALIVE_PERIOD; // send one immediately
    let mut last_recv_mono: Option<u64> = None;

    while !stop.load(Ordering::Acquire) {
        if last_keepalive.elapsed() >= KEEPALIVE_PERIOD {
            let _ = socket.send(b"mp-keepalive");
            last_keepalive = Instant::now();
        }
        match socket.recv(&mut buf) {
            Ok(n) => {
                let recv_ts = Timestamp::now();
                counters.received.fetch_add(1, Ordering::Relaxed);
                counters
                    .bytes_received
                    .fetch_add(n as u64, Ordering::Relaxed);
                let (sender, seq) = match RealtimePacket::decode(&buf[..n]) {
                    Ok(p) => (
                        Some(Timestamp {
                            mono_ns: p.send_mono_ns,
                            utc_ns: p.send_utc_ns,
                        }),
                        p.sequence_number,
                    ),
                    Err(_) => (None, 0),
                };
                let actual = last_recv_mono.map(|l| recv_ts.mono_ns.saturating_sub(l));
                last_recv_mono = Some(recv_ts.mono_ns);
                sink.push(PacketRow {
                    flow_id: flow.flow_id,
                    sequence_number: seq,
                    direction: Direction::Received,
                    timestamp: recv_ts,
                    payload_size: n as u32,
                    protocol: Protocol::Udp,
                    local_port: None,
                    remote_port: None,
                    sender,
                    expected_interval_ns: Some(period_ns),
                    actual_interval_ns: actual,
                    dscp_sent: None,
                    dscp_recv: None,
                });
            }
            Err(ref e) if would_block(e) => {}
            Err(_) => {}
        }
    }
}

/// Bulk ToServer: UE uploads chunks. One row per chunk.
fn bulk_send(
    mut stream: TcpStream,
    mut sink: LogSink,
    flow: UeFlow,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) {
    let _ = stream.set_write_timeout(Some(RECV_TIMEOUT));
    let payload = vec![0u8; BULK_CHUNK];
    let mut seq = 0u64;
    let mut total = 0u64;
    while !stop.load(Ordering::Acquire) {
        let ts = Timestamp::now();
        match stream.write(&payload) {
            Ok(0) => break,
            Ok(n) => {
                counters.sent.fetch_add(1, Ordering::Relaxed);
                counters.bytes_sent.fetch_add(n as u64, Ordering::Relaxed);
                total += n as u64;
                sink.push(bulk_row(flow.flow_id, seq, Direction::Sent, ts, n as u32));
                seq += 1;
                if flow.target_bytes.is_some_and(|t| total >= t) {
                    break;
                }
            }
            Err(ref e) if would_block(e) => {}
            Err(_) => break,
        }
    }
}

/// Bulk FromServer: UE downloads chunks. One row per chunk.
fn bulk_recv(
    mut stream: TcpStream,
    mut sink: LogSink,
    flow: UeFlow,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) {
    let _ = stream.set_read_timeout(Some(RECV_TIMEOUT));
    let mut buf = vec![0u8; BULK_CHUNK];
    let mut seq = 0u64;
    while !stop.load(Ordering::Acquire) {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let ts = Timestamp::now();
                counters.received.fetch_add(1, Ordering::Relaxed);
                counters
                    .bytes_received
                    .fetch_add(n as u64, Ordering::Relaxed);
                sink.push(bulk_row(
                    flow.flow_id,
                    seq,
                    Direction::Received,
                    ts,
                    n as u32,
                ));
                seq += 1;
            }
            Err(ref e) if would_block(e) => {}
            Err(_) => break,
        }
    }
}

fn bulk_row(flow_id: u32, seq: u64, direction: Direction, ts: Timestamp, size: u32) -> PacketRow {
    PacketRow {
        flow_id,
        sequence_number: seq,
        direction,
        timestamp: ts,
        payload_size: size,
        protocol: Protocol::Tcp,
        local_port: None,
        remote_port: None,
        sender: None,
        expected_interval_ns: None,
        actual_interval_ns: None,
        dscp_sent: None,
        dscp_recv: None,
    }
}

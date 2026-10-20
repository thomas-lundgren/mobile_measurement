//! UE-side traffic engine (Phase 2). The mirror of the server's
//! `data-generator` from the *initiating* side (invariant 5: the UE always
//! opens the connection / sends first). One real-time thread per flow, each
//! feeding its own wait-free SPSC ring; one writer drains them all into the
//! per-test `mobile/packets.parquet`.
//!
//! Config is a **lean** serde subset (no `mp-contracts` dependency — keeps the
//! Android `.so` small). The Java shell / interop test merges the contract
//! `RunTestPayload` (plan flows + flow_bindings + server_host) into this shape.

use std::net::{TcpStream, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::endpoint::{self, Counters, RealtimeStream};
use crate::log_writer::{
    packet_channel, spawn_packet_writer_multi, Direction, LogContext, LogSink, PacketRow,
    PacketTiming, WriterHandle,
};
use crate::packet::{DscpProbePacket, ProbePacket, DSCP_UNKNOWN};
use crate::timestamp::Timestamp;
use crate::udp_timing::{self, SendStatus};

const RING_CAPACITY: usize = 8192;
const RECV_TIMEOUT: Duration = Duration::from_millis(100);
const KEEPALIVE_PERIOD: Duration = Duration::from_secs(1);
/// Sends must fail this long before a flow reopens its UDP socket.
const REOPEN_AFTER: Duration = Duration::from_millis(200);
/// At most one reopen attempt per this interval while sends keep failing.
const REOPEN_RETRY: Duration = Duration::from_millis(500);

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
    crate::log_writer::SEGMENT_ROWS
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
    /// DSCP values (0–63) to cycle through. Present for dscp_probe flows only.
    #[serde(default)]
    pub dscp_values: Option<Vec<u8>>,
    /// realtime_udp sender only: limit SO_SNDBUF to this and send
    /// non-blocking, so a full queue below the socket shows as EAGAIN.
    #[serde(default)]
    pub sndbuf_bytes: Option<u32>,
    /// bulk_tcp sender only: pace writes to this many kbit/s.
    #[serde(default)]
    pub rate_kbps: Option<u32>,
    /// app_traffic: user model (`app_traffic::PROFILES`), concurrent agents,
    /// seed, byte scale and — for profile "script" — the action timeline.
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub workers: Option<u32>,
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub scale: Option<f64>,
    #[serde(default)]
    pub actions: Option<Vec<crate::app_traffic::AppAction>>,
    #[serde(default)]
    pub placement: Option<String>,
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
    mut sink: LogSink,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    let dir = flow.direction.as_deref();

    match (flow.flow_type.as_str(), dir) {
        ("probe", _)
        | ("realtime_udp", _)
        | ("bulk_tcp", _)
        | ("dscp_probe", _)
        | ("app_traffic", _) => {
            let port = flow.server_port.ok_or_else(|| {
                std::io::Error::other(format!("flow {} missing server_port", flow.flow_id))
            })?;
            let server = format!("{server_host}:{port}");
            match (flow.flow_type.as_str(), dir) {
                ("probe", _) => {
                    let socket = connected_udp(&server)?;
                    let reopen = Reopener::new(&server);
                    Ok(thread::spawn(move || {
                        probe_ue(socket, reopen, sink, flow, counters, stop)
                    }))
                }
                ("realtime_udp", Some("ToServer")) => {
                    let socket = connected_udp(&server)?;
                    endpoint::back_pressure_mode(&socket, flow.sndbuf_bytes);
                    let mut out = UeOut {
                        port: local_port(&socket),
                        socket,
                        reopen: Reopener::new(&server),
                        sndbuf_bytes: flow.sndbuf_bytes,
                    };
                    let stream = RealtimeStream {
                        flow_id: flow.flow_id,
                        period: rate_period(flow.rate_pps),
                        size: flow.packet_size_bytes.unwrap_or(0) as usize,
                    };
                    Ok(thread::spawn(move || {
                        endpoint::realtime_send(&mut out, stream, &mut sink, &counters, &stop)
                    }))
                }
                ("realtime_udp", _) => {
                    let socket = connected_udp(&server)?;
                    let mut inp = UeIn {
                        port: local_port(&socket),
                        socket,
                        reopen: Reopener::new(&server),
                        last_keepalive: Instant::now() - KEEPALIVE_PERIOD, // send one immediately
                    };
                    let period_ns = rate_period(flow.rate_pps).as_nanos() as u64;
                    Ok(thread::spawn(move || {
                        endpoint::realtime_recv(
                            &mut inp,
                            flow.flow_id,
                            period_ns,
                            &mut sink,
                            &counters,
                            &stop,
                        )
                    }))
                }
                ("bulk_tcp", Some("ToServer")) => {
                    let mut stream = TcpStream::connect(&server)?;
                    stream.set_write_timeout(Some(RECV_TIMEOUT))?;
                    Ok(thread::spawn(move || {
                        endpoint::bulk_send(
                            &mut stream,
                            flow.flow_id,
                            flow.rate_kbps,
                            flow.target_bytes,
                            &mut sink,
                            &counters,
                            &stop,
                        )
                    }))
                }
                ("app_traffic", _) => {
                    let app = crate::app_traffic::AppTraffic {
                        server,
                        flow_id: flow.flow_id,
                        profile: flow.profile.clone().unwrap_or_else(|| "mixed".into()),
                        workers: flow.workers.unwrap_or(1),
                        seed: flow.seed.unwrap_or(1),
                        scale: flow.scale.unwrap_or(1.0),
                        actions: flow.actions.clone().unwrap_or_default(),
                        placement: crate::app_traffic::Placement::parse(
                            flow.placement.as_deref().unwrap_or("device"),
                        ),
                    };
                    Ok(thread::spawn(move || {
                        crate::app_traffic::run(&app, sink, &counters, &stop)
                    }))
                }
                ("dscp_probe", _) => {
                    let socket = connected_udp(&server)?;
                    Ok(thread::spawn(move || {
                        dscp_probe_ue(socket, sink, flow, counters, stop)
                    }))
                }
                _ => {
                    let mut stream = TcpStream::connect(&server)?;
                    stream.set_read_timeout(Some(RECV_TIMEOUT))?;
                    Ok(thread::spawn(move || {
                        endpoint::bulk_recv(&mut stream, flow.flow_id, &mut sink, &counters, &stop)
                    }))
                }
            }
        }
        // gateway_ping: local probe to auto-discovered first-hop gateway.
        // Full implementation is a later milestone; stub thread exits immediately.
        ("gateway_ping", _) => Ok(thread::spawn(move || {
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

/// Follows the phone's default network across a Wi-Fi <-> mobile switch.
///
/// A connected UDP socket keeps the source address of the network it was
/// opened on; once that network is gone every send fails. When sends have
/// failed for `REOPEN_AFTER`, the flow opens a fresh socket, which the OS
/// routes over the current default network. The new socket has a new local
/// port, logged in `local_port` on the flow's rows, so a switch shows in the
/// packet log; the server answers whatever address the UE sends from.
struct Reopener {
    server: String,
    failing_since: Option<Instant>,
    last_try: Option<Instant>,
}

impl Reopener {
    fn new(server: &str) -> Self {
        Self {
            server: server.to_string(),
            failing_since: None,
            last_try: None,
        }
    }

    /// Record a send outcome; true when the socket should be reopened now.
    fn note(&mut self, failed: bool, now: Instant) -> bool {
        if !failed {
            self.failing_since = None;
            return false;
        }
        let since = *self.failing_since.get_or_insert(now);
        now.duration_since(since) >= REOPEN_AFTER
            && self
                .last_try
                .is_none_or(|t| now.duration_since(t) >= REOPEN_RETRY)
    }

    /// A fresh socket to the server (None while no network can route there).
    fn reopen(&mut self, now: Instant) -> Option<UdpSocket> {
        self.last_try = Some(now);
        connected_udp(&self.server).ok()
    }
}

fn local_port(socket: &UdpSocket) -> Option<u32> {
    socket.local_addr().ok().map(|a| a.port() as u32)
}

fn rate_period(rate_pps: Option<u32>) -> Duration {
    Duration::from_nanos(1_000_000_000u64 / rate_pps.unwrap_or(10).max(1) as u64)
}

// --- flow behaviors --------------------------------------------------------

/// Probe: UE originates, server echoes. Log SENT, then RECEIVED on echo.
fn probe_ue(
    mut socket: UdpSocket,
    mut reopen: Reopener,
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
    // kernel receive times: an echo's RTT must not include how late this
    // thread got to read it (the app traffic beside it loads the CPU); the
    // send side still wants the CPU on time
    let _ = udp_timing::enable_rx_timestamps(socket.as_raw_fd());
    let _ = udp_timing::raise_thread_priority();

    let mut bytes = vec![0u8; size.max(ProbePacket::WIRE_LEN)];

    while !stop.load(Ordering::Acquire) {
        let ts = Timestamp::now();
        let head = ProbePacket {
            flow_id: flow.flow_id,
            sequence_number: seq,
            send_mono_ns: ts.mono_ns,
        }
        .to_bytes();
        bytes[..head.len()].copy_from_slice(&head);
        let sent = socket.send(&bytes).is_ok();
        if sent {
            counters.sent(bytes.len() as u64);
            sink.push(PacketRow {
                local_port: local_port(&socket),
                ..PacketRow::probe(flow.flow_id, seq, Direction::Sent, ts, bytes.len() as u32)
            });
        }
        let now = Instant::now();
        if reopen.note(!sent, now) {
            if let Some(s) = reopen.reopen(now) {
                let _ = udp_timing::enable_rx_timestamps(s.as_raw_fd());
                socket = s;
            }
        }
        seq += 1;
        next = (next + period).max(Instant::now());
        recv_until(
            socket.as_raw_fd(),
            next,
            &mut recv_buf,
            |pkt, r, recv_ts| {
                if let Ok(echo) = ProbePacket::decode(pkt) {
                    counters.received(pkt.len() as u64);
                    sink.push(PacketRow {
                        timing: PacketTiming {
                            kernel_rx_utc_ns: r.kernel_utc_ns,
                            ..PacketTiming::default()
                        },
                        ..PacketRow::probe(
                            flow.flow_id,
                            echo.sequence_number,
                            Direction::Received,
                            recv_ts,
                            pkt.len() as u32,
                        )
                    });
                }
            },
        );
        sleep_until(next);
    }
}

/// Spend the gap until the next send receiving: read each echo the moment it
/// arrives (so its timestamp is its arrival) until `deadline`. Everything
/// already queued is read first, without blocking; the wait is a `ppoll`, so
/// the socket's options never change per packet.
///
/// Reading one echo per send instead leaves a permanent backlog after a single
/// late echo (a stall longer than the read timeout): every later echo is then
/// read — and timestamped — one or more send periods after it arrived, so RTT
/// comes out as true RTT + k × period.
fn recv_until(
    fd: std::os::fd::RawFd,
    deadline: Instant,
    buf: &mut [u8],
    mut on_recv: impl FnMut(&[u8], &udp_timing::Received, Timestamp),
) {
    loop {
        loop {
            match udp_timing::recv_stamped_nowait(fd, buf) {
                Ok(r) => {
                    let ts = Timestamp::now();
                    on_recv(&buf[..r.n], &r, ts);
                }
                Err(ref e) if endpoint::would_block(e) => break,
                // an immediate error (e.g. ICMP refused during an outage):
                // the caller sleeps out the remainder
                Err(_) => return,
            }
        }
        let wait = deadline.saturating_duration_since(Instant::now());
        if wait.is_zero() || !udp_timing::wait_readable(fd, wait) {
            return;
        }
    }
}

fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if deadline > now {
        thread::sleep(deadline - now);
    }
}

/// The UE's realtime sender socket: connected to the server, reopened (in
/// back-pressure mode again if asked) after sends keep failing — the OS then
/// routes it over the current default network.
struct UeOut {
    socket: UdpSocket,
    reopen: Reopener,
    sndbuf_bytes: Option<u32>,
    port: Option<u32>,
}

impl endpoint::DatagramOut for UeOut {
    fn socket(&self) -> &UdpSocket {
        &self.socket
    }

    fn send(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.socket.send(buf)
    }

    fn ports(&self) -> (Option<u32>, Option<u32>) {
        (self.port, None)
    }

    fn after_send(&mut self, status: SendStatus) {
        let now = Instant::now();
        if self.reopen.note(status == SendStatus::Error, now) {
            if let Some(s) = self.reopen.reopen(now) {
                endpoint::back_pressure_mode(&s, self.sndbuf_bytes);
                self.port = local_port(&s);
                self.socket = s;
            }
        }
    }
}

/// The UE's realtime receiver socket: sends ~1 Hz keepalives (NAT
/// punch-through; they also tell the server where to send after a network
/// switch) and reopens when they keep failing.
struct UeIn {
    socket: UdpSocket,
    reopen: Reopener,
    last_keepalive: Instant,
    port: Option<u32>,
}

impl endpoint::DatagramIn for UeIn {
    fn fd(&self) -> std::os::fd::RawFd {
        self.socket.as_raw_fd()
    }

    fn local_port(&self) -> Option<u32> {
        self.port
    }

    fn before_recv(&mut self) {
        if self.last_keepalive.elapsed() < KEEPALIVE_PERIOD {
            return;
        }
        let failed = self.socket.send(b"mp-keepalive").is_err();
        let now = Instant::now();
        // retry a failed keepalive every RECV_TIMEOUT, not once a second,
        // so a dead network is noticed within REOPEN_AFTER
        self.last_keepalive = if failed {
            now - KEEPALIVE_PERIOD + RECV_TIMEOUT
        } else {
            now
        };
        if self.reopen.note(failed, now) {
            if let Some(s) = self.reopen.reopen(now) {
                self.port = local_port(&s);
                self.socket = s;
                self.last_keepalive = now - KEEPALIVE_PERIOD; // announce the new address at once
            }
        }
    }
}

/// DSCP transparency probe: cycles through `dscp_values` on each sent packet,
/// reads the echoed packet's IP TOS to measure downlink transparency.
///
/// Log rows:
/// - SENT: `dscp_sent = dscp_intended_ul`, `dscp_recv = None`
/// - RECEIVED: `dscp_sent = dscp_intended_dl` (server's intended DL DSCP),
///             `dscp_recv = dscp_actual_dl` (TOS observed on arriving echo)
fn dscp_probe_ue(
    socket: UdpSocket,
    mut sink: LogSink,
    flow: UeFlow,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) {
    let period = rate_period(flow.rate_pps);
    let dscp_values: Vec<u8> = flow
        .dscp_values
        .as_deref()
        .filter(|v| !v.is_empty())
        .map(|v| v.to_vec())
        .unwrap_or_else(|| vec![0]);

    let fd = socket.as_raw_fd();
    let _ = udp_timing::enable_recv_tos(fd);

    let mut recv_buf = [0u8; 2048];
    let mut seq = 1u64;
    let mut next = Instant::now();

    while !stop.load(Ordering::Acquire) {
        let dscp_intended_ul = dscp_values[((seq - 1) as usize) % dscp_values.len()];

        let _ = udp_timing::set_tos(fd, dscp_intended_ul << 2);

        let ts = Timestamp::now();
        let pkt = DscpProbePacket {
            flow_id: flow.flow_id,
            sequence_number: seq,
            send_mono_ns: ts.mono_ns,
            dscp_intended_ul,
            dscp_actual_ul: DSCP_UNKNOWN,
            dscp_intended_dl: DSCP_UNKNOWN,
        };
        let bytes = pkt.to_bytes();
        if socket.send(&bytes).is_ok() {
            counters.sent(bytes.len() as u64);
            sink.push(PacketRow {
                dscp_sent: Some(dscp_intended_ul),
                ..PacketRow::probe(flow.flow_id, seq, Direction::Sent, ts, bytes.len() as u32)
            });
        }

        seq += 1;
        next = (next + period).max(Instant::now());
        recv_until(fd, next, &mut recv_buf, |pkt, r, recv_ts| {
            let n = pkt.len();
            if let Ok(echo) = DscpProbePacket::decode(pkt) {
                counters.received(n as u64);
                let dscp_actual_dl = r.tos.map(|t| t >> 2).unwrap_or(DSCP_UNKNOWN);
                sink.push(PacketRow {
                    dscp_sent: Some(echo.dscp_intended_dl),
                    dscp_recv: Some(dscp_actual_dl),
                    ..PacketRow::probe(
                        flow.flow_id,
                        echo.sequence_number,
                        Direction::Received,
                        recv_ts,
                        n as u32,
                    )
                });
            }
        });
        sleep_until(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopens_only_after_sends_keep_failing() {
        let mut r = Reopener::new("127.0.0.1:9");
        let t0 = Instant::now();
        assert!(!r.note(true, t0));
        assert!(!r.note(true, t0 + Duration::from_millis(150)));
        assert!(r.note(true, t0 + REOPEN_AFTER));
        assert!(r.reopen(t0 + REOPEN_AFTER).is_some());
        // still failing on the new socket: retry at most every REOPEN_RETRY
        assert!(!r.note(true, t0 + REOPEN_AFTER + Duration::from_millis(100)));
        assert!(r.note(true, t0 + REOPEN_AFTER + REOPEN_RETRY));
        // one success resets the streak
        assert!(!r.note(false, t0 + Duration::from_secs(2)));
        assert!(!r.note(true, t0 + Duration::from_secs(2)));
    }
}

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
use std::mem;
use std::net::{TcpStream, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::log_writer::{
    packet_channel, spawn_packet_writer_multi, Direction, LogContext, LogSink, PacketRow, PacketTiming,
    Protocol, WriterHandle,
};
use crate::packet::{DscpProbePacket, ProbePacket, RealtimePacket, DSCP_UNKNOWN};
use crate::timestamp::Timestamp;
use crate::udp_timing::{self, Pacer, SendStatus};

const RING_CAPACITY: usize = 8192;
const BULK_CHUNK: usize = 64 * 1024;
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
        ("probe", _) | ("realtime_udp", _) | ("bulk_tcp", _) | ("dscp_probe", _) => {
            let port = flow.server_port.ok_or_else(|| {
                std::io::Error::other(format!("flow {} missing server_port", flow.flow_id))
            })?;
            let server = format!("{server_host}:{port}");
            match (flow.flow_type.as_str(), dir) {
                ("probe", _) => {
                    let socket = connected_udp(&server)?;
                    let reopen = Reopener::new(&server);
                    Ok(thread::spawn(move || probe_ue(socket, reopen, sink, flow, counters, stop)))
                }
                ("realtime_udp", Some("ToServer")) => {
                    let socket = connected_udp(&server)?;
                    let reopen = Reopener::new(&server);
                    Ok(thread::spawn(move || realtime_send(socket, reopen, sink, flow, counters, stop)))
                }
                ("realtime_udp", _) => {
                    let socket = connected_udp(&server)?;
                    let reopen = Reopener::new(&server);
                    Ok(thread::spawn(move || realtime_recv(socket, reopen, sink, flow, counters, stop)))
                }
                ("bulk_tcp", Some("ToServer")) => {
                    let stream = TcpStream::connect(&server)?;
                    Ok(thread::spawn(move || bulk_send(stream, sink, flow, counters, stop)))
                }
                ("dscp_probe", _) => {
                    let socket = connected_udp(&server)?;
                    Ok(thread::spawn(move || dscp_probe_ue(socket, sink, flow, counters, stop)))
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
            && self.last_try.is_none_or(|t| now.duration_since(t) >= REOPEN_RETRY)
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
        let sent = socket.send(&bytes).is_ok();
        if sent {
            counters.sent.fetch_add(1, Ordering::Relaxed);
            counters
                .bytes_sent
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
            sink.push(PacketRow {
                local_port: local_port(&socket),
                ..PacketRow::probe(flow.flow_id, seq, Direction::Sent, ts, bytes.len() as u32)
            });
        }
        let now = Instant::now();
        if reopen.note(!sent, now) {
            if let Some(s) = reopen.reopen(now) {
                socket = s;
            }
        }
        seq += 1;
        next = (next + period).max(Instant::now());
        recv_until(
            &socket,
            next,
            &mut recv_buf,
            |buf| socket.recv(buf).map(|n| (n, ())),
            |pkt, (), recv_ts| {
                if let Ok(echo) = ProbePacket::decode(pkt) {
                    counters.received.fetch_add(1, Ordering::Relaxed);
                    counters
                        .bytes_received
                        .fetch_add(pkt.len() as u64, Ordering::Relaxed);
                    sink.push(PacketRow::probe(
                        flow.flow_id,
                        echo.sequence_number,
                        Direction::Received,
                        recv_ts,
                        pkt.len() as u32,
                    ));
                }
            },
        );
        sleep_until(next);
    }
}

/// Spend the gap until the next send receiving: read each echo the moment it
/// arrives (so its timestamp is its arrival) until `deadline`, then sweep
/// whatever is already queued without blocking.
///
/// Reading one echo per send instead leaves a permanent backlog after a single
/// late echo (a stall longer than the read timeout): every later echo is then
/// read — and timestamped — one or more send periods after it arrived, so RTT
/// comes out as true RTT + k × period.
fn recv_until<X>(
    socket: &UdpSocket,
    deadline: Instant,
    buf: &mut [u8],
    mut recv: impl FnMut(&mut [u8]) -> std::io::Result<(usize, X)>,
    mut on_recv: impl FnMut(&[u8], X, Timestamp),
) {
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        // A zero read timeout would block forever, so once due, only sweep.
        if wait < Duration::from_micros(100) {
            if socket.set_nonblocking(true).is_ok() {
                while let Ok((n, x)) = recv(buf) {
                    on_recv(&buf[..n], x, Timestamp::now());
                }
                let _ = socket.set_nonblocking(false);
            }
            return;
        }
        if socket.set_read_timeout(Some(wait)).is_err() {
            return;
        }
        match recv(buf) {
            Ok((n, x)) => {
                let ts = Timestamp::now();
                on_recv(&buf[..n], x, ts);
            }
            // timed out at the deadline, or an immediate error (e.g. ICMP
            // refused during an outage) — the caller sleeps out the remainder
            Err(_) => return,
        }
    }
}

fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if deadline > now {
        thread::sleep(deadline - now);
    }
}

/// Realtime ToServer: UE sends the fixed-rate padded stream. Logs one SENT
/// row per attempt, tagged with its outcome, the syscall time and the bytes
/// still queued below the socket. With `sndbuf_bytes` the socket is small and
/// non-blocking: a queue the modem does not drain fills it and shows as EAGAIN.
fn realtime_send(
    mut socket: UdpSocket,
    mut reopen: Reopener,
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
    let setup = |socket: &UdpSocket| {
        if let Some(bytes) = flow.sndbuf_bytes {
            if udp_timing::limit_send_buffer(socket.as_raw_fd(), bytes).is_ok() {
                let _ = socket.set_nonblocking(true);
            }
        }
    };
    setup(&socket);
    let mut fd = socket.as_raw_fd();
    let mut port = local_port(&socket);
    let mut seq = 0u64;
    let _ = udp_timing::raise_thread_priority();
    let mut pacer = Pacer::new(period);

    while !stop.load(Ordering::Acquire) {
        let ts = Timestamp::now();
        let bytes = RealtimePacket {
            flow_id: flow.flow_id,
            sequence_number: seq,
            send_mono_ns: ts.mono_ns,
            send_utc_ns: ts.utc_ns,
        }
        .to_bytes_padded(size);
        let t0 = Instant::now();
        let result = socket.send(&bytes);
        let call_ns = t0.elapsed().as_nanos().min(u32::MAX as u128) as u32;
        let status = SendStatus::of(&result);
        if status == SendStatus::Ok {
            counters.sent.fetch_add(1, Ordering::Relaxed);
            counters
                .bytes_sent
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        }
        sink.push(PacketRow {
            flow_id: flow.flow_id,
            sequence_number: seq,
            direction: Direction::Sent,
            timestamp: ts,
            payload_size: bytes.len() as u32,
            protocol: Protocol::Udp,
            local_port: port,
            remote_port: None,
            sender: None,
            expected_interval_ns: Some(period_ns),
            actual_interval_ns: None,
            dscp_sent: None,
            dscp_recv: None,
            timing: PacketTiming {
                send_status: Some(status),
                send_call_ns: Some(call_ns),
                sock_outq_bytes: udp_timing::outq_bytes(fd),
                ..PacketTiming::default()
            },
        });
        seq += 1;
        let now = Instant::now();
        if reopen.note(status == SendStatus::Error, now) {
            if let Some(s) = reopen.reopen(now) {
                setup(&s);
                socket = s;
                fd = socket.as_raw_fd();
                port = local_port(&socket);
            }
        }
        pacer.wait();
    }
}

/// Realtime FromServer: UE sends ~1 Hz keepalives (NAT punch-through) and
/// receives the downlink, logging RECEIVED with sender timestamps + interval.
fn realtime_recv(
    mut socket: UdpSocket,
    mut reopen: Reopener,
    mut sink: LogSink,
    flow: UeFlow,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) {
    let period_ns = rate_period(flow.rate_pps).as_nanos() as u64;
    let mut buf = [0u8; 2048];
    let mut last_keepalive = Instant::now() - KEEPALIVE_PERIOD; // send one immediately
    let mut last_recv_mono: Option<u64> = None;
    let mut fd = socket.as_raw_fd();
    let _ = udp_timing::enable_rx_timestamps(fd);
    let mut port = local_port(&socket);

    while !stop.load(Ordering::Acquire) {
        if last_keepalive.elapsed() >= KEEPALIVE_PERIOD {
            let failed = socket.send(b"mp-keepalive").is_err();
            let now = Instant::now();
            // retry a failed keepalive every RECV_TIMEOUT, not once a second,
            // so a dead network is noticed within REOPEN_AFTER
            last_keepalive = if failed { now - KEEPALIVE_PERIOD + RECV_TIMEOUT } else { now };
            if reopen.note(failed, now) {
                if let Some(s) = reopen.reopen(now) {
                    socket = s;
                    fd = socket.as_raw_fd();
                    let _ = udp_timing::enable_rx_timestamps(fd);
                    port = local_port(&socket);
                    last_keepalive = now - KEEPALIVE_PERIOD; // announce the new address at once
                }
            }
        }
        match udp_timing::recv_stamped(fd, &mut buf) {
            Ok(udp_timing::Received { n, kernel_utc_ns, .. }) => {
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
                    local_port: port,
                    remote_port: None,
                    sender,
                    expected_interval_ns: Some(period_ns),
                    actual_interval_ns: actual,
                    dscp_sent: None,
                    dscp_recv: None,
                    timing: PacketTiming {
                        kernel_rx_utc_ns: kernel_utc_ns,
                        ..PacketTiming::default()
                    },
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
    let (chunk, mut pacer) = match flow.rate_kbps {
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

// ---------------------------------------------------------------------------
// DSCP probe (UE side)
// ---------------------------------------------------------------------------

fn enable_ip_recvtos(fd: std::os::fd::RawFd) {
    let one: libc::c_int = 1;
    unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_IP,
            libc::IP_RECVTOS,
            &one as *const libc::c_int as *const libc::c_void,
            mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}

fn set_ip_tos(fd: std::os::fd::RawFd, tos: u8) {
    let val: libc::c_int = tos as libc::c_int;
    unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_IP,
            libc::IP_TOS,
            &val as *const libc::c_int as *const libc::c_void,
            mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}

/// `recvmsg` on a connected socket: returns `(bytes_read, tos_byte)`.
/// Respects the socket's `SO_RCVTIMEO` (set via `set_read_timeout`).
fn recv_with_tos_connected(fd: std::os::fd::RawFd, buf: &mut [u8]) -> std::io::Result<(usize, Option<u8>)> {
    let cmsg_capacity =
        unsafe { libc::CMSG_SPACE(mem::size_of::<libc::c_int>() as u32) as usize };
    let mut cmsg_buf = vec![0u8; cmsg_capacity];
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: buf.len(),
    };
    let mut msg = libc::msghdr {
        msg_name: std::ptr::null_mut(),
        msg_namelen: 0,
        msg_iov: &mut iov,
        msg_iovlen: 1,
        msg_control: cmsg_buf.as_mut_ptr() as *mut libc::c_void,
        msg_controllen: cmsg_buf.len() as _,
        msg_flags: 0,
    };
    let n = unsafe { libc::recvmsg(fd, &mut msg, 0) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut tos_byte: Option<u8> = None;
    let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    while !cmsg.is_null() {
        let (level, typ) = unsafe { ((*cmsg).cmsg_level, (*cmsg).cmsg_type) };
        if level == libc::IPPROTO_IP && typ == libc::IP_TOS {
            tos_byte = Some(unsafe { *libc::CMSG_DATA(cmsg) });
        }
        cmsg = unsafe { libc::CMSG_NXTHDR(&msg, cmsg) };
    }
    Ok((n as usize, tos_byte))
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
    enable_ip_recvtos(fd);

    let mut recv_buf = [0u8; 2048];
    let mut seq = 1u64;
    let mut next = Instant::now();

    while !stop.load(Ordering::Acquire) {
        let dscp_intended_ul = dscp_values[((seq - 1) as usize) % dscp_values.len()];

        set_ip_tos(fd, dscp_intended_ul << 2);

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
            counters.sent.fetch_add(1, Ordering::Relaxed);
            counters.bytes_sent.fetch_add(bytes.len() as u64, Ordering::Relaxed);
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
                expected_interval_ns: None,
                actual_interval_ns: None,
                dscp_sent: Some(dscp_intended_ul),
                dscp_recv: None,
                timing: PacketTiming::default(),
            });
        }

        seq += 1;
        next = (next + period).max(Instant::now());
        recv_until(
            &socket,
            next,
            &mut recv_buf,
            |buf| recv_with_tos_connected(fd, buf),
            |pkt, tos_byte, recv_ts| {
                let n = pkt.len();
                if let Ok(echo) = DscpProbePacket::decode(pkt) {
                    counters.received.fetch_add(1, Ordering::Relaxed);
                    counters.bytes_received.fetch_add(n as u64, Ordering::Relaxed);
                    let dscp_actual_dl = tos_byte.map(|t| t >> 2).unwrap_or(DSCP_UNKNOWN);
                    sink.push(PacketRow {
                        flow_id: flow.flow_id,
                        sequence_number: echo.sequence_number,
                        direction: Direction::Received,
                        timestamp: recv_ts,
                        payload_size: n as u32,
                        protocol: Protocol::Udp,
                        local_port: None,
                        remote_port: None,
                        sender: None,
                        expected_interval_ns: None,
                        actual_interval_ns: None,
                        dscp_sent: Some(echo.dscp_intended_dl),
                        dscp_recv: Some(dscp_actual_dl),
                        timing: PacketTiming::default(),
                    });
                }
            },
        );
        sleep_until(next);
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
        timing: PacketTiming::default(),
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

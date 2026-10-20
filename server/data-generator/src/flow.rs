//! Server-side flow threads, one per flow, driven by the control API.
//!
//! The UE always originates (invariant 5): the server echoes probes, learns
//! the UE's address from its first datagram or connection, and then runs the
//! same endpoint loops as the UE (`fieldtest_core::endpoint`) — only the
//! socket glue differs. Hot-path order per `contracts/internal/logger_sink.md`:
//! receive → timestamp → push, then timestamp → send → push; the writer
//! thread does Parquet off the hot path.

use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fieldtest_core::app_traffic::{self, activity_name};
pub use fieldtest_core::endpoint::Counters;
use fieldtest_core::endpoint::{self, would_block, RealtimeStream, BULK_CHUNK};
use fieldtest_core::log_writer::{Direction, LogSink, PacketRow, PacketTiming, WriterHandle};
use fieldtest_core::packet::{AppExchange, AppRequest, DscpProbePacket, ProbePacket, DSCP_UNKNOWN};
use fieldtest_core::timestamp::Timestamp;
use fieldtest_core::udp_timing::{self, SendStatus};
use mp_contracts::Direction as FlowDir;
use mp_contracts::GeneratorState;

const ACCEPT_POLL: Duration = Duration::from_millis(50);
const IO_TIMEOUT: Duration = Duration::from_millis(200);

/// Spawn the echo loop. `socket` must already be bound with a read timeout so
/// the loop can observe `stop`. Transitions state READY → WAITING_FOR_CLIENT →
/// RUNNING (first packet) → STOPPING → STOPPED.
#[allow(clippy::too_many_arguments)]
pub fn spawn_probe_echo(
    socket: UdpSocket,
    mut sink: LogSink,
    writer: WriterHandle,
    flow_id: u32,
    state: Arc<Mutex<GeneratorState>>,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        *state.lock().unwrap() = GeneratorState::WaitingForClient;
        let mut buf = [0u8; 2048];
        let mut seen_client = false;
        let fd = socket.as_raw_fd();
        let _ = udp_timing::enable_rx_timestamps(fd);

        while !stop.load(Ordering::Acquire) {
            // timeout (re-check stop), a transient error, or no source to echo to
            let Ok(udp_timing::Received {
                n,
                from: Some(peer),
                kernel_utc_ns,
                ..
            }) = udp_timing::recv_stamped(fd, &mut buf)
            else {
                continue;
            };
            let recv_ts = Timestamp::now();
            if !seen_client {
                seen_client = true;
                counters.note_peer(peer);
                *state.lock().unwrap() = GeneratorState::Running;
            }
            counters.received(n as u64);
            let seq = ProbePacket::decode(&buf[..n])
                .map(|p| p.sequence_number)
                .unwrap_or(0);
            sink.push(PacketRow {
                timing: PacketTiming {
                    kernel_rx_utc_ns: kernel_utc_ns,
                    ..PacketTiming::default()
                },
                ..PacketRow::probe(flow_id, seq, Direction::Received, recv_ts, n as u32)
            });

            // echo verbatim
            let send_ts = Timestamp::now();
            if socket.send_to(&buf[..n], peer).is_ok() {
                counters.sent(n as u64);
                sink.push(PacketRow::probe(
                    flow_id,
                    seq,
                    Direction::Sent,
                    send_ts,
                    n as u32,
                ));
            }
        }
        finalize(&state, sink, writer);
    })
}

/// DSCP probe echo: reads the TOS byte each `DscpProbePacket` arrived with,
/// fills in `dscp_actual_ul` and `dscp_intended_dl`, marks the reply with
/// `IP_TOS` and echoes it back to the UE.
///
/// Log schema (packets.parquet):
/// - RECEIVED row: `dscp_sent = dscp_intended_ul`, `dscp_recv = dscp_actual_ul`
/// - SENT echo row: `dscp_sent = dscp_intended_dl`, `dscp_recv = None`
#[allow(clippy::too_many_arguments)]
pub fn spawn_dscp_probe_echo(
    socket: UdpSocket,
    mut sink: LogSink,
    writer: WriterHandle,
    flow_id: u32,
    state: Arc<Mutex<GeneratorState>>,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let fd = socket.as_raw_fd();
        let _ = udp_timing::enable_recv_tos(fd);

        *state.lock().unwrap() = GeneratorState::WaitingForClient;
        let mut buf = [0u8; 2048];
        let mut seen_client = false;

        while !stop.load(Ordering::Acquire) {
            let Ok(udp_timing::Received {
                n,
                from: Some(peer),
                tos,
                ..
            }) = udp_timing::recv_stamped(fd, &mut buf)
            else {
                continue;
            };
            let recv_ts = Timestamp::now();
            if !seen_client {
                seen_client = true;
                counters.note_peer(peer);
                *state.lock().unwrap() = GeneratorState::Running;
            }
            counters.received(n as u64);

            // Skip silently what doesn't parse.
            let Ok(pkt) = DscpProbePacket::decode(&buf[..n]) else {
                continue;
            };
            // dscp_actual_ul = DSCP bits from received IP TOS (top 6 bits).
            let dscp_actual_ul = tos.map(|t| t >> 2).unwrap_or(DSCP_UNKNOWN);
            sink.push(PacketRow {
                dscp_sent: Some(pkt.dscp_intended_ul),
                dscp_recv: Some(dscp_actual_ul),
                ..PacketRow::probe(
                    flow_id,
                    pkt.sequence_number,
                    Direction::Received,
                    recv_ts,
                    n as u32,
                )
            });

            // Echo strategy: mirror the UE's intended DSCP for the downlink direction.
            let dscp_intended_dl = pkt.dscp_intended_ul;
            let echo = DscpProbePacket {
                dscp_actual_ul,
                dscp_intended_dl,
                ..pkt
            }
            .to_bytes();
            let _ = udp_timing::set_tos(fd, dscp_intended_dl << 2);

            let send_ts = Timestamp::now();
            if socket.send_to(&echo, peer).is_ok() {
                counters.sent(echo.len() as u64);
                sink.push(PacketRow {
                    dscp_sent: Some(dscp_intended_dl),
                    ..PacketRow::probe(
                        flow_id,
                        pkt.sequence_number,
                        Direction::Sent,
                        send_ts,
                        echo.len() as u32,
                    )
                });
            }
        }
        finalize(&state, sink, writer);
    })
}

/// How often a FromServer realtime sender looks for keepalives from a new
/// UE address.
const PEER_CHECK: Duration = Duration::from_millis(50);

/// Source of the newest datagram waiting on `socket`, draining the queue
/// without blocking. `stay_nonblocking` keeps the socket non-blocking after.
fn latest_sender(socket: &UdpSocket, buf: &mut [u8], stay_nonblocking: bool) -> Option<SocketAddr> {
    if socket.set_nonblocking(true).is_err() {
        return None;
    }
    let mut last = None;
    while let Ok((_, addr)) = socket.recv_from(buf) {
        last = Some(addr);
    }
    if !stay_nonblocking {
        let _ = socket.set_nonblocking(false);
    }
    last
}

/// The server's realtime sender socket: sends to the UE's address and follows
/// it to a new one — after a Wi-Fi <-> mobile switch the UE reopens its
/// socket and its keepalives arrive from there. Logged as remote_port.
struct ServerOut {
    socket: UdpSocket,
    peer: SocketAddr,
    nonblocking: bool,
    last_check: Instant,
    buf: [u8; 2048],
}

impl endpoint::DatagramOut for ServerOut {
    fn socket(&self) -> &UdpSocket {
        &self.socket
    }

    fn send(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.socket.send_to(buf, self.peer)
    }

    fn ports(&self) -> (Option<u32>, Option<u32>) {
        (None, Some(self.peer.port() as u32))
    }

    fn before_send(&mut self) {
        if self.last_check.elapsed() >= PEER_CHECK {
            if let Some(addr) = latest_sender(&self.socket, &mut self.buf, self.nonblocking) {
                self.peer = addr;
            }
            self.last_check = Instant::now();
        }
    }

    fn after_send(&mut self, _status: SendStatus) {}
}

/// The server's realtime receiver socket: the first datagram names the UE.
struct ServerIn<'a> {
    fd: RawFd,
    started: bool,
    state: &'a Mutex<GeneratorState>,
    counters: &'a Counters,
}

impl endpoint::DatagramIn for ServerIn<'_> {
    fn fd(&self) -> RawFd {
        self.fd
    }

    fn local_port(&self) -> Option<u32> {
        None
    }

    fn on_datagram(&mut self, from: Option<SocketAddr>) {
        if !self.started {
            self.started = true;
            if let Some(addr) = from {
                self.counters.note_peer(addr);
            }
            *self.state.lock().unwrap() = GeneratorState::Running;
        }
    }
}

/// Realtime UDP flow. FromServer: learn the peer from the UE's keepalive, then
/// send a fixed-rate padded stream (log SENT). ToServer: receive the UE's
/// stream, logging RECEIVED with the embedded sender timestamps + measured
/// inter-arrival (the OWD / jitter inputs).
#[allow(clippy::too_many_arguments)]
pub fn spawn_realtime_udp(
    socket: UdpSocket,
    mut sink: LogSink,
    writer: WriterHandle,
    flow_id: u32,
    direction: FlowDir,
    rate_pps: u32,
    packet_size: usize,
    sndbuf_bytes: Option<u32>,
    state: Arc<Mutex<GeneratorState>>,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        *state.lock().unwrap() = GeneratorState::WaitingForClient;
        let period_ns = 1_000_000_000u64 / rate_pps.max(1) as u64;

        match direction {
            FlowDir::FromServer => {
                // wait for the first keepalive to learn the UE address
                let mut buf = [0u8; 2048];
                let peer = loop {
                    if stop.load(Ordering::Acquire) {
                        finalize(&state, sink, writer);
                        return;
                    }
                    if let Ok((_, addr)) = socket.recv_from(&mut buf) {
                        counters.note_peer(addr);
                        break addr;
                    }
                };
                *state.lock().unwrap() = GeneratorState::Running;
                let nonblocking = endpoint::back_pressure_mode(&socket, sndbuf_bytes);
                let mut out = ServerOut {
                    socket,
                    peer,
                    nonblocking,
                    last_check: Instant::now(),
                    buf,
                };
                let stream = RealtimeStream {
                    flow_id,
                    period: Duration::from_nanos(period_ns),
                    size: packet_size,
                };
                endpoint::realtime_send(&mut out, stream, &mut sink, &counters, &stop);
            }
            FlowDir::ToServer => {
                let mut inp = ServerIn {
                    fd: socket.as_raw_fd(),
                    started: false,
                    state: &state,
                    counters: &counters,
                };
                endpoint::realtime_recv(&mut inp, flow_id, period_ns, &mut sink, &counters, &stop);
            }
        }
        finalize(&state, sink, writer);
    })
}

/// Bulk TCP flow. The UE connects (invariant 5). FromServer: stream bytes to
/// the UE; ToServer: drain bytes from the UE. One PacketRow per application
/// chunk (TCP hides segmentation) — throughput = sum(payload_size)/time.
#[allow(clippy::too_many_arguments)]
pub fn spawn_bulk_tcp(
    listener: TcpListener,
    mut sink: LogSink,
    writer: WriterHandle,
    flow_id: u32,
    direction: FlowDir,
    target_bytes: Option<u64>,
    rate_kbps: Option<u32>,
    state: Arc<Mutex<GeneratorState>>,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        *state.lock().unwrap() = GeneratorState::WaitingForClient;
        listener.set_nonblocking(true).ok();

        // accept the UE connection, honoring stop
        let mut stream = loop {
            if stop.load(Ordering::Acquire) {
                finalize(&state, sink, writer);
                return;
            }
            match listener.accept() {
                Ok((s, addr)) => {
                    counters.note_peer(addr);
                    break s;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(ACCEPT_POLL);
                    continue;
                }
                Err(_) => {
                    finalize(&state, sink, writer);
                    return;
                }
            }
        };
        stream.set_nonblocking(false).ok();
        stream.set_read_timeout(Some(IO_TIMEOUT)).ok();
        stream.set_write_timeout(Some(IO_TIMEOUT)).ok();
        *state.lock().unwrap() = GeneratorState::Running;

        match direction {
            FlowDir::FromServer => endpoint::bulk_send(
                &mut stream,
                flow_id,
                rate_kbps,
                target_bytes,
                &mut sink,
                &counters,
                &stop,
            ),
            FlowDir::ToServer => {
                endpoint::bulk_recv(&mut stream, flow_id, &mut sink, &counters, &stop)
            }
        }
        finalize(&state, sink, writer);
    })
}

/// app_traffic object server: accepts any number of UE connections and serves
/// object requests on each (`AppExchange`, or a v1 `AppRequest`): it reads the
/// upload, waits the think time, then sends the response (paced when asked) —
/// or, with no response, acknowledges by echoing the request. Rows carry the
/// object id, activity and sizes, like the UE's (see core `app_traffic`).
pub fn spawn_app_server(
    listener: TcpListener,
    sink: LogSink,
    writer: WriterHandle,
    flow_id: u32,
    state: Arc<Mutex<GeneratorState>>,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        *state.lock().unwrap() = GeneratorState::WaitingForClient;
        listener.set_nonblocking(true).ok();
        let sink = Mutex::new(sink);
        thread::scope(|s| {
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((conn, addr)) => {
                        counters.note_peer(addr);
                        *state.lock().unwrap() = GeneratorState::Running;
                        let (sink, counters, stop) = (&sink, &*counters, &*stop);
                        s.spawn(move || serve_app_connection(conn, flow_id, sink, counters, stop));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(ACCEPT_POLL)
                    }
                    Err(_) => thread::sleep(ACCEPT_POLL),
                }
            }
        });
        finalize(&state, sink.into_inner().unwrap(), writer);
    })
}

fn serve_app_connection(
    mut conn: TcpStream,
    flow_id: u32,
    sink: &Mutex<LogSink>,
    counters: &Counters,
    stop: &AtomicBool,
) {
    let _ = conn.set_nonblocking(false);
    let _ = conn.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = conn.set_write_timeout(Some(Duration::from_millis(200)));
    let _ = conn.set_nodelay(true);
    let push = |dir: Direction, size: usize, x: &AppExchange, v1: bool| {
        let row = PacketRow {
            timing: PacketTiming {
                app_activity: Some(activity_name(x.activity)),
                app_object_bytes: Some(if v1 {
                    x.down_bytes.max(x.up_bytes)
                } else {
                    x.down_bytes
                }),
                app_up_bytes: (!v1).then_some(x.up_bytes),
                ..PacketTiming::default()
            },
            ..PacketRow::tcp(flow_id, x.object_id, dir, Timestamp::now(), size as u32)
        };
        sink.lock().unwrap().push(row);
    };
    let sent = |n: usize| counters.sent(n as u64);
    let received = |n: usize| counters.received(n as u64);
    let mut head = [0u8; AppExchange::WIRE_LEN];
    let mut buf = vec![0u8; BULK_CHUNK];
    // a connection may sit idle between bursts for as long as the UE keeps it
    let forever = Instant::now() + Duration::from_secs(365 * 86_400);
    while !stop.load(Ordering::Acquire) {
        // pkt_t + ver say which request layout follows
        if app_traffic::read_exact_until(&mut conn, &mut head[..2], stop, forever).is_err() {
            return;
        }
        let len = if head[1] == fieldtest_core::packet::VER_1 {
            AppRequest::WIRE_LEN
        } else {
            AppExchange::WIRE_LEN
        };
        if app_traffic::read_exact_until(&mut conn, &mut head[2..len], stop, forever).is_err() {
            return;
        }
        let (x, v1) = match (
            AppRequest::decode(&head[..len]),
            AppExchange::decode(&head[..len]),
        ) {
            (Ok(r), _) => (AppExchange::from_v1(&r), true),
            (_, Ok(x)) => (x, false),
            _ => return, // not our protocol
        };
        let head = &head[..len];
        push(Direction::Received, len, &x, v1);
        received(len);
        let server_time = x.delay_ms as f64 / 1e3
            + if x.rate_bps > 0 {
                x.down_bytes as f64 * 8.0 / x.rate_bps as f64
            } else {
                0.0
            };
        let deadline =
            Instant::now() + Duration::from_secs(120) + Duration::from_secs_f64(server_time);
        let mut left = x.up_bytes;
        while left > 0 {
            let want = left.min(BULK_CHUNK as u64) as usize;
            match conn.read(&mut buf[..want]) {
                Ok(0) => return,
                Ok(n) => {
                    push(Direction::Received, n, &x, v1);
                    received(n);
                    left -= n as u64;
                }
                Err(ref e) if would_block(e) => {
                    if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
        if x.down_bytes == 0 {
            // acknowledge the upload: echo the request
            if app_traffic::write_all_until(&mut conn, head, stop, deadline).is_err() {
                return;
            }
            push(Direction::Sent, head.len(), &x, v1);
            sent(head.len());
            continue;
        }
        // think time (an LLM's time to first token, a tool's latency)
        let start = Instant::now() + Duration::from_millis(x.delay_ms as u64);
        while Instant::now() < start {
            if stop.load(Ordering::Acquire) {
                return;
            }
            thread::sleep((start - Instant::now()).min(Duration::from_millis(50)));
        }
        // Paced (a token stream): the reply is generated at rate_bps whatever
        // the network does — what a stall holds back goes out in one write
        // when it clears. Else as fast as TCP takes it.
        let chunk = match (x.rate_bps, x.chunk_bytes) {
            (0, _) => BULK_CHUNK as u64,
            (bps, 0) => udp_timing::bulk_pacing(bps.div_ceil(1000), BULK_CHUNK).0 as u64,
            (_, c) => c as u64,
        };
        let mut done = 0u64;
        while done < x.down_bytes {
            let due = if x.rate_bps == 0 {
                x.down_bytes
            } else {
                let generated = start.elapsed().as_secs_f64() * x.rate_bps as f64 / 8.0;
                // whole chunks only, and the first one at once
                ((generated as u64 / chunk + 1) * chunk).min(x.down_bytes)
            };
            if due <= done {
                // chunk k is due k chunks' worth of time after the start
                let next = start
                    + Duration::from_secs_f64(
                        (done / chunk * chunk) as f64 * 8.0 / x.rate_bps as f64,
                    );
                thread::sleep(
                    next.saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(50)),
                );
                if stop.load(Ordering::Acquire) {
                    return;
                }
                continue;
            }
            let n = (due - done).min(BULK_CHUNK as u64) as usize;
            if app_traffic::write_all_until(&mut conn, &buf[..n], stop, deadline).is_err() {
                return;
            }
            push(Direction::Sent, n, &x, v1);
            sent(n);
            done += n as u64;
        }
    }
}

fn finalize(state: &Arc<Mutex<GeneratorState>>, sink: LogSink, writer: WriterHandle) {
    *state.lock().unwrap() = GeneratorState::Stopping;
    drop(sink); // close the producer so the writer can drain + finish
    writer.shutdown();
    *state.lock().unwrap() = GeneratorState::Stopped;
}

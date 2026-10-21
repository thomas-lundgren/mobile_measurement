//! Server-side flow threads, one per flow, driven by the control API.
//!
//! The UE always originates (invariant 5): the server echoes probes, learns
//! the UE's address from its first datagram or connection, and then runs the
//! same endpoint loops as the UE (`fieldtest_core::endpoint`) — only the
//! socket glue differs. Hot-path order per `contracts/internal/logger_sink.md`:
//! receive → timestamp → push, then timestamp → send → push; the writer
//! thread does Parquet off the hot path.

use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fieldtest_core::burst;
pub use fieldtest_core::endpoint::Counters;
use fieldtest_core::endpoint::{self, RealtimeStream};
use fieldtest_core::exchange;
use fieldtest_core::log_writer::{Direction, LogSink, PacketRow, PacketTiming, WriterHandle};
use fieldtest_core::packet::{DscpProbePacket, ProbePacket, DSCP_UNKNOWN};
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

/// app_traffic object server: accepts any number of UE connections and runs the
/// request/response engine's server side on each (`fieldtest_core::exchange`):
/// read the upload, wait the think time, send the response (paced when asked)
/// or acknowledge. On the same port over UDP it serves bursts
/// (`fieldtest_core::burst`). Rows carry the object id, activity and sizes,
/// like the UE's.
#[allow(clippy::too_many_arguments)]
pub fn spawn_app_server(
    listener: TcpListener,
    udp: UdpSocket,
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
            let (sink_b, counters_b, stop_b) = (&sink, &*counters, &*stop);
            s.spawn(move || {
                burst::serve(&udp, stop_b, &|io, activity| {
                    sink_b
                        .lock()
                        .unwrap()
                        .push(PacketRow::burst(flow_id, activity, &io));
                    match io.direction {
                        Direction::Sent => counters_b.sent(io.size as u64),
                        Direction::Received => counters_b.received(io.size as u64),
                    }
                })
            });
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((conn, addr)) => {
                        counters.note_peer(addr);
                        *state.lock().unwrap() = GeneratorState::Running;
                        let (sink, counters, stop) = (&sink, &*counters, &*stop);
                        s.spawn(move || {
                            exchange::serve(conn, stop, |dir, n, x, v1| {
                                sink.lock()
                                    .unwrap()
                                    .push(PacketRow::exchange(flow_id, x, v1, dir, n));
                                match dir {
                                    Direction::Sent => counters.sent(n as u64),
                                    Direction::Received => counters.received(n as u64),
                                }
                            })
                        });
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

fn finalize(state: &Arc<Mutex<GeneratorState>>, sink: LogSink, writer: WriterHandle) {
    *state.lock().unwrap() = GeneratorState::Stopping;
    drop(sink); // close the producer so the writer can drain + finish
    writer.shutdown();
    *state.lock().unwrap() = GeneratorState::Stopped;
}

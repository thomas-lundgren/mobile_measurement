//! Probe echo flow — the server-side real-time thread for a `probe` flow.
//!
//! The server hosts the echo (invariant 5: the UE originates). Hot-path order
//! per `contracts/internal/logger_sink.md`: receive → timestamp → push, then
//! timestamp → send → push for the echo. Logging is the wait-free SPSC handoff;
//! the writer thread does Parquet off the hot path.

use std::io::{self, Read, Write};
use std::mem;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, TcpListener, TcpStream,
               UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fieldtest_core::log_writer::{Direction, LogSink, PacketRow, PacketTiming, Protocol, WriterHandle};
use fieldtest_core::udp_timing::{self, Pacer, SendStatus};
use fieldtest_core::app_traffic::{self, activity_name};
use fieldtest_core::packet::{AppRequest, DscpProbePacket, ProbePacket, RealtimePacket, DSCP_UNKNOWN};
use fieldtest_core::timestamp::Timestamp;
use mp_contracts::Direction as FlowDir;
use mp_contracts::GeneratorState;

const BULK_CHUNK: usize = 64 * 1024;
const ACCEPT_POLL: Duration = Duration::from_millis(50);

#[derive(Default)]
pub struct Counters {
    pub sent: AtomicU64,
    pub received: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub bytes_received: AtomicU64,
    /// The UE's address as first seen on this flow (after NAT): which network
    /// the test really ran over. Written as a CLIENT_CONNECTED event at stop.
    pub peer: Mutex<Option<(Timestamp, SocketAddr)>>,
}

impl Counters {
    /// Record the UE's address on first contact (later calls are no-ops).
    pub fn note_peer(&self, peer: SocketAddr) {
        let mut p = self.peer.lock().unwrap();
        if p.is_none() {
            *p = Some((Timestamp::now(), peer));
        }
    }
}

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

        while !stop.load(Ordering::Acquire) {
            match socket.recv_from(&mut buf) {
                Ok((n, peer)) => {
                    let recv_ts = Timestamp::now();
                    if !seen_client {
                        seen_client = true;
                        counters.note_peer(peer);
                        *state.lock().unwrap() = GeneratorState::Running;
                    }
                    counters.received.fetch_add(1, Ordering::Relaxed);
                    counters
                        .bytes_received
                        .fetch_add(n as u64, Ordering::Relaxed);
                    let seq = ProbePacket::decode(&buf[..n])
                        .map(|p| p.sequence_number)
                        .unwrap_or(0);
                    sink.push(PacketRow::probe(
                        flow_id,
                        seq,
                        Direction::Received,
                        recv_ts,
                        n as u32,
                    ));

                    // echo verbatim
                    let send_ts = Timestamp::now();
                    if socket.send_to(&buf[..n], peer).is_ok() {
                        counters.sent.fetch_add(1, Ordering::Relaxed);
                        counters.bytes_sent.fetch_add(n as u64, Ordering::Relaxed);
                        sink.push(PacketRow::probe(
                            flow_id,
                            seq,
                            Direction::Sent,
                            send_ts,
                            n as u32,
                        ));
                    }
                }
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    // read timeout — loop to re-check stop
                }
                Err(_) => { /* transient; keep serving */ }
            }
        }

        *state.lock().unwrap() = GeneratorState::Stopping;
        drop(sink); // close the producer so the writer can drain + finish
        writer.shutdown();
        *state.lock().unwrap() = GeneratorState::Stopped;
    })
}

// ---------------------------------------------------------------------------
// IP_RECVTOS helpers (Linux IPv4 only — the server binds 0.0.0.0)
// ---------------------------------------------------------------------------

/// Enable `IP_RECVTOS` on a UDP socket so `recv_with_tos` can read the TOS
/// byte from incoming packets' IP headers via ancillary data.
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

/// Set `IP_TOS` on the socket before a send so the outgoing packet carries
/// `tos` in its IP header. `tos = dscp << 2` (ECN bits left zero).
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

struct RecvMsg {
    n: usize,
    peer: SocketAddr,
    /// Raw TOS byte from the IP header. `None` if `IP_RECVTOS` returned no
    /// control message (shouldn't happen once the option is enabled).
    tos_byte: Option<u8>,
}

/// `recvmsg(2)` wrapper that extracts both the sender address and the IP TOS
/// byte (delivered via `IP_RECVTOS` ancillary data). The read timeout set on
/// the socket still fires, returning `WouldBlock` / `TimedOut`.
fn recv_with_tos(fd: std::os::fd::RawFd, buf: &mut [u8]) -> io::Result<RecvMsg> {
    // Space for one IP_TOS control message. The kernel delivers TOS as a
    // single byte but `CMSG_SPACE` rounds up to the alignment requirement.
    let cmsg_capacity =
        unsafe { libc::CMSG_SPACE(mem::size_of::<libc::c_int>() as u32) as usize };
    let mut cmsg_buf = vec![0u8; cmsg_capacity];

    let mut addr: libc::sockaddr_storage = unsafe { mem::zeroed() };
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: buf.len(),
    };
    let mut msg = libc::msghdr {
        msg_name: &mut addr as *mut _ as *mut libc::c_void,
        msg_namelen: mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t,
        msg_iov: &mut iov,
        msg_iovlen: 1,
        msg_control: cmsg_buf.as_mut_ptr() as *mut libc::c_void,
        msg_controllen: cmsg_buf.len() as _,
        msg_flags: 0,
    };

    let n = unsafe { libc::recvmsg(fd, &mut msg, 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }

    let peer = sockaddr_storage_to_addr(&addr, msg.msg_namelen)?;

    // Walk the control-message chain looking for IP_TOS.
    let mut tos_byte: Option<u8> = None;
    let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    while !cmsg.is_null() {
        // SAFETY: `cmsg` is a valid pointer within `cmsg_buf` while `msg` lives.
        let (level, typ) = unsafe { ((*cmsg).cmsg_level, (*cmsg).cmsg_type) };
        if level == libc::IPPROTO_IP && typ == libc::IP_TOS {
            // The kernel puts a single TOS byte in the data area.
            // SAFETY: CMSG_DATA returns a pointer inside cmsg_buf.
            tos_byte = Some(unsafe { *libc::CMSG_DATA(cmsg) });
        }
        cmsg = unsafe { libc::CMSG_NXTHDR(&msg, cmsg) };
    }

    Ok(RecvMsg { n: n as usize, peer, tos_byte })
}

fn sockaddr_storage_to_addr(
    addr: &libc::sockaddr_storage,
    _len: libc::socklen_t,
) -> io::Result<SocketAddr> {
    match addr.ss_family as libc::c_int {
        libc::AF_INET => {
            // SAFETY: ss_family == AF_INET guarantees this cast is valid.
            let a = unsafe { &*(addr as *const _ as *const libc::sockaddr_in) };
            let ip = Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr));
            Ok(SocketAddr::V4(SocketAddrV4::new(ip, u16::from_be(a.sin_port))))
        }
        libc::AF_INET6 => {
            let a = unsafe { &*(addr as *const _ as *const libc::sockaddr_in6) };
            let ip = Ipv6Addr::from(a.sin6_addr.s6_addr);
            Ok(SocketAddr::V6(SocketAddrV6::new(
                ip,
                u16::from_be(a.sin6_port),
                a.sin6_flowinfo,
                a.sin6_scope_id,
            )))
        }
        _ => Err(io::Error::new(io::ErrorKind::Other, "unexpected address family")),
    }
}

/// DSCP probe echo: reads incoming `DscpProbePacket`s via `recvmsg` to capture
/// the TOS byte from the IP header, fills in `dscp_actual_ul` and
/// `dscp_intended_dl` in the echo packet, sets `IP_TOS` on the reply socket,
/// and echoes the modified packet back to the UE.
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
        enable_ip_recvtos(fd);

        *state.lock().unwrap() = GeneratorState::WaitingForClient;
        let mut buf = [0u8; 2048];
        let mut seen_client = false;

        loop {
            if stop.load(Ordering::Acquire) { break; }

            let recv = match recv_with_tos(fd, &mut buf) {
                Ok(r) => r,
                Err(ref e) if would_block(e) => continue,
                Err(_) => continue,
            };
            let recv_ts = Timestamp::now();

            if !seen_client {
                seen_client = true;
                counters.note_peer(recv.peer);
                *state.lock().unwrap() = GeneratorState::Running;
            }
            counters.received.fetch_add(1, Ordering::Relaxed);
            counters.bytes_received.fetch_add(recv.n as u64, Ordering::Relaxed);

            // Decode the incoming packet. Skip silently if it doesn't parse.
            let pkt = match DscpProbePacket::decode(&buf[..recv.n]) {
                Ok(p) => p,
                Err(_) => continue,
            };

            // dscp_actual_ul = DSCP bits from received IP TOS (top 6 bits).
            let dscp_actual_ul = recv.tos_byte
                .map(|t| t >> 2)
                .unwrap_or(DSCP_UNKNOWN);

            // Log RECEIVED: dscp_sent = what UE intended, dscp_recv = what arrived.
            sink.push(PacketRow {
                flow_id,
                sequence_number: pkt.sequence_number,
                direction: Direction::Received,
                timestamp: recv_ts,
                payload_size: recv.n as u32,
                protocol: Protocol::Udp,
                local_port: None,
                remote_port: None,
                sender: None,
                expected_interval_ns: None,
                actual_interval_ns: None,
                dscp_sent: Some(pkt.dscp_intended_ul),
                dscp_recv: Some(dscp_actual_ul),
                timing: PacketTiming::default(),
            });

            // Echo strategy: mirror the UE's intended DSCP for the downlink direction.
            let dscp_intended_dl = pkt.dscp_intended_ul;

            // Build echo with updated DSCP fields.
            let echo = DscpProbePacket {
                dscp_actual_ul,
                dscp_intended_dl,
                ..pkt
            };
            let echo_bytes = echo.to_bytes();

            // Mark the outgoing IP header with dscp_intended_dl before sending.
            set_ip_tos(fd, dscp_intended_dl << 2);

            let send_ts = Timestamp::now();
            if socket.send_to(&echo_bytes, recv.peer).is_ok() {
                counters.sent.fetch_add(1, Ordering::Relaxed);
                counters.bytes_sent.fetch_add(echo_bytes.len() as u64, Ordering::Relaxed);
                sink.push(PacketRow {
                    flow_id,
                    sequence_number: pkt.sequence_number,
                    direction: Direction::Sent,
                    timestamp: send_ts,
                    payload_size: echo_bytes.len() as u32,
                    protocol: Protocol::Udp,
                    local_port: None,
                    remote_port: None,
                    sender: None,
                    expected_interval_ns: None,
                    actual_interval_ns: None,
                    dscp_sent: Some(dscp_intended_dl),
                    dscp_recv: None,
                    timing: PacketTiming::default(),
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
        let mut buf = [0u8; 2048];
        let fd = socket.as_raw_fd();

        match direction {
            FlowDir::FromServer => {
                // wait for the first keepalive to learn the UE address
                let peer = loop {
                    if stop.load(Ordering::Acquire) {
                        finalize(&state, sink, writer);
                        return;
                    }
                    match socket.recv_from(&mut buf) {
                        Ok((_, addr)) => {
                            counters.note_peer(addr);
                            break addr;
                        }
                        Err(ref e) if would_block(e) => continue,
                        Err(_) => continue,
                    }
                };
                *state.lock().unwrap() = GeneratorState::Running;

                // Same tagging as the UE sender (see udp_timing): one SENT row
                // per attempt; sndbuf_bytes makes the socket small + non-blocking.
                if let Some(bytes) = sndbuf_bytes {
                    if udp_timing::limit_send_buffer(fd, bytes).is_ok() {
                        let _ = socket.set_nonblocking(true);
                    }
                }
                let nonblocking = sndbuf_bytes.is_some();
                let mut peer = peer;
                let mut last_peer_check = Instant::now();
                let _ = udp_timing::raise_thread_priority();
                let mut pacer = Pacer::new(Duration::from_nanos(period_ns));
                let mut seq = 0u64;
                while !stop.load(Ordering::Acquire) {
                    // Follow the UE to a new address: after a Wi-Fi <-> mobile
                    // switch it reopens its socket and keepalives arrive from
                    // there. Logged as remote_port on the SENT rows.
                    if last_peer_check.elapsed() >= PEER_CHECK {
                        if let Some(addr) = latest_sender(&socket, &mut buf, nonblocking) {
                            peer = addr;
                        }
                        last_peer_check = Instant::now();
                    }
                    let ts = Timestamp::now();
                    let pkt = RealtimePacket {
                        flow_id,
                        sequence_number: seq,
                        send_mono_ns: ts.mono_ns,
                        send_utc_ns: ts.utc_ns,
                    };
                    let bytes = pkt.to_bytes_padded(packet_size);
                    let t0 = Instant::now();
                    let result = socket.send_to(&bytes, peer);
                    let call_ns = t0.elapsed().as_nanos().min(u32::MAX as u128) as u32;
                    let status = SendStatus::of(&result);
                    if status == SendStatus::Ok {
                        counters.sent.fetch_add(1, Ordering::Relaxed);
                        counters
                            .bytes_sent
                            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    }
                    sink.push(PacketRow {
                        flow_id,
                        sequence_number: seq,
                        direction: Direction::Sent,
                        timestamp: ts,
                        payload_size: bytes.len() as u32,
                        protocol: Protocol::Udp,
                        local_port: None,
                        remote_port: Some(peer.port() as u32),
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
                    pacer.wait();
                }
            }
            FlowDir::ToServer => {
                let mut last_recv_mono: Option<u64> = None;
                let mut started = false;
                let _ = udp_timing::enable_rx_timestamps(fd);
                while !stop.load(Ordering::Acquire) {
                    match udp_timing::recv_stamped(fd, &mut buf) {
                        Ok(udp_timing::Received { n, from, kernel_utc_ns }) => {
                            let recv_ts = Timestamp::now();
                            if !started {
                                started = true;
                                if let Some(addr) = from {
                                    counters.note_peer(addr);
                                }
                                *state.lock().unwrap() = GeneratorState::Running;
                            }
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
                                flow_id,
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
                                timing: PacketTiming {
                                    kernel_rx_utc_ns: kernel_utc_ns,
                                    ..PacketTiming::default()
                                },
                            });
                        }
                        Err(ref e) if would_block(e) => continue,
                        Err(_) => continue,
                    }
                }
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
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .ok();
        stream
            .set_write_timeout(Some(Duration::from_millis(200)))
            .ok();
        *state.lock().unwrap() = GeneratorState::Running;

        let mut seq = 0u64;
        match direction {
            FlowDir::FromServer => {
                let (chunk, mut pacer) = match rate_kbps {
                    Some(kbps) => {
                        let (chunk, period) = udp_timing::bulk_pacing(kbps, BULK_CHUNK);
                        (chunk, Some(Pacer::new(period)))
                    }
                    None => (BULK_CHUNK, None),
                };
                let payload = vec![0u8; chunk];
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
                            sink.push(bulk_row(flow_id, seq, Direction::Sent, ts, n as u32));
                            seq += 1;
                            if target_bytes.is_some_and(|t| total >= t) {
                                break;
                            }
                        }
                        Err(ref e) if would_block(e) => continue,
                        Err(_) => break,
                    }
                }
            }
            FlowDir::ToServer => {
                let mut payload = vec![0u8; BULK_CHUNK];
                while !stop.load(Ordering::Acquire) {
                    match stream.read(&mut payload) {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            let ts = Timestamp::now();
                            counters.received.fetch_add(1, Ordering::Relaxed);
                            counters
                                .bytes_received
                                .fetch_add(n as u64, Ordering::Relaxed);
                            sink.push(bulk_row(flow_id, seq, Direction::Received, ts, n as u32));
                            seq += 1;
                        }
                        Err(ref e) if would_block(e) => continue,
                        Err(_) => break,
                    }
                }
            }
        }
        finalize(&state, sink, writer);
    })
}

/// app_traffic object server: accepts any number of UE connections and serves
/// object requests on each (`AppRequest`): a download is answered with `size`
/// bytes, an upload is read and acknowledged by echoing the header. Rows carry
/// the object id, activity and size, like the UE's (see core `app_traffic`).
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
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(ACCEPT_POLL),
                    Err(_) => thread::sleep(ACCEPT_POLL),
                }
            }
        });
        finalize(&state, sink.into_inner().unwrap(), writer);
    })
}

fn serve_app_connection(mut conn: TcpStream, flow_id: u32, sink: &Mutex<LogSink>, counters: &Counters, stop: &AtomicBool) {
    let _ = conn.set_nonblocking(false);
    let _ = conn.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = conn.set_write_timeout(Some(Duration::from_millis(200)));
    let _ = conn.set_nodelay(true);
    let push = |dir: Direction, size: usize, req: &AppRequest| {
        let row = PacketRow {
            timing: PacketTiming {
                app_activity: Some(activity_name(req.activity)),
                app_object_bytes: Some(req.size),
                ..PacketTiming::default()
            },
            ..bulk_row(flow_id, req.object_id, dir, Timestamp::now(), size as u32)
        };
        sink.lock().unwrap().push(row);
    };
    let mut head = [0u8; AppRequest::WIRE_LEN];
    let mut buf = vec![0u8; BULK_CHUNK];
    // a connection may sit idle between bursts for as long as the UE keeps it
    let forever = Instant::now() + Duration::from_secs(365 * 86_400);
    while !stop.load(Ordering::Acquire) {
        if app_traffic::read_exact_until(&mut conn, &mut head, stop, forever).is_err() {
            return;
        }
        let Ok(req) = AppRequest::decode(&head) else {
            return; // not our protocol
        };
        push(Direction::Received, head.len(), &req);
        counters.received.fetch_add(1, Ordering::Relaxed);
        counters.bytes_received.fetch_add(head.len() as u64, Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut left = req.size;
        if req.kind == AppRequest::UPLOAD {
            while left > 0 {
                let want = left.min(BULK_CHUNK as u64) as usize;
                match conn.read(&mut buf[..want]) {
                    Ok(0) => return,
                    Ok(n) => {
                        push(Direction::Received, n, &req);
                        counters.received.fetch_add(1, Ordering::Relaxed);
                        counters.bytes_received.fetch_add(n as u64, Ordering::Relaxed);
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
            if app_traffic::write_all_until(&mut conn, &head, stop, deadline).is_err() {
                return;
            }
            push(Direction::Sent, head.len(), &req);
            counters.sent.fetch_add(1, Ordering::Relaxed);
            counters.bytes_sent.fetch_add(head.len() as u64, Ordering::Relaxed);
        } else {
            while left > 0 {
                let n = left.min(BULK_CHUNK as u64) as usize;
                if app_traffic::write_all_until(&mut conn, &buf[..n], stop, deadline).is_err() {
                    return;
                }
                push(Direction::Sent, n, &req);
                counters.sent.fetch_add(1, Ordering::Relaxed);
                counters.bytes_sent.fetch_add(n as u64, Ordering::Relaxed);
                left -= n as u64;
            }
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
        timing: PacketTiming::default(),
    }
}

fn would_block(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

fn finalize(state: &Arc<Mutex<GeneratorState>>, sink: LogSink, writer: WriterHandle) {
    *state.lock().unwrap() = GeneratorState::Stopping;
    drop(sink);
    writer.shutdown();
    *state.lock().unwrap() = GeneratorState::Stopped;
}

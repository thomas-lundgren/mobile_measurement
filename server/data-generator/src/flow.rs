//! Probe echo flow — the server-side real-time thread for a `probe` flow.
//!
//! The server hosts the echo (invariant 5: the UE originates). Hot-path order
//! per `contracts/internal/logger_sink.md`: receive → timestamp → push, then
//! timestamp → send → push for the echo. Logging is the wait-free SPSC handoff;
//! the writer thread does Parquet off the hot path.

use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fieldtest_core::log_writer::{Direction, LogSink, PacketRow, Protocol, WriterHandle};
use fieldtest_core::packet::{ProbePacket, RealtimePacket};
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
                        *state.lock().unwrap() = GeneratorState::Running;
                    }
                    counters.received.fetch_add(1, Ordering::Relaxed);
                    counters.bytes_received.fetch_add(n as u64, Ordering::Relaxed);
                    let seq = ProbePacket::decode(&buf[..n]).map(|p| p.sequence_number).unwrap_or(0);
                    sink.push(PacketRow::probe(flow_id, seq, Direction::Received, recv_ts, n as u32));

                    // echo verbatim
                    let send_ts = Timestamp::now();
                    if socket.send_to(&buf[..n], peer).is_ok() {
                        counters.sent.fetch_add(1, Ordering::Relaxed);
                        counters.bytes_sent.fetch_add(n as u64, Ordering::Relaxed);
                        sink.push(PacketRow::probe(flow_id, seq, Direction::Sent, send_ts, n as u32));
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
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
    state: Arc<Mutex<GeneratorState>>,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        *state.lock().unwrap() = GeneratorState::WaitingForClient;
        let period_ns = 1_000_000_000u64 / rate_pps.max(1) as u64;
        let mut buf = [0u8; 2048];

        match direction {
            FlowDir::FromServer => {
                // wait for the first keepalive to learn the UE address
                let peer = loop {
                    if stop.load(Ordering::Acquire) {
                        finalize(&state, sink, writer);
                        return;
                    }
                    match socket.recv_from(&mut buf) {
                        Ok((_, addr)) => break addr,
                        Err(ref e) if would_block(e) => continue,
                        Err(_) => continue,
                    }
                };
                *state.lock().unwrap() = GeneratorState::Running;

                let period = Duration::from_nanos(period_ns);
                let mut seq = 0u64;
                let mut next = Instant::now();
                while !stop.load(Ordering::Acquire) {
                    let ts = Timestamp::now();
                    let pkt = RealtimePacket { flow_id, sequence_number: seq, send_mono_ns: ts.mono_ns, send_utc_ns: ts.utc_ns };
                    let bytes = pkt.to_bytes_padded(packet_size);
                    if socket.send_to(&bytes, peer).is_ok() {
                        counters.sent.fetch_add(1, Ordering::Relaxed);
                        counters.bytes_sent.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                        sink.push(PacketRow {
                            flow_id,
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
            FlowDir::ToServer => {
                let mut last_recv_mono: Option<u64> = None;
                let mut started = false;
                while !stop.load(Ordering::Acquire) {
                    match socket.recv_from(&mut buf) {
                        Ok((n, _addr)) => {
                            let recv_ts = Timestamp::now();
                            if !started {
                                started = true;
                                *state.lock().unwrap() = GeneratorState::Running;
                            }
                            counters.received.fetch_add(1, Ordering::Relaxed);
                            counters.bytes_received.fetch_add(n as u64, Ordering::Relaxed);
                            let (sender, seq) = match RealtimePacket::decode(&buf[..n]) {
                                Ok(p) => (Some(Timestamp { mono_ns: p.send_mono_ns, utc_ns: p.send_utc_ns }), p.sequence_number),
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
                Ok((s, _)) => break s,
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
        stream.set_read_timeout(Some(Duration::from_millis(200))).ok();
        stream.set_write_timeout(Some(Duration::from_millis(200))).ok();
        *state.lock().unwrap() = GeneratorState::Running;

        let mut seq = 0u64;
        match direction {
            FlowDir::FromServer => {
                let payload = vec![0u8; BULK_CHUNK];
                let mut total = 0u64;
                while !stop.load(Ordering::Acquire) {
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
                            counters.bytes_received.fetch_add(n as u64, Ordering::Relaxed);
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
    }
}

fn would_block(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
}

fn finalize(state: &Arc<Mutex<GeneratorState>>, sink: LogSink, writer: WriterHandle) {
    *state.lock().unwrap() = GeneratorState::Stopping;
    drop(sink);
    writer.shutdown();
    *state.lock().unwrap() = GeneratorState::Stopped;
}

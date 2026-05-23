//! Probe echo flow — the server-side real-time thread for a `probe` flow.
//!
//! The server hosts the echo (invariant 5: the UE originates). Hot-path order
//! per `contracts/internal/logger_sink.md`: receive → timestamp → push, then
//! timestamp → send → push for the echo. Logging is the wait-free SPSC handoff;
//! the writer thread does Parquet off the hot path.

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use fieldtest_core::log_writer::{Direction, LogSink, PacketRow, WriterHandle};
use fieldtest_core::packet::ProbePacket;
use fieldtest_core::timestamp::Timestamp;
use mp_contracts::GeneratorState;

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

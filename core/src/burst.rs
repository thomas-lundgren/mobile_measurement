//! Paced UDP burst engine: one object moved as `count` datagrams of `size`
//! bytes, `interval` apart, over the flow's UDP port
//! (`contracts/packets/app_burst.md`). The phone's side (`run`) opens every
//! burst; the server's side (`serve`) does what each request says.
//!
//! Like `exchange`, the engine knows nothing about users or applications and
//! does not log: it reports every data datagram it sends or receives to its
//! caller (`BurstIo`), which logs it (`PacketRow::burst`).
//! `core/tests/layering.rs` keeps the workloads out of this module.

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use crate::log_writer::Direction;
use crate::packet::{AppBurstAck, AppBurstData, AppBurstEnd, AppBurstRequest};
use crate::timestamp::Timestamp;
use crate::udp_timing::{self, Pacer};

/// Datagram size when the object does not say (fits any path MTU).
pub const DATAGRAM: u32 = 1200;
/// Largest datagram sent (no IP fragmentation on a 1500-byte path).
pub const MAX_DATAGRAM: u32 = 1400;
/// Pacing when the object has no rate.
pub const DEFAULT_BPS: u32 = 8_000_000;
/// A request or end marker is repeated this often until answered…
const RETRY: Duration = Duration::from_millis(300);
/// …at most this many times in all.
const TRIES: u32 = 3;
/// Downlink: a burst ends this long after its last datagram (or its expected
/// end, whichever is later) when some never came.
const IDLE_END: Duration = Duration::from_secs(1);
/// Server: forget a burst this long after its last packet.
const FORGET: Duration = Duration::from_secs(60);

/// `direction` of an `AppBurstRequest`.
pub const DOWNLINK: u8 = 0;
pub const UPLINK: u8 = 1;
/// `dscp` of an `AppBurstRequest`: leave the datagrams unmarked.
pub const UNMARKED: u8 = 0xFF;

/// One data datagram sent or received, for the caller's log.
pub struct BurstIo<'a> {
    pub direction: Direction,
    pub at: Timestamp,
    pub size: usize,
    pub data: &'a AppBurstData,
    /// RECEIVED, when the socket reports them: kernel receive time (UTC ns)
    /// and the IP TOS byte.
    pub kernel_utc_ns: Option<u64>,
    pub tos: Option<u8>,
}

/// What a burst moved, for the caller's record.
#[derive(Debug, Clone, Copy, Default)]
pub struct BurstResult {
    /// Data datagrams that arrived (downlink: here; uplink: the server's ack).
    pub received: u32,
    /// The first datagram (downlink) or the acknowledgement (uplink).
    pub first: Option<Timestamp>,
    pub bytes_up: u64,
    pub bytes_down: u64,
}

/// Datagram size and spacing for `bytes` at `rate_bps`: `chunk` bytes per
/// datagram (0 = `DATAGRAM`), clamped to fit a data header and the MTU.
pub fn shape(bytes: u64, rate_bps: u32, chunk: u32) -> (u32, u32, Duration) {
    let size = if chunk == 0 { DATAGRAM } else { chunk }
        .clamp(AppBurstData::WIRE_LEN as u32, MAX_DATAGRAM);
    let count = bytes.div_ceil(size as u64).max(1) as u32;
    let bps = if rate_bps == 0 { DEFAULT_BPS } else { rate_bps };
    let interval = Duration::from_secs_f64(size as f64 * 8.0 / bps as f64);
    (count, size, interval)
}

/// A UDP socket for bursts to `server`, with kernel receive times and the
/// received TOS enabled.
pub fn socket(server: &str) -> io::Result<UdpSocket> {
    let s = UdpSocket::bind("0.0.0.0:0")?;
    s.connect(server)?;
    let _ = udp_timing::enable_rx_timestamps(s.as_raw_fd());
    let _ = udp_timing::enable_recv_tos(s.as_raw_fd());
    Ok(s)
}

fn data_bytes(d: &AppBurstData, size: usize) -> Vec<u8> {
    let mut b = d.to_bytes().to_vec();
    b.resize(size.max(b.len()), 0);
    b
}

/// Phone side: run the burst `req` on `sock` (connected to the server's UDP
/// port). Errors: `Interrupted` when `stop` is set, `TimedOut` when nothing
/// came back (no datagram, or no acknowledgement) after the retries.
pub fn run(
    sock: &UdpSocket,
    req: &AppBurstRequest,
    stop: &AtomicBool,
    mut on_io: impl FnMut(BurstIo),
) -> io::Result<BurstResult> {
    let fd = sock.as_raw_fd();
    let mut r = BurstResult::default();
    let head = req.to_bytes();
    let send = |b: &[u8], r: &mut BurstResult| {
        if sock.send(b).is_ok() {
            r.bytes_up += b.len() as u64;
        }
    };
    send(&head, &mut r);
    let interval = Duration::from_micros(req.interval_us as u64);
    let mut buf = vec![0u8; MAX_DATAGRAM as usize + 64];
    if req.direction == UPLINK {
        let mut pacer = Pacer::new(interval);
        let mut sent = 0u32;
        for index in 0..req.count {
            if stop.load(Ordering::Acquire) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            pacer.wait();
            let d = AppBurstData {
                flow_id: req.flow_id,
                object_id: req.object_id,
                index,
                count: req.count,
                send_mono_ns: 0,
                send_utc_ns: 0,
            };
            let at = Timestamp::now();
            let d = AppBurstData {
                send_mono_ns: at.mono_ns,
                send_utc_ns: at.utc_ns,
                ..d
            };
            let b = data_bytes(&d, req.size as usize);
            if sock.send(&b).is_ok() {
                sent += 1;
                r.bytes_up += b.len() as u64;
                on_io(BurstIo {
                    direction: Direction::Sent,
                    at,
                    size: b.len(),
                    data: &d,
                    kernel_utc_ns: None,
                    tos: None,
                });
            }
        }
        let end = AppBurstEnd {
            flow_id: req.flow_id,
            object_id: req.object_id,
            sent,
        }
        .to_bytes();
        for _ in 0..TRIES {
            send(&end, &mut r);
            let until = Instant::now() + RETRY;
            while Instant::now() < until {
                if stop.load(Ordering::Acquire) {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                if !udp_timing::wait_readable(fd, Duration::from_millis(50)) {
                    continue;
                }
                let Ok(rx) = udp_timing::recv_stamped_nowait(fd, &mut buf) else {
                    continue;
                };
                if let Ok(ack) = AppBurstAck::decode(&buf[..rx.n]) {
                    if ack.object_id == req.object_id {
                        r.bytes_down += rx.n as u64;
                        r.received = ack.received;
                        r.first = Some(Timestamp::now());
                        return Ok(r);
                    }
                }
            }
        }
        return Err(io::ErrorKind::TimedOut.into());
    }
    // downlink
    let start = Instant::now();
    let expected_end = start + Duration::from_millis(req.delay_ms as u64) + interval * req.count;
    let mut retry_at = start + RETRY + Duration::from_millis(req.delay_ms as u64);
    let (mut tries, mut last_rx) = (1u32, start);
    let mut seen = vec![false; req.count as usize];
    loop {
        if stop.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let now = Instant::now();
        if r.received == 0 && now >= retry_at {
            if tries >= TRIES {
                return Err(io::ErrorKind::TimedOut.into());
            }
            send(&head, &mut r);
            tries += 1;
            retry_at = now + RETRY;
        }
        if r.received > 0 && now >= expected_end.max(last_rx) + IDLE_END {
            return Ok(r); // the rest never came
        }
        if !udp_timing::wait_readable(fd, Duration::from_millis(50)) {
            continue;
        }
        let Ok(rx) = udp_timing::recv_stamped_nowait(fd, &mut buf) else {
            continue;
        };
        let Ok(d) = AppBurstData::decode(&buf[..rx.n]) else {
            continue;
        };
        if d.object_id != req.object_id || d.index >= req.count || seen[d.index as usize] {
            continue;
        }
        seen[d.index as usize] = true;
        let at = Timestamp::now();
        last_rx = Instant::now();
        r.first.get_or_insert(at);
        r.received += 1;
        r.bytes_down += rx.n as u64;
        on_io(BurstIo {
            direction: Direction::Received,
            at,
            size: rx.n,
            data: &d,
            kernel_utc_ns: rx.kernel_utc_ns,
            tos: rx.tos,
        });
        if r.received == req.count {
            return Ok(r);
        }
    }
}

/// A burst the server knows: its request, and for an uplink how many
/// datagrams arrived.
struct Known {
    req: AppBurstRequest,
    received: u32,
    last: Instant,
}

/// Server side: serve bursts on `sock` (bound to the flow's port) until
/// `stop`. A downlink request starts, once per object, a sender that waits
/// the think time and sends the datagrams to the request's source; uplink
/// datagrams are counted and an end marker answered with the count.
/// `on_io(io, activity)` is called for every data datagram, from several
/// threads.
pub fn serve(sock: &UdpSocket, stop: &AtomicBool, on_io: &(dyn Fn(BurstIo, u8) + Sync)) {
    let fd = sock.as_raw_fd();
    let _ = udp_timing::enable_rx_timestamps(fd);
    let _ = udp_timing::enable_recv_tos(fd);
    let known: Mutex<HashMap<(u32, u64), Known>> = Mutex::new(HashMap::new());
    let mut buf = vec![0u8; MAX_DATAGRAM as usize + 64];
    thread::scope(|s| {
        while !stop.load(Ordering::Acquire) {
            if !udp_timing::wait_readable(fd, Duration::from_millis(100)) {
                known
                    .lock()
                    .unwrap()
                    .retain(|_, k| k.last.elapsed() < FORGET);
                continue;
            }
            let Ok(rx) = udp_timing::recv_stamped_nowait(fd, &mut buf) else {
                continue;
            };
            let (Some(from), pkt) = (rx.from, &buf[..rx.n]) else {
                continue;
            };
            if let Ok(req) = AppBurstRequest::decode(pkt) {
                let key = (req.flow_id, req.object_id);
                let mut k = known.lock().unwrap();
                if k.contains_key(&key) {
                    continue; // a repeated request
                }
                k.insert(
                    key,
                    Known {
                        req,
                        received: 0,
                        last: Instant::now(),
                    },
                );
                if req.direction == DOWNLINK {
                    let Ok(out) = sock.try_clone() else { continue };
                    s.spawn(move || send_burst(&out, from, &req, stop, on_io));
                }
            } else if let Ok(d) = AppBurstData::decode(pkt) {
                let activity = {
                    let mut k = known.lock().unwrap();
                    let Some(b) = k.get_mut(&(d.flow_id, d.object_id)) else {
                        continue; // its request was lost: not ours to count
                    };
                    b.received += 1;
                    b.last = Instant::now();
                    b.req.activity
                };
                on_io(
                    BurstIo {
                        direction: Direction::Received,
                        at: Timestamp::now(),
                        size: rx.n,
                        data: &d,
                        kernel_utc_ns: rx.kernel_utc_ns,
                        tos: rx.tos,
                    },
                    activity,
                );
            } else if let Ok(end) = AppBurstEnd::decode(pkt) {
                let received = known
                    .lock()
                    .unwrap()
                    .get(&(end.flow_id, end.object_id))
                    .map_or(0, |k| k.received);
                let ack = AppBurstAck {
                    flow_id: end.flow_id,
                    object_id: end.object_id,
                    received,
                };
                let _ = sock.send_to(&ack.to_bytes(), from);
            }
        }
    });
}

/// A downlink burst: the think time, then the datagrams, paced, to `to`.
fn send_burst(
    sock: &UdpSocket,
    to: SocketAddr,
    req: &AppBurstRequest,
    stop: &AtomicBool,
    on_io: &(dyn Fn(BurstIo, u8) + Sync),
) {
    let start = Instant::now() + Duration::from_millis(req.delay_ms as u64);
    while Instant::now() < start {
        if stop.load(Ordering::Acquire) {
            return;
        }
        thread::sleep((start - Instant::now()).min(Duration::from_millis(50)));
    }
    let mut pacer = Pacer::new(Duration::from_micros(req.interval_us as u64));
    for index in 0..req.count {
        if stop.load(Ordering::Acquire) {
            return;
        }
        pacer.wait();
        let at = Timestamp::now();
        let d = AppBurstData {
            flow_id: req.flow_id,
            object_id: req.object_id,
            index,
            count: req.count,
            send_mono_ns: at.mono_ns,
            send_utc_ns: at.utc_ns,
        };
        let b = data_bytes(&d, req.size as usize);
        if sock.send_to(&b, to).is_ok() {
            on_io(
                BurstIo {
                    direction: Direction::Sent,
                    at,
                    size: b.len(),
                    data: &d,
                    kernel_utc_ns: None,
                    tos: None,
                },
                req.activity,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(direction: u8, count: u32, interval_us: u32, object_id: u64) -> AppBurstRequest {
        AppBurstRequest {
            flow_id: 4,
            object_id,
            activity: 6,
            direction,
            dscp: UNMARKED,
            count,
            size: 1000,
            interval_us,
            delay_ms: 30,
        }
    }

    /// The phone's and the server's halves against each other on
    /// localhost: a paced downlink burst, then an uplink one with its ack.
    #[test]
    fn run_against_serve() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap().to_string();
        let stop = AtomicBool::new(false);
        let server_rx = std::sync::atomic::AtomicU32::new(0);
        thread::scope(|s| {
            let (server, stop, server_rx) = (&server, &stop, &server_rx);
            s.spawn(move || {
                serve(server, stop, &|io: BurstIo, activity| {
                    assert_eq!(activity, 6);
                    if io.direction == Direction::Received {
                        server_rx.fetch_add(1, Ordering::AcqRel);
                    }
                })
            });
            let sock = socket(&addr).unwrap();
            // 50 datagrams, 2 ms apart: about 100 ms after the 30 ms think
            let (mut got, t0) = (vec![], Instant::now());
            let r = run(&sock, &request(DOWNLINK, 50, 2000, 1), stop, |io| {
                assert_eq!(io.direction, Direction::Received);
                assert!(io.data.send_utc_ns > 0 && io.size == 1000);
                got.push(io.data.index);
            })
            .unwrap();
            let took = t0.elapsed();
            assert_eq!((r.received, got.len()), (50, 50));
            assert!(
                took >= Duration::from_millis(120) && took < Duration::from_millis(600),
                "{took:?}"
            );
            assert_eq!(r.bytes_down, 50_000);
            // uplink: 20 datagrams, then the server's count
            let mut sent = 0;
            let r = run(&sock, &request(UPLINK, 20, 1000, 2), stop, |io| {
                assert_eq!(io.direction, Direction::Sent);
                sent += 1;
            })
            .unwrap();
            assert_eq!((sent, r.received), (20, 20));
            assert_eq!(server_rx.load(Ordering::Acquire), 20);
            stop.store(true, Ordering::Release);
        });
    }

    /// No server: the request is tried three times, then `TimedOut`.
    #[test]
    fn no_answer_times_out_after_the_retries() {
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let sock = socket(&silent.local_addr().unwrap().to_string()).unwrap();
        let t0 = Instant::now();
        let mut req = request(DOWNLINK, 5, 1000, 3);
        req.delay_ms = 0;
        let e = run(&sock, &req, &AtomicBool::new(false), |_| {}).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(t0.elapsed() >= RETRY * TRIES - Duration::from_millis(50));
        let mut b = [0u8; 64];
        silent
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let tries = std::iter::from_fn(|| silent.recv(&mut b).ok()).count();
        assert_eq!(tries, TRIES as usize);
    }

    #[test]
    fn shape_from_bytes_rate_and_chunk() {
        // 1.2 MB at 9.6 Mbit/s in 1200-B datagrams: 1000 of them, 1 ms apart
        let (count, size, interval) = shape(1_200_000, 9_600_000, 0);
        assert_eq!(
            (count, size, interval),
            (1000, 1200, Duration::from_millis(1))
        );
        // tiny chunks grow to a data header; huge ones stop at the MTU
        assert_eq!(shape(100, 0, 10).1, AppBurstData::WIRE_LEN as u32);
        assert_eq!(shape(100_000, 0, 9000).1, MAX_DATAGRAM);
    }
}

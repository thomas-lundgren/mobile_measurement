//! Request/response traffic engine: one `AppExchange` per object over a TCP
//! connection — bytes up, a server think time, bytes down (paced like a token
//! stream when asked) or an acknowledgement. Both nodes use it: the UE's
//! application workloads (`app_traffic`) call `transfer`, the server's
//! data-generator runs `serve` on every connection the UE opens. Wire format:
//! `contracts/packets/app_request.md` (v2; v1 requests are still served).
//!
//! The engine knows nothing about users or applications: it moves one
//! exchange and reports every read and write. Which exchanges happen, when,
//! and over which pooled connection is the workload's business.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use crate::endpoint::Counters;
use crate::log_writer::{Direction, LogSink, PacketRow, PacketTiming};
use crate::packet::{AppExchange, AppRequest, VER_1};
use crate::timestamp::Timestamp;
use crate::udp_timing;

/// Bytes per read / write (and the cap on a paced write).
pub const CHUNK: usize = 64 * 1024;
/// Socket timeout: short, so blocked reads and writes notice `stop`.
pub const IO_TIMEOUT: Duration = Duration::from_millis(200);
/// Give up on an exchange (and its connection) after this long beyond the
/// server's own think and stream time.
pub const OBJECT_TIMEOUT: Duration = Duration::from_secs(120);

/// Activity codes on the wire (the index) and the names logged for them.
pub const ACTIVITIES: [&str; 14] = [
    "other",
    "web_page",
    "web_object",
    "mail_sync",
    "mail_attachment",
    "mail_send",
    "video_chunk",
    "feed_image",
    "photo_upload",
    "llm_call",
    "mcp_call",
    "web_fetch",
    "cloud_tool",
    "llm_resume",
];

pub fn activity_code(name: &str) -> u8 {
    ACTIVITIES.iter().position(|a| *a == name).unwrap_or(0) as u8
}

pub fn activity_name(code: u8) -> &'static str {
    ACTIVITIES.get(code as usize).copied().unwrap_or("other")
}

/// How long the server takes beyond moving the bytes: think time plus, for a
/// paced stream, its length.
pub fn server_time(delay_ms: u32, down_bytes: u64, rate_bps: u32) -> Duration {
    let stream = if rate_bps > 0 {
        down_bytes as f64 * 8.0 / rate_bps as f64
    } else {
        0.0
    };
    Duration::from_millis(delay_ms as u64) + Duration::from_secs_f64(stream)
}

fn deadline(x: &AppExchange) -> Instant {
    Instant::now() + OBJECT_TIMEOUT + server_time(x.delay_ms, x.down_bytes, x.rate_bps)
}

/// The packet row both nodes log for one read or write of an exchange. `v1`:
/// the request came in the old layout (object size = the larger direction, no
/// separate upload size).
pub fn row(
    flow_id: u32,
    x: &AppExchange,
    v1: bool,
    direction: Direction,
    size: usize,
) -> PacketRow {
    PacketRow {
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
        ..PacketRow::tcp(
            flow_id,
            x.object_id,
            direction,
            Timestamp::now(),
            size as u32,
        )
    }
}

// --- socket primitives ---------------------------------------------------------

/// A connection to the server, ready for exchanges: connect timeout, short
/// socket timeouts, no Nagle delay.
pub fn connect(server: &str) -> io::Result<TcpStream> {
    let addr = server
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::other(format!("cannot resolve {server}")))?;
    let c = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    prepare(&c);
    Ok(c)
}

/// Socket settings for an exchange connection (either end).
fn prepare(c: &TcpStream) {
    let _ = c.set_nonblocking(false);
    let _ = c.set_read_timeout(Some(IO_TIMEOUT));
    let _ = c.set_write_timeout(Some(IO_TIMEOUT));
    let _ = c.set_nodelay(true);
}

/// Should a blocked read/write give up? (stopped, or the exchange took too long)
fn give_up(stop: &AtomicBool, deadline: Instant) -> bool {
    stop.load(Ordering::Acquire) || Instant::now() >= deadline
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// `write_all` that survives the socket's short write timeout.
pub fn write_all_until(
    conn: &mut TcpStream,
    mut buf: &[u8],
    stop: &AtomicBool,
    deadline: Instant,
) -> io::Result<()> {
    while !buf.is_empty() {
        match conn.write(buf) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => buf = &buf[n..],
            Err(e) if is_timeout(&e) || e.kind() == io::ErrorKind::Interrupted => {
                if give_up(stop, deadline) {
                    return Err(io::ErrorKind::TimedOut.into());
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// `read_exact` that survives the socket's short read timeout.
pub fn read_exact_until(
    conn: &mut TcpStream,
    mut buf: &mut [u8],
    stop: &AtomicBool,
    deadline: Instant,
) -> io::Result<()> {
    while !buf.is_empty() {
        match conn.read(buf) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => buf = &mut buf[n..],
            Err(e) if is_timeout(&e) || e.kind() == io::ErrorKind::Interrupted => {
                if give_up(stop, deadline) {
                    return Err(io::ErrorKind::TimedOut.into());
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// --- client --------------------------------------------------------------------

/// Run one exchange on `conn`: the request, `up_bytes` in chunks, then the
/// response (`down_bytes`) or, for an upload, the server's acknowledgement.
/// `on_io(direction, bytes)` is called for every write and read — before the
/// request is written (its time is the request time), after each later write
/// and each read. Errors: `Interrupted` when `stop` is set mid-exchange,
/// `TimedOut` past the deadline, or the socket's error; the connection is then
/// unusable.
pub fn transfer(
    conn: &mut TcpStream,
    x: &AppExchange,
    stop: &AtomicBool,
    mut on_io: impl FnMut(Direction, usize),
) -> io::Result<()> {
    let deadline = deadline(x);
    let head = x.to_bytes();
    on_io(Direction::Sent, head.len());
    write_all_until(conn, &head, stop, deadline)?;

    let mut buf = vec![0u8; CHUNK];
    let mut left = x.up_bytes;
    while left > 0 {
        if stop.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let n = left.min(CHUNK as u64) as usize;
        write_all_until(conn, &buf[..n], stop, deadline)?;
        on_io(Direction::Sent, n);
        left -= n as u64;
    }
    if x.down_bytes == 0 {
        let mut ack = [0u8; AppExchange::WIRE_LEN];
        read_exact_until(conn, &mut ack, stop, deadline)?;
        on_io(Direction::Received, ack.len());
        return Ok(());
    }
    let mut left = x.down_bytes;
    while left > 0 {
        let want = left.min(CHUNK as u64) as usize;
        match conn.read(&mut buf[..want]) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                on_io(Direction::Received, n);
                left -= n as u64;
                // a stream that keeps delivering never times out: check here
                // too, or the test's end waits for the reply to finish
                if stop.load(Ordering::Acquire) {
                    return Err(io::ErrorKind::Interrupted.into());
                }
            }
            Err(e) if is_timeout(&e) || e.kind() == io::ErrorKind::Interrupted => {
                if give_up(stop, deadline) {
                    return Err(io::ErrorKind::TimedOut.into());
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// --- server --------------------------------------------------------------------

/// Serve exchanges on one UE connection until it closes or `stop`: read the
/// request (v2, or v1) and the upload, wait the think time, then send the
/// response — paced at `rate_bps` in `chunk_bytes` writes when asked — or,
/// with no response, acknowledge by echoing the request. Logs a row per read
/// and write (`row`), like the UE.
pub fn serve(
    mut conn: TcpStream,
    flow_id: u32,
    sink: &Mutex<LogSink>,
    counters: &Counters,
    stop: &AtomicBool,
) {
    prepare(&conn);
    let push = |dir: Direction, size: usize, x: &AppExchange, v1: bool| {
        sink.lock().unwrap().push(row(flow_id, x, v1, dir, size));
    };
    let mut head = [0u8; AppExchange::WIRE_LEN];
    let mut buf = vec![0u8; CHUNK];
    // a connection may sit idle between bursts for as long as the UE keeps it
    let forever = Instant::now() + Duration::from_secs(365 * 86_400);
    while !stop.load(Ordering::Acquire) {
        // pkt_t + ver say which request layout follows
        if read_exact_until(&mut conn, &mut head[..2], stop, forever).is_err() {
            return;
        }
        let len = if head[1] == VER_1 {
            AppRequest::WIRE_LEN
        } else {
            AppExchange::WIRE_LEN
        };
        if read_exact_until(&mut conn, &mut head[2..len], stop, forever).is_err() {
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
        counters.received(len as u64);
        let deadline = deadline(&x);
        let mut left = x.up_bytes;
        while left > 0 {
            let want = left.min(CHUNK as u64) as usize;
            match conn.read(&mut buf[..want]) {
                Ok(0) => return,
                Ok(n) => {
                    push(Direction::Received, n, &x, v1);
                    counters.received(n as u64);
                    left -= n as u64;
                }
                Err(ref e) if is_timeout(e) => {
                    if give_up(stop, deadline) {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
        if x.down_bytes == 0 {
            // acknowledge the upload: echo the request
            if write_all_until(&mut conn, head, stop, deadline).is_err() {
                return;
            }
            push(Direction::Sent, head.len(), &x, v1);
            counters.sent(head.len() as u64);
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
            (0, _) => CHUNK as u64,
            (bps, 0) => udp_timing::bulk_pacing(bps.div_ceil(1000), CHUNK).0 as u64,
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
            let n = (due - done).min(CHUNK as u64) as usize;
            if write_all_until(&mut conn, &buf[..n], stop, deadline).is_err() {
                return;
            }
            push(Direction::Sent, n, &x, v1);
            counters.sent(n as u64);
            done += n as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_writer::packet_channel;
    use std::net::TcpListener;

    #[test]
    fn activity_names_round_trip() {
        for (i, name) in ACTIVITIES.iter().enumerate() {
            assert_eq!(activity_code(name) as usize, i);
            assert_eq!(activity_name(i as u8), *name);
        }
        assert_eq!(activity_code("nonsense"), 0);
    }

    /// The wire table is `enums.yaml` app_activities, in order (code = index).
    #[test]
    fn activities_match_the_contract() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../contracts/enums/enums.yaml");
        let doc: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let want: Vec<&str> = doc["app_activities"]
            .as_sequence()
            .expect("enums.yaml app_activities")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(ACTIVITIES.as_slice(), want.as_slice());
    }

    fn exchange(
        object_id: u64,
        up: u64,
        down: u64,
        delay_ms: u32,
        rate_bps: u32,
        chunk: u32,
    ) -> AppExchange {
        AppExchange {
            activity: activity_code("llm_call"),
            flow_id: 7,
            object_id,
            up_bytes: up,
            down_bytes: down,
            delay_ms,
            rate_bps,
            chunk_bytes: chunk,
        }
    }

    /// The client and server halves against each other on localhost: a plain
    /// download, an upload acknowledged by an echo, and a paced stream with a
    /// think time — on one connection, as a pooled connection is reused.
    #[test]
    fn transfer_against_serve() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (sink, mut rx, _) = packet_channel(1 << 14);
        let (sink, counters, stop) = (
            Mutex::new(sink),
            Counters::default(),
            AtomicBool::new(false),
        );
        thread::scope(|s| {
            s.spawn(|| {
                let (conn, _) = listener.accept().unwrap();
                serve(conn, 7, &sink, &counters, &stop);
            });
            let mut conn = connect(&addr).unwrap();
            let mut moved = |x: &AppExchange| {
                let (mut up, mut down, mut reads) = (0usize, 0usize, 0usize);
                let t0 = Instant::now();
                transfer(&mut conn, x, &stop, |dir, n| match dir {
                    Direction::Sent => up += n,
                    Direction::Received => {
                        down += n;
                        reads += 1
                    }
                })
                .unwrap();
                (up, down, reads, t0.elapsed())
            };
            let (up, down, _, _) = moved(&exchange(1, 0, 300_000, 0, 0, 0));
            assert_eq!((up, down), (AppExchange::WIRE_LEN, 300_000));
            let (up, down, _, _) = moved(&exchange(2, 150_000, 0, 0, 0, 0));
            assert_eq!(
                (up, down),
                (AppExchange::WIRE_LEN + 150_000, AppExchange::WIRE_LEN)
            );
            // 6000 B at 48 kbit/s after 100 ms: about 1.1 s, in many small reads
            let (_, down, reads, took) = moved(&exchange(3, 2_000, 6_000, 100, 48_000, 140));
            assert_eq!(down, 6_000);
            assert!(reads >= 10, "{reads} reads");
            assert!(
                took >= Duration::from_millis(1_000) && took < Duration::from_secs(3),
                "{took:?}"
            );
            stop.store(true, Ordering::Release);
        });
        // the server logged every object: the request, then its bytes
        let mut objects = std::collections::BTreeMap::new();
        while let Ok(r) = rx.pop() {
            *objects
                .entry((r.sequence_number, r.direction == Direction::Sent))
                .or_insert(0u64) += r.payload_size as u64;
        }
        let w = AppExchange::WIRE_LEN as u64;
        assert_eq!(objects[&(1, false)], w);
        assert_eq!(objects[&(1, true)], 300_000);
        assert_eq!(objects[&(2, false)], w + 150_000);
        assert_eq!(objects[&(2, true)], w);
        assert_eq!(objects[&(3, true)], 6_000);
        assert_eq!(
            counters.bytes_sent.load(Ordering::Relaxed),
            300_000 + w + 6_000
        );
    }
}

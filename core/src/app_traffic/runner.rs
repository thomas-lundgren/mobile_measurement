//! Runs the users' steps over TCP against the server's object server: a
//! connection pool per agent, one `AppExchange` per object, a row per
//! request, chunk and acknowledgement.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{activity_name, Agent, AppAction, Fetch, Placement, Step, MAX_PARALLEL};
use crate::endpoint::Counters;
use crate::log_writer::{Direction, LogSink, PacketRow, PacketTiming};
use crate::packet::AppExchange;
use crate::timestamp::Timestamp;

const CHUNK: usize = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_millis(200);
/// Give up on an object (and its connection) after this long.
const OBJECT_TIMEOUT: Duration = Duration::from_secs(120);
/// Idle connections are closed after this long, as browsers and apps do; the
/// next burst then pays for a new TCP handshake (and slow start).
const IDLE_CLOSE: Duration = Duration::from_secs(20);

/// One app_traffic flow on the UE.
#[derive(Debug, Clone)]
pub struct AppTraffic {
    pub server: String,
    pub flow_id: u32,
    pub profile: String,
    pub workers: u32,
    pub seed: u64,
    pub scale: f64,
    pub actions: Vec<AppAction>,
    /// profile "agent": where the tools run.
    pub placement: Placement,
}

struct Ctx<'a> {
    cfg: &'a AppTraffic,
    sink: &'a Mutex<LogSink>,
    counters: &'a Counters,
    stop: &'a AtomicBool,
}

/// Run the flow's agents until `stop`. Each agent is a thread with its own
/// connection pool; a script runs on one agent only.
pub fn run(cfg: &AppTraffic, sink: LogSink, counters: &Counters, stop: &AtomicBool) {
    let sink = Mutex::new(sink);
    let ctx = Ctx {
        cfg,
        sink: &sink,
        counters,
        stop,
    };
    let workers = if cfg.profile == "script" {
        1
    } else {
        cfg.workers.clamp(1, 16)
    };
    let start = Instant::now();
    std::thread::scope(|s| {
        for w in 0..workers {
            let ctx = &ctx;
            s.spawn(move || worker(ctx, w, start));
        }
    });
}

fn worker(ctx: &Ctx, w: u32, start: Instant) {
    let seed = ctx
        .cfg
        .seed
        .wrapping_add((w as u64).wrapping_mul(0x9E37_79B9));
    let mut agent = Agent::new(&ctx.cfg.profile, seed, ctx.cfg.scale, &ctx.cfg.actions)
        .with_placement(ctx.cfg.placement);
    let mut pool: Vec<TcpStream> = Vec::new();
    let mut episode = 0u64;
    while !ctx.stop.load(Ordering::Acquire) {
        match agent.next_step(start.elapsed()) {
            Step::Until(t) => {
                let idle_from = Instant::now();
                while !ctx.stop.load(Ordering::Acquire) && start.elapsed() < t {
                    std::thread::sleep(
                        (t.saturating_sub(start.elapsed())).min(Duration::from_millis(100)),
                    );
                    if !pool.is_empty() && idle_from.elapsed() >= IDLE_CLOSE {
                        pool.clear();
                    }
                }
            }
            Step::Burst { fetches, parallel } => {
                episode += 1;
                // object_id: agent | episode (page, sync, segment...) | object
                let base = ((w as u64) << 48) | ((episode & 0x0FFF_FFFF) << 20);
                pool = burst(ctx, pool, &fetches, parallel, base);
            }
            Step::Done => {
                while !ctx.stop.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}

/// Transfer `fetches` over up to `parallel` pooled connections; returns the
/// connections still usable.
fn burst(
    ctx: &Ctx,
    mut pool: Vec<TcpStream>,
    fetches: &[Fetch],
    parallel: usize,
    base: u64,
) -> Vec<TcpStream> {
    let want = parallel.clamp(1, MAX_PARALLEL).min(fetches.len().max(1));
    while pool.len() < want {
        match connect(&ctx.cfg.server) {
            Ok(c) => pool.push(c),
            Err(_) => break,
        }
    }
    if pool.is_empty() {
        // no route to the server right now: skip this burst, don't spin
        std::thread::sleep(Duration::from_secs(1));
        return pool;
    }
    let queue = Mutex::new(fetches.iter().copied().enumerate().collect::<VecDeque<_>>());
    let kept = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for mut conn in pool.drain(..) {
            let (queue, kept) = (&queue, &kept);
            s.spawn(move || {
                loop {
                    let next = queue.lock().unwrap().pop_front();
                    let Some((i, f)) = next else {
                        kept.lock().unwrap().push(conn);
                        return;
                    };
                    if ctx.stop.load(Ordering::Acquire)
                        || transfer(ctx, &mut conn, f, base | i as u64).is_err()
                    {
                        return; // connection dropped; others finish the queue
                    }
                }
            });
        }
    });
    kept.into_inner().unwrap()
}

fn connect(server: &str) -> io::Result<TcpStream> {
    let addr = server
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::other(format!("cannot resolve {server}")))?;
    let c = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    c.set_read_timeout(Some(IO_TIMEOUT))?;
    c.set_write_timeout(Some(IO_TIMEOUT))?;
    let _ = c.set_nodelay(true);
    Ok(c)
}

fn row(flow_id: u32, object_id: u64, direction: Direction, size: u32, f: &Fetch) -> PacketRow {
    PacketRow {
        timing: PacketTiming {
            app_activity: Some(activity_name(f.activity)),
            app_object_bytes: Some(f.down),
            app_up_bytes: Some(f.up),
            ..PacketTiming::default()
        },
        ..PacketRow::tcp(flow_id, object_id, direction, Timestamp::now(), size)
    }
}

/// Should a blocked read/write give up? (stopped, or the object took too long)
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

fn transfer(ctx: &Ctx, conn: &mut TcpStream, f: Fetch, object_id: u64) -> io::Result<()> {
    let flow_id = ctx.cfg.flow_id;
    let deadline = Instant::now() + OBJECT_TIMEOUT + f.server_time();
    let req = AppExchange {
        activity: f.activity,
        flow_id,
        object_id,
        up_bytes: f.up,
        down_bytes: f.down,
        delay_ms: f.delay_ms,
        rate_bps: f.rate_bps,
        chunk_bytes: f.chunk,
    };
    let push = |dir: Direction, n: usize| {
        ctx.sink
            .lock()
            .unwrap()
            .push(row(flow_id, object_id, dir, n as u32, &f));
    };
    let head = req.to_bytes();
    push(Direction::Sent, head.len());
    write_all_until(conn, &head, ctx.stop, deadline)?;
    ctx.counters.sent(head.len() as u64);

    let mut buf = vec![0u8; CHUNK];
    let mut left = f.up;
    while left > 0 {
        if ctx.stop.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let n = left.min(CHUNK as u64) as usize;
        write_all_until(conn, &buf[..n], ctx.stop, deadline)?;
        push(Direction::Sent, n);
        ctx.counters.sent(n as u64);
        left -= n as u64;
    }
    if f.down == 0 {
        let mut ack = [0u8; AppExchange::WIRE_LEN];
        read_exact_until(conn, &mut ack, ctx.stop, deadline)?;
        push(Direction::Received, ack.len());
        ctx.counters.received(ack.len() as u64);
        return Ok(());
    }
    let mut left = f.down;
    while left > 0 {
        let want = left.min(CHUNK as u64) as usize;
        match conn.read(&mut buf[..want]) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                push(Direction::Received, n);
                ctx.counters.received(n as u64);
                left -= n as u64;
                // a stream that keeps delivering never times out: check here
                // too, or the test's end waits for the reply to finish
                if ctx.stop.load(Ordering::Acquire) {
                    return Err(io::ErrorKind::Interrupted.into());
                }
            }
            Err(e) if is_timeout(&e) || e.kind() == io::ErrorKind::Interrupted => {
                if give_up(ctx.stop, deadline) {
                    return Err(io::ErrorKind::TimedOut.into());
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

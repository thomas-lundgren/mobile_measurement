//! Runs the users' steps against the server's object server: the bursts'
//! parallelism here, each object one exchange of the request/response engine
//! (`crate::exchange`) over the agent's connection pool, a row per request,
//! chunk and acknowledgement.

use std::collections::VecDeque;
use std::io;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{Agent, AppAction, Fetch, Placement, Step, MAX_PARALLEL};
use crate::endpoint::Counters;
use crate::exchange::{self, Pool};
use crate::log_writer::{Direction, LogSink, PacketRow};
use crate::packet::AppExchange;

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
    let mut pool = Pool::new(&ctx.cfg.server, IDLE_CLOSE);
    let mut episode = 0u64;
    while !ctx.stop.load(Ordering::Acquire) {
        match agent.next_step(start.elapsed()) {
            Step::Until(t) => {
                while !ctx.stop.load(Ordering::Acquire) && start.elapsed() < t {
                    std::thread::sleep(
                        (t.saturating_sub(start.elapsed())).min(Duration::from_millis(100)),
                    );
                    pool.close_idle();
                }
            }
            Step::Burst { fetches, parallel } => {
                episode += 1;
                // object_id: agent | episode (page, sync, segment...) | object
                let base = ((w as u64) << 48) | ((episode & 0x0FFF_FFFF) << 20);
                burst(ctx, &mut pool, &fetches, parallel, base);
            }
            Step::Done => {
                while !ctx.stop.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}

/// Transfer `fetches` over up to `parallel` pooled connections; the ones
/// still usable go back to the pool.
fn burst(ctx: &Ctx, pool: &mut Pool, fetches: &[Fetch], parallel: usize, base: u64) {
    let want = parallel.clamp(1, MAX_PARALLEL).min(fetches.len().max(1));
    let conns = pool.take(want);
    if conns.is_empty() {
        // no route to the server right now: skip this burst, don't spin
        std::thread::sleep(Duration::from_secs(1));
        return;
    }
    let queue = Mutex::new(fetches.iter().copied().enumerate().collect::<VecDeque<_>>());
    let kept = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for mut conn in conns {
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
    pool.put_back(kept.into_inner().unwrap());
}

fn transfer(ctx: &Ctx, conn: &mut TcpStream, f: Fetch, object_id: u64) -> io::Result<()> {
    let flow_id = ctx.cfg.flow_id;
    let x = AppExchange {
        activity: f.activity,
        flow_id,
        object_id,
        up_bytes: f.up,
        down_bytes: f.down,
        delay_ms: f.delay_ms,
        rate_bps: f.rate_bps,
        chunk_bytes: f.chunk,
    };
    exchange::transfer(conn, &x, ctx.stop, |dir, n| {
        ctx.sink
            .lock()
            .unwrap()
            .push(PacketRow::exchange(flow_id, &x, false, dir, n));
        match dir {
            Direction::Sent => ctx.counters.sent(n as u64),
            Direction::Received => ctx.counters.received(n as u64),
        }
    })
}

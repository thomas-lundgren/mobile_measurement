//! Execution: runs one burst's operations through the request/response
//! engine (`crate::exchange`) over the agent's connection pool, at most
//! `parallel` in flight, logging a packet row per request, chunk and
//! acknowledgement, and reports how each operation ended. When and what to
//! run is the scheduler's business (`scheduler`).

use std::collections::VecDeque;
use std::io;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use super::scheduler::Operation;
use super::MAX_PARALLEL;
use crate::endpoint::Counters;
use crate::exchange::{self, Pool};
use crate::log_writer::{Direction, LogSink, PacketRow};
use crate::packet::AppExchange;
use crate::timestamp::Timestamp;

/// How an operation ended (`enums.yaml` app_outcomes, in order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Timeout,
    Stopped,
    Error,
    Skipped,
}

impl Outcome {
    pub const ALL: [Outcome; 5] = [
        Self::Ok,
        Self::Timeout,
        Self::Stopped,
        Self::Error,
        Self::Skipped,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Timeout => "timeout",
            Self::Stopped => "stopped",
            Self::Error => "error",
            Self::Skipped => "skipped",
        }
    }
}

/// What happened to one operation.
#[derive(Debug, Clone)]
pub struct Done {
    pub start: Option<Timestamp>,
    pub first_byte: Option<Timestamp>,
    pub done: Option<Timestamp>,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub outcome: Outcome,
    /// The first object on a connection opened for this burst.
    pub new_connection: Option<bool>,
}

impl Done {
    fn not_started(outcome: Outcome) -> Self {
        Self {
            start: None,
            first_byte: None,
            done: None,
            bytes_up: 0,
            bytes_down: 0,
            outcome,
            new_connection: None,
        }
    }
}

/// What the executor needs from the flow.
pub struct Exec<'a> {
    pub flow_id: u32,
    pub sink: &'a Mutex<LogSink>,
    pub counters: &'a Counters,
    pub stop: &'a AtomicBool,
}

/// Run `ops` over up to `parallel` pooled connections; the connections still
/// usable go back to the pool. One `Done` per operation, in order.
pub fn run_burst(x: &Exec, pool: &mut Pool, ops: &[Operation], parallel: usize) -> Vec<Done> {
    let want = parallel.clamp(1, MAX_PARALLEL).min(ops.len().max(1));
    let conns = pool.take(want);
    if conns.is_empty() {
        // no route to the server right now: skip this burst, don't spin
        std::thread::sleep(Duration::from_secs(1));
        return ops
            .iter()
            .map(|_| Done::not_started(Outcome::Skipped))
            .collect();
    }
    let queue = Mutex::new((0..ops.len()).collect::<VecDeque<_>>());
    let results: Mutex<Vec<Option<Done>>> = Mutex::new(vec![None; ops.len()]);
    let kept = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for (mut conn, mut fresh) in conns {
            let (queue, results, kept) = (&queue, &results, &kept);
            s.spawn(move || loop {
                if x.stop.load(Ordering::Acquire) {
                    return;
                }
                let next = queue.lock().unwrap().pop_front();
                let Some(i) = next else {
                    kept.lock().unwrap().push(conn);
                    return;
                };
                let d = transfer(x, &mut conn, &ops[i], fresh);
                fresh = false;
                let ok = d.outcome == Outcome::Ok;
                results.lock().unwrap()[i] = Some(d);
                if !ok {
                    return; // connection unusable; others finish the queue
                }
            });
        }
    });
    pool.put_back(kept.into_inner().unwrap());
    // never started: the test ended, or every connection of the burst failed
    let left = if x.stop.load(Ordering::Acquire) {
        Outcome::Stopped
    } else {
        Outcome::Skipped
    };
    results
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|d| d.unwrap_or_else(|| Done::not_started(left)))
        .collect()
}

fn transfer(x: &Exec, conn: &mut TcpStream, op: &Operation, fresh: bool) -> Done {
    let f = op.fetch;
    let ex = AppExchange {
        activity: f.activity,
        flow_id: x.flow_id,
        object_id: op.id,
        up_bytes: f.up,
        down_bytes: f.down,
        delay_ms: f.delay_ms,
        rate_bps: f.rate_bps,
        chunk_bytes: f.chunk,
    };
    let (mut start, mut first_byte) = (None, None);
    let (mut up, mut down) = (0u64, 0u64);
    let r = exchange::transfer(conn, &ex, x.stop, |dir, n| {
        x.sink
            .lock()
            .unwrap()
            .push(PacketRow::exchange(x.flow_id, &ex, false, dir, n));
        match dir {
            Direction::Sent => {
                start.get_or_insert_with(Timestamp::now);
                up += n as u64;
                x.counters.sent(n as u64);
            }
            Direction::Received => {
                first_byte.get_or_insert_with(Timestamp::now);
                down += n as u64;
                x.counters.received(n as u64);
            }
        }
    });
    let outcome = match &r {
        Ok(()) => Outcome::Ok,
        Err(_) if x.stop.load(Ordering::Acquire) => Outcome::Stopped,
        Err(e) if e.kind() == io::ErrorKind::TimedOut => Outcome::Timeout,
        Err(_) => Outcome::Error,
    };
    Done {
        start,
        first_byte,
        done: r.is_ok().then(Timestamp::now),
        bytes_up: up,
        bytes_down: down,
        outcome,
        new_connection: Some(fresh),
    }
}

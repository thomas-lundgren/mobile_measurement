//! Execution: runs one burst or task graph of operations through the
//! request/response engine (`crate::exchange`) over the agent's connection
//! pool — at most `parallel` in flight, each when the operations it depends
//! on have ended ok (`graph::Tracker`) — logging a packet row per request,
//! chunk and acknowledgement, and reports how each operation ended. When and
//! what to run is the scheduler's business (`scheduler`).

use std::io;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use super::graph::{State, Tracker};
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
    DepFailed,
}

impl Outcome {
    pub const ALL: [Outcome; 6] = [
        Self::Ok,
        Self::Timeout,
        Self::Stopped,
        Self::Error,
        Self::Skipped,
        Self::DepFailed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Timeout => "timeout",
            Self::Stopped => "stopped",
            Self::Error => "error",
            Self::Skipped => "skipped",
            Self::DepFailed => "dep_failed",
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
    /// The object opened its connection (handshake and slow start).
    pub new_connection: Option<bool>,
}

impl Done {
    pub fn not_started(outcome: Outcome) -> Self {
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

/// Run `ops` — `deps[i]`: the operations `i` waits for, none for a plain
/// burst — with up to `parallel` workers. Each operation takes a connection
/// from the pool (an idle one if any, as a browser or app reuses its
/// connections) and gives it back when it ends ok. A worker whose operation
/// fails drops that connection and stops; the others go on. One `Done` per
/// operation, in order.
pub fn run_graph(
    x: &Exec,
    pool: &Mutex<Pool>,
    ops: &[Operation],
    deps: Vec<Vec<usize>>,
    parallel: usize,
) -> Vec<Done> {
    let workers = parallel.clamp(1, MAX_PARALLEL).min(ops.len().max(1));
    let state = Mutex::new((Tracker::new(deps), vec![None::<Done>; ops.len()]));
    let changed = Condvar::new();
    let connected = AtomicBool::new(false);
    std::thread::scope(|s| {
        for _ in 0..workers {
            let (state, changed, pool, connected) = (&state, &changed, &pool, &connected);
            s.spawn(move || {
                loop {
                    // the next ready operation; wait while others may unlock one
                    let i = {
                        let mut g = state.lock().unwrap();
                        loop {
                            if x.stop.load(Ordering::Acquire) {
                                break None;
                            }
                            if let Some(i) = g.0.next_ready() {
                                break Some(i);
                            }
                            if g.0.stuck_or_done() {
                                break None;
                            }
                            g = changed
                                .wait_timeout(g, Duration::from_millis(100))
                                .unwrap()
                                .0;
                        }
                    };
                    let Some(i) = i else { break };
                    let taken = pool.lock().unwrap().take_one();
                    let d = match taken {
                        Some((mut c, fresh)) => {
                            connected.store(true, Ordering::Release);
                            let d = transfer(x, &mut c, &ops[i], fresh);
                            if d.outcome == Outcome::Ok {
                                pool.lock().unwrap().put_back(vec![c]);
                            }
                            d
                        }
                        None => Done::not_started(Outcome::Skipped),
                    };
                    let ok = d.outcome == Outcome::Ok;
                    {
                        let mut g = state.lock().unwrap();
                        g.0.finish(i, ok);
                        g.1[i] = Some(d);
                    }
                    changed.notify_all();
                    if !ok {
                        return; // connection unusable (or none); others go on
                    }
                }
            });
        }
    });
    if !connected.load(Ordering::Acquire) && !x.stop.load(Ordering::Acquire) {
        // no route to the server right now: don't spin
        std::thread::sleep(Duration::from_secs(1));
    }
    let (tracker, results) = state.into_inner().unwrap();
    // never started: a dependency failed, the test ended, or no connection
    let left = if x.stop.load(Ordering::Acquire) {
        Outcome::Stopped
    } else {
        Outcome::Skipped
    };
    results
        .into_iter()
        .enumerate()
        .map(|(i, d)| {
            d.unwrap_or_else(|| {
                Done::not_started(if tracker.state(i) == State::DepFailed {
                    Outcome::DepFailed
                } else {
                    left
                })
            })
        })
        .collect()
}

/// Open loop: start every operation when it is due, whatever has happened
/// to the others (offered load, not paced by completions), each taking a
/// pooled connection and returning it when it ends ok. One `Done` per
/// operation, in order; one not yet due when the test ends is `stopped`.
pub fn run_timed(x: &Exec, pool: &Mutex<Pool>, ops: &[Operation], due: &[Instant]) -> Vec<Done> {
    std::thread::scope(|s| {
        let runs: Vec<_> = ops
            .iter()
            .zip(due)
            .map(|(op, &due)| {
                s.spawn(move || {
                    while Instant::now() < due {
                        if x.stop.load(Ordering::Acquire) {
                            return Done::not_started(Outcome::Stopped);
                        }
                        std::thread::sleep(
                            due.saturating_duration_since(Instant::now())
                                .min(Duration::from_millis(50)),
                        );
                    }
                    if x.stop.load(Ordering::Acquire) {
                        return Done::not_started(Outcome::Stopped);
                    }
                    let taken = pool.lock().unwrap().take_one();
                    match taken {
                        Some((mut c, fresh)) => {
                            let d = transfer(x, &mut c, op, fresh);
                            if d.outcome == Outcome::Ok {
                                pool.lock().unwrap().put_back(vec![c]);
                            }
                            d
                        }
                        None => Done::not_started(Outcome::Skipped),
                    }
                })
            })
            .collect();
        runs.into_iter().map(|r| r.join().unwrap()).collect()
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_traffic::{Fetch, StepMeta};
    use crate::log_writer::packet_channel;
    use std::net::TcpListener;

    fn op(i: u64, down: u64, delay_ms: u32) -> Operation {
        Operation {
            id: i,
            agent: 0,
            episode: 1,
            after_episode: None,
            after: None,
            due: None,
            meta: StepMeta {
                task: 1,
                step: 1,
                kind: "fetch",
            },
            fetch: Fetch {
                delay_ms,
                ..Fetch::download(11, down)
            },
            parallel: 6,
            planned: Timestamp::now(),
        }
    }

    /// Run a plan → search → 3 fetches → read graph against `serve`, or a
    /// server that drops every connection after `drop_after` requests.
    fn run(drop_after: Option<usize>) -> Vec<Done> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (sink, _rx, _) = packet_channel(1 << 14);
        let (sink, counters, stop) = (
            Mutex::new(sink),
            Counters::default(),
            AtomicBool::new(false),
        );
        let ops: Vec<Operation> = (0..6).map(|i| op(i, 20_000, 50)).collect();
        let deps = vec![vec![], vec![0], vec![1], vec![1], vec![1], vec![2, 3, 4]];
        std::thread::scope(|s| {
            let stop = &stop;
            s.spawn(move || {
                listener.set_nonblocking(true).unwrap();
                let served = std::sync::atomic::AtomicUsize::new(0);
                std::thread::scope(|s| {
                    while !stop.load(Ordering::Acquire) {
                        match listener.accept() {
                            Ok((c, _)) => {
                                let served = &served;
                                s.spawn(move || match drop_after {
                                    None => exchange::serve(c, stop, |_, _, _, _| {}),
                                    // answer the first `n` downloads (on any
                                    // connection), then drop mid-request
                                    Some(n) => {
                                        use std::io::{Read, Write};
                                        let mut c = c;
                                        let mut head = [0u8; AppExchange::WIRE_LEN];
                                        while c.read_exact(&mut head).is_ok() {
                                            if served.fetch_add(1, Ordering::AcqRel) >= n {
                                                return;
                                            }
                                            let x = AppExchange::decode(&head).unwrap();
                                            c.write_all(&vec![0u8; x.down_bytes as usize]).unwrap();
                                        }
                                    }
                                });
                            }
                            Err(_) => std::thread::sleep(Duration::from_millis(5)),
                        }
                    }
                });
            });
            let x = Exec {
                flow_id: 1,
                sink: &sink,
                counters: &counters,
                stop,
            };
            let pool = Mutex::new(Pool::new(&addr, Duration::from_secs(20)));
            let done = run_graph(&x, &pool, &ops, deps, 6);
            stop.store(true, Ordering::Release);
            done
        })
    }

    #[test]
    fn graph_nodes_start_after_their_dependencies() {
        let d = run(None);
        assert!(d.iter().all(|d| d.outcome == Outcome::Ok), "{d:?}");
        let start = |i: usize| d[i].start.unwrap().mono_ns;
        let end = |i: usize| d[i].done.unwrap().mono_ns;
        assert!(start(1) >= end(0));
        assert!((2..5).all(|i| start(i) >= end(1)));
        assert!(
            (2..5).all(|i| start(5) >= end(i)),
            "the read waits for every fetch"
        );
        // the three fetches overlap: they ran in parallel
        assert!((3..5).all(|i| start(i) < end(2)));
        // the plan's connection was reused for the search
        assert_eq!(d[0].new_connection, Some(true));
        assert_eq!(d[1].new_connection, Some(false));
    }

    /// The search's connection is dropped: the search fails, and everything
    /// that waited for it is `dep_failed` and never runs.
    #[test]
    fn a_failed_node_fails_its_dependants() {
        let d = run(Some(1));
        assert_eq!(d[0].outcome, Outcome::Ok);
        assert!(
            matches!(d[1].outcome, Outcome::Error | Outcome::Timeout),
            "{:?}",
            d[1].outcome
        );
        for (i, d) in d.iter().enumerate().skip(2) {
            assert_eq!(d.outcome, Outcome::DepFailed, "node {i}");
            assert!(d.start.is_none());
        }
    }
}

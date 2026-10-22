//! Scheduling: each agent asks its model what to do and turns it into
//! operations — the exchanges with their place in the task (agent → task →
//! step → object), the burst they waited for and, in a graph, the operations
//! they depend on — which the executor runs. Every operation becomes a row of
//! the intended-operations log (`contracts/parquet/app_events.parquet.yaml`),
//! written when the flow stops.
//!
//! Closed loop (default): a step starts once the previous one has ended, and
//! the user's idle time follows the model. Open loop (profiles agent and
//! websearch): tasks arrive at random whatever is still running, and every
//! object starts when it is due — the task's arrival plus its planned offset
//! — so a slow network shows as lateness and backlog, not as less load.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::executor::{self, Done, Engine, Exec, Outcome};
use super::model::Rng;
use super::{activity_name, Agent, AppAction, Fetch, GraphNode, Placement, Step, StepMeta};
use crate::aux_log::{self, AppEventRow, AuxContext};
use crate::endpoint::Counters;
use crate::exchange::{self, Pool};
use crate::log_writer::LogSink;
use crate::timestamp::Timestamp;

/// Idle connections are closed after this long, as browsers and apps do; the
/// next burst then pays for a new TCP handshake (and slow start).
const IDLE_CLOSE: Duration =
    Duration::from_millis((super::params::APP_IDLE_CLOSE_S * 1000.0) as u64);
/// Open loop: the rate an object's bytes are planned to move at, for the
/// offsets of the objects that depend on it.
const NOMINAL_BPS: f64 = super::params::APP_NOMINAL_BPS;

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
    /// profiles "agent", "websearch": where the tools run.
    pub placement: Placement,
    /// Where the intended-operations log goes; none: not written.
    pub log: Option<AppLog>,
    /// Open loop (profiles "agent", "websearch"); none: closed loop.
    pub open: Option<OpenLoop>,
    /// How each activity's objects move (activity code -> engine); others
    /// use `Engine::Exchange`.
    pub engines: std::collections::BTreeMap<u8, Engine>,
    /// DSCP per activity code (0–63); others unmarked.
    pub dscp: std::collections::BTreeMap<u8, u8>,
}

/// How tasks arrive in open loop.
#[derive(Debug, Clone, Copy)]
pub struct OpenLoop {
    /// Mean gap between task arrivals per user (exponential).
    pub task_interval_s: f64,
    /// Unfinished tasks per user before an arriving one is dropped.
    pub max_open_tasks: u32,
}

/// The node's log folder and join keys for `app_events_<flow>.parquet`.
#[derive(Debug, Clone)]
pub struct AppLog {
    pub dir: PathBuf,
    pub ctx: AuxContext,
}

impl AppLog {
    pub fn path(&self, flow_id: u32) -> PathBuf {
        self.dir.join(format!("app_events_{flow_id:04}.parquet"))
    }
}

/// One exchange as the workload means it.
#[derive(Debug, Clone)]
pub struct Operation {
    /// object_id: agent | episode | index in the burst.
    pub id: u64,
    pub agent: u32,
    pub episode: u64,
    pub after_episode: Option<u64>,
    /// Task graphs: the operations (object ids) this one waits for; `None`
    /// for a plain burst.
    pub after: Option<Vec<u64>>,
    /// Open loop: when it is due.
    pub due: Option<Timestamp>,
    pub engine: Engine,
    /// DSCP to mark it with; none: unmarked.
    pub dscp: Option<u8>,
    pub meta: StepMeta,
    pub fetch: Fetch,
    pub parallel: usize,
    pub planned: Timestamp,
}

/// Run the flow's agents until `stop`, then write the intended-operations
/// log. Each agent is a thread with its own connection pool; a script runs on
/// one agent only.
pub fn run(cfg: &AppTraffic, sink: LogSink, counters: &Counters, stop: &AtomicBool) {
    let sink = Mutex::new(sink);
    let rows = Mutex::new(Vec::new());
    let exec = Exec {
        flow_id: cfg.flow_id,
        sink: &sink,
        counters,
        stop,
    };
    let workers = if cfg.profile == "script" {
        1
    } else {
        cfg.workers.clamp(1, 16)
    };
    let open = cfg.open.filter(|_| {
        matches!(
            cfg.profile.as_str(),
            "agent" | "websearch" | "chat" | "image_gen" | "video_understanding"
        )
    });
    let start = Instant::now();
    std::thread::scope(|s| {
        for w in 0..workers {
            let (exec, rows) = (&exec, &rows);
            s.spawn(move || match open {
                Some(open) => agent_open(cfg, open, exec, rows, w, start),
                None => agent(cfg, exec, rows, w, start),
            });
        }
    });
    if let Some(log) = &cfg.log {
        let mut rows = rows.into_inner().unwrap();
        rows.sort_by_key(|r: &AppEventRow| (r.agent, r.object_id));
        // logging-first: a failure here loses only this table
        let _ = aux_log::write_app_events(&log.path(cfg.flow_id), &log.ctx, &rows);
    }
}

fn model(cfg: &AppTraffic, w: u32) -> Agent {
    let seed = cfg.seed.wrapping_add((w as u64).wrapping_mul(0x9E37_79B9));
    Agent::new(&cfg.profile, seed, cfg.scale, &cfg.actions).with_placement(cfg.placement)
}

/// Sleep until `t` after `start` (or `stop`), closing idle connections.
fn idle_until(exec: &Exec, pool: &Mutex<Pool>, start: Instant, t: Duration) {
    while !exec.stop.load(Ordering::Acquire) && start.elapsed() < t {
        std::thread::sleep((t.saturating_sub(start.elapsed())).min(Duration::from_millis(100)));
        pool.lock().unwrap().close_idle();
    }
}

/// Closed loop: one step at a time, as the model says.
fn agent(cfg: &AppTraffic, exec: &Exec, rows: &Mutex<Vec<AppEventRow>>, w: u32, start: Instant) {
    let mut model = model(cfg, w);
    let pool = Mutex::new(Pool::new(&cfg.server, IDLE_CLOSE));
    let mut episode = 0u64;
    while !exec.stop.load(Ordering::Acquire) {
        let (nodes, parallel, graph) = match model.next_step(start.elapsed()) {
            Step::Until(t) => {
                idle_until(exec, &pool, start, t);
                continue;
            }
            Step::Burst {
                fetches,
                parallel,
                meta,
            } => {
                let nodes = fetches
                    .into_iter()
                    .map(|fetch| GraphNode {
                        fetch,
                        meta,
                        deps: vec![],
                    })
                    .collect();
                (nodes, parallel, false)
            }
            Step::Graph { nodes, parallel } => (nodes, parallel, true),
            Step::Done => {
                idle_until(exec, &pool, start, Duration::MAX);
                continue;
            }
        };
        episode += 1;
        let ops = operations(
            cfg,
            w,
            episode,
            &nodes,
            parallel,
            graph,
            Timestamp::now(),
            None,
        );
        let deps = nodes.into_iter().map(|n| n.deps).collect();
        let done = executor::run_graph(exec, &pool, &ops, deps, parallel);
        log(cfg, rows, &ops, done);
    }
}

/// Open loop: tasks arrive with exponential gaps (their own seeded draws, so
/// the tasks are the same as closed loop's for a seed); each runs on its own
/// thread, objects starting when due. With `max_open_tasks` unfinished, an
/// arriving task is logged `skipped` and not run.
fn agent_open(
    cfg: &AppTraffic,
    open: OpenLoop,
    exec: &Exec,
    rows: &Mutex<Vec<AppEventRow>>,
    w: u32,
    start: Instant,
) {
    let mut model = model(cfg, w);
    let mut arrivals = Rng(cfg.seed ^ 0xA076_1D64_78BD_642F ^ ((w as u64) << 32));
    let pool = Mutex::new(Pool::new(&cfg.server, IDLE_CLOSE));
    let unfinished = AtomicU32::new(0);
    let mean = open.task_interval_s.max(0.1);
    let mut episode = 0u64;
    let mut arrival = Duration::from_secs_f64(arrivals.exponential(mean));
    std::thread::scope(|s| {
        while !exec.stop.load(Ordering::Acquire) {
            idle_until(exec, &pool, start, arrival);
            if exec.stop.load(Ordering::Acquire) {
                break;
            }
            arrival += Duration::from_secs_f64(arrivals.exponential(mean));
            let Some(nodes) = model.next_task(start.elapsed()) else {
                idle_until(exec, &pool, start, Duration::MAX);
                break;
            };
            episode += 1;
            let (arrived_at, arrived) = (Instant::now(), Timestamp::now());
            let offsets = offsets(&nodes);
            let due_ts = |o: Duration| Timestamp {
                mono_ns: arrived.mono_ns + o.as_nanos() as u64,
                utc_ns: arrived.utc_ns + o.as_nanos() as u64,
            };
            let mut ops = operations(cfg, w, episode, &nodes, nodes.len(), true, arrived, None);
            for (op, o) in ops.iter_mut().zip(&offsets) {
                op.due = Some(due_ts(*o));
            }
            if unfinished.load(Ordering::Acquire) >= open.max_open_tasks {
                let dropped = ops
                    .iter()
                    .map(|_| Done::not_started(Outcome::Skipped))
                    .collect();
                log(cfg, rows, &ops, dropped);
                continue;
            }
            unfinished.fetch_add(1, Ordering::AcqRel);
            let due: Vec<Instant> = offsets.iter().map(|o| arrived_at + *o).collect();
            let (pool, unfinished) = (&pool, &unfinished);
            s.spawn(move || {
                let done = executor::run_timed(exec, pool, &ops, &due);
                log(cfg, rows, &ops, done);
                unfinished.fetch_sub(1, Ordering::AcqRel);
            });
        }
    });
}

/// When each node is due after its task's arrival: once its dependencies
/// would have ended — their think times plus transfers at `NOMINAL_BPS`
/// (a paced stream at its own rate).
fn offsets(nodes: &[GraphNode]) -> Vec<Duration> {
    let nominal = |f: &Fetch| {
        let bytes = f.up + if f.rate_bps == 0 { f.down } else { 0 };
        exchange::server_time(f.delay_ms, f.down, f.rate_bps)
            + Duration::from_secs_f64(bytes as f64 * 8.0 / NOMINAL_BPS)
    };
    let mut out: Vec<Duration> = Vec::with_capacity(nodes.len());
    for n in nodes {
        let at = n
            .deps
            .iter()
            .map(|&d| out[d] + nominal(&nodes[d].fetch))
            .max()
            .unwrap_or(Duration::ZERO);
        out.push(at);
    }
    out
}

/// A step's operations. object_id: agent | episode (page, sync, segment,
/// task...) | object.
#[allow(clippy::too_many_arguments)]
fn operations(
    cfg: &AppTraffic,
    w: u32,
    episode: u64,
    nodes: &[GraphNode],
    parallel: usize,
    graph: bool,
    planned: Timestamp,
    due: Option<Timestamp>,
) -> Vec<Operation> {
    let base = ((w as u64) << 48) | ((episode & 0x0FFF_FFFF) << 20);
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| Operation {
            id: base | i as u64,
            agent: w,
            episode,
            after_episode: (episode > 1).then(|| episode - 1),
            after: graph.then(|| n.deps.iter().map(|&j| base | j as u64).collect()),
            due,
            engine: cfg
                .engines
                .get(&n.fetch.activity)
                .copied()
                .unwrap_or(Engine::Exchange),
            dscp: cfg.dscp.get(&n.fetch.activity).copied(),
            meta: n.meta,
            fetch: n.fetch,
            parallel,
            planned,
        })
        .collect()
}

fn log(cfg: &AppTraffic, rows: &Mutex<Vec<AppEventRow>>, ops: &[Operation], done: Vec<Done>) {
    rows.lock()
        .unwrap()
        .extend(ops.iter().zip(done).map(|(op, d)| row(cfg.flow_id, op, d)));
}

fn row(flow_id: u32, op: &Operation, d: Done) -> AppEventRow {
    let f = op.fetch;
    AppEventRow {
        flow_id,
        object_id: op.id,
        agent: op.agent,
        task: op.meta.task,
        step: op.meta.step,
        step_kind: op.meta.kind,
        activity: activity_name(f.activity),
        engine: op.engine.as_str(),
        dscp: op.dscp,
        episode: op.episode,
        after_episode: op.after_episode,
        after_objects: op.after.clone(),
        parallel: op.parallel as u32,
        plan_up_bytes: f.up,
        plan_down_bytes: f.down,
        plan_delay_ms: f.delay_ms,
        plan_rate_bps: f.rate_bps,
        plan_chunk_bytes: f.chunk,
        planned: op.planned,
        due: op.due,
        start: d.start,
        first_byte: d.first_byte,
        done: d.done,
        bytes_up: d.bytes_up,
        bytes_down: d.bytes_down,
        plan_packets: d.plan_packets,
        packets: d.packets,
        outcome: d.outcome.as_str(),
        new_connection: d.new_connection,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(f: Fetch, deps: Vec<usize>) -> GraphNode {
        GraphNode {
            fetch: f,
            meta: StepMeta {
                task: 1,
                step: 1,
                kind: "fetch",
            },
            deps,
        }
    }

    /// A node is due when its slowest dependency would have ended: think
    /// time plus bytes at 20 Mbit/s, or a paced stream at its own rate.
    #[test]
    fn offsets_follow_the_planned_durations() {
        let plan = Fetch {
            up: 25_000,       // 10 ms at 20 Mbit/s
            delay_ms: 900,    // think
            rate_bps: 48_000, // 6000 B streamed: 1 s
            ..Fetch::download(9, 6_000)
        };
        let page = |ms| Fetch {
            delay_ms: ms,
            ..Fetch::download(11, 250_000) // 100 ms at 20 Mbit/s
        };
        let o = offsets(&[
            node(plan, vec![]),
            node(page(200), vec![0]),
            node(page(400), vec![0]),
            node(Fetch::download(9, 1), vec![1, 2]),
        ]);
        let ms = |d: Duration| d.as_millis();
        assert_eq!(ms(o[0]), 0);
        assert_eq!((ms(o[1]), ms(o[2])), (1910, 1910));
        assert_eq!(ms(o[3]), 1910 + 400 + 100, "the slower page");
    }
}

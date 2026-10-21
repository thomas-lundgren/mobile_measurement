//! Scheduling: each agent asks its model for the next step, waits when told
//! to, and turns a burst into operations — the exchanges with their place in
//! the task (agent → task → step → object) and the burst they waited for —
//! which the executor runs. Closed loop: a burst starts once the previous one
//! has ended. Every operation becomes a row of the intended-operations log
//! (`contracts/parquet/app_events.parquet.yaml`), written when the flow stops.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::executor::{self, Exec};
use super::{activity_name, Agent, AppAction, Fetch, Placement, Step, StepMeta};
use crate::aux_log::{self, AppEventRow, AuxContext};
use crate::endpoint::Counters;
use crate::exchange::Pool;
use crate::log_writer::LogSink;
use crate::timestamp::Timestamp;

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
    /// Where the intended-operations log goes; none: not written.
    pub log: Option<AppLog>,
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
    let start = Instant::now();
    std::thread::scope(|s| {
        for w in 0..workers {
            let (exec, rows) = (&exec, &rows);
            s.spawn(move || agent(cfg, exec, rows, w, start));
        }
    });
    if let Some(log) = &cfg.log {
        let mut rows = rows.into_inner().unwrap();
        rows.sort_by_key(|r: &AppEventRow| (r.agent, r.object_id));
        // logging-first: a failure here loses only this table
        let _ = aux_log::write_app_events(&log.path(cfg.flow_id), &log.ctx, &rows);
    }
}

fn agent(cfg: &AppTraffic, exec: &Exec, rows: &Mutex<Vec<AppEventRow>>, w: u32, start: Instant) {
    let seed = cfg.seed.wrapping_add((w as u64).wrapping_mul(0x9E37_79B9));
    let mut model =
        Agent::new(&cfg.profile, seed, cfg.scale, &cfg.actions).with_placement(cfg.placement);
    let mut pool = Pool::new(&cfg.server, IDLE_CLOSE);
    let mut episode = 0u64;
    while !exec.stop.load(Ordering::Acquire) {
        match model.next_step(start.elapsed()) {
            Step::Until(t) => {
                while !exec.stop.load(Ordering::Acquire) && start.elapsed() < t {
                    std::thread::sleep(
                        (t.saturating_sub(start.elapsed())).min(Duration::from_millis(100)),
                    );
                    pool.close_idle();
                }
            }
            Step::Burst {
                fetches,
                parallel,
                meta,
            } => {
                episode += 1;
                let planned = Timestamp::now();
                // object_id: agent | episode (page, sync, segment...) | object
                let base = ((w as u64) << 48) | ((episode & 0x0FFF_FFFF) << 20);
                let ops: Vec<Operation> = fetches
                    .iter()
                    .enumerate()
                    .map(|(i, f)| Operation {
                        id: base | i as u64,
                        agent: w,
                        episode,
                        after_episode: (episode > 1).then(|| episode - 1),
                        meta,
                        fetch: *f,
                        parallel,
                        planned,
                    })
                    .collect();
                let done = executor::run_burst(exec, &mut pool, &ops, parallel);
                rows.lock()
                    .unwrap()
                    .extend(ops.iter().zip(done).map(|(op, d)| row(cfg.flow_id, op, d)));
            }
            Step::Done => {
                while !exec.stop.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}

fn row(flow_id: u32, op: &Operation, d: executor::Done) -> AppEventRow {
    let f = op.fetch;
    AppEventRow {
        flow_id,
        object_id: op.id,
        agent: op.agent,
        task: op.meta.task,
        step: op.meta.step,
        step_kind: op.meta.kind,
        activity: activity_name(f.activity),
        episode: op.episode,
        after_episode: op.after_episode,
        parallel: op.parallel as u32,
        plan_up_bytes: f.up,
        plan_down_bytes: f.down,
        plan_delay_ms: f.delay_ms,
        plan_rate_bps: f.rate_bps,
        plan_chunk_bytes: f.chunk,
        planned: op.planned,
        start: d.start,
        first_byte: d.first_byte,
        done: d.done,
        bytes_up: d.bytes_up,
        bytes_down: d.bytes_down,
        outcome: d.outcome.as_str(),
        new_connection: d.new_connection,
    }
}

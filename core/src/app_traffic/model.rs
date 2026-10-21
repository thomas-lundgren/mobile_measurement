//! The simulated users: what each profile asks for and when (seeded,
//! network-independent). `runner` turns their steps into connections.

use std::collections::VecDeque;
use std::time::Duration;

use serde::Deserialize;

/// Activity codes and names live with the wire format (the server logs them
/// too); the model's codes below index that table.
pub use crate::packet::{
    app_activity_code as activity_code, app_activity_name as activity_name,
    APP_ACTIVITIES as ACTIVITIES,
};
const WEB_PAGE: u8 = 1;
const WEB_OBJECT: u8 = 2;
const MAIL_SYNC: u8 = 3;
const MAIL_ATTACHMENT: u8 = 4;
const MAIL_SEND: u8 = 5;
const VIDEO_CHUNK: u8 = 6;
const FEED_IMAGE: u8 = 7;
const PHOTO_UPLOAD: u8 = 8;
const LLM_CALL: u8 = 9;
const MCP_CALL: u8 = 10;
const WEB_FETCH: u8 = 11;
const CLOUD_TOOL: u8 = 12;
const LLM_RESUME: u8 = 13;
const WEB_SEARCH: u8 = 14;

pub const PROFILES: [&str; 8] = [
    "web",
    "mail",
    "video",
    "social",
    "mixed",
    "agent",
    "websearch",
    "script",
];

/// Where an AI agent's tools run (profile "agent").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// On the phone: every tool call is phone traffic.
    Device,
    /// At the LLM provider, inside the call (server-side tools): no tool
    /// connections from the phone; the reply pauses while a tool runs, then
    /// carries the result text down.
    Cloud,
    /// MCP calls on the phone, web fetches in the cloud.
    Hybrid,
}

impl Placement {
    pub fn parse(s: &str) -> Self {
        match s {
            "cloud" => Self::Cloud,
            "hybrid" => Self::Hybrid,
            _ => Self::Device,
        }
    }

    fn on_device(self, tool: &Fetch) -> bool {
        match self {
            Self::Device => true,
            Self::Cloud => false,
            Self::Hybrid => tool.activity == MCP_CALL,
        }
    }
}

/// Objects in flight at once per agent (a browser's per-host connection cap).
pub const MAX_PARALLEL: usize = 6;
/// Video segment length: one chunk is due every SEGMENT.
const SEGMENT: Duration = Duration::from_secs(4);
/// LLM reply stream: bytes per output token (server-sent events carry ~3
/// tokens of text in a ~140 B event) and per event.
const SSE_BYTES_PER_TOKEN: f64 = 47.0;
const SSE_EVENT: u32 = 140;
/// Bytes a token adds to the context the next call sends (JSON text).
const CONTEXT_BYTES_PER_TOKEN: u64 = 4;
/// A tool result adds at most this much text to the context.
const TOOL_RESULT_MAX: u64 = 25_000;
/// The context is compacted to COMPACT_TO once it passes COMPACT_AT (~150k
/// tokens), as coding agents do.
const COMPACT_AT: u64 = 600_000;
const COMPACT_TO: u64 = 60_000;

/// One action of a scripted timeline (`profile = "script"`).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct AppAction {
    /// Start this long after the test starts (ms). Actions run in order; one
    /// that falls due while the previous is still running starts after it.
    pub at_ms: u64,
    /// "down" or "up".
    pub kind: String,
    /// Size of each object.
    pub bytes: u64,
    /// Number of objects (default 1).
    #[serde(default)]
    pub count: Option<u32>,
    /// Objects in flight at once (default 1, at most MAX_PARALLEL).
    #[serde(default)]
    pub parallel: Option<u32>,
    /// Activity name for the log (one of ACTIVITIES; default "other").
    #[serde(default)]
    pub activity: Option<String>,
    /// "down" only: bytes sent up first (request body, LLM context).
    #[serde(default)]
    pub up_bytes: Option<u64>,
    /// "down" only: server think time before the first byte.
    #[serde(default)]
    pub delay_ms: Option<u32>,
    /// "down": pace the response at this rate (a token stream); "up": the
    /// phone paces the upload at this rate.
    #[serde(default)]
    pub rate_kbps: Option<u32>,
    /// "down" with rate_kbps: bytes per paced write (default 5 ms worth).
    #[serde(default)]
    pub chunk_bytes: Option<u32>,
}

/// One object: `up` bytes to the server, `delay_ms` think time, `down` bytes
/// back (paced at `rate_bps` in `chunk`-byte writes when `rate_bps > 0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fetch {
    pub activity: u8,
    pub up: u64,
    pub down: u64,
    pub delay_ms: u32,
    pub rate_bps: u32,
    pub chunk: u32,
}

impl Fetch {
    pub fn download(activity: u8, bytes: u64) -> Self {
        Self {
            activity,
            up: 0,
            down: bytes,
            delay_ms: 0,
            rate_bps: 0,
            chunk: 0,
        }
    }

    pub fn upload(activity: u8, bytes: u64) -> Self {
        Self {
            activity,
            up: bytes,
            down: 0,
            delay_ms: 0,
            rate_bps: 0,
            chunk: 0,
        }
    }

    /// How long the server takes beyond moving the bytes: think time plus,
    /// for a paced stream, its length.
    #[cfg(test)]
    fn server_time(&self) -> Duration {
        crate::exchange::server_time(self.delay_ms, self.down, self.rate_bps)
    }
}

/// Kinds of burst (`enums.yaml` app_step_kinds, in order).
pub const STEP_KINDS: [&str; 12] = [
    "llm_turn",
    "tools",
    "cloud_tools",
    "page_html",
    "page_objects",
    "mail_sync",
    "video_start",
    "video_segment",
    "feed",
    "script_action",
    "search",
    "fetch",
];

/// Where a burst belongs: the agent's `task` (one unit of user intent, from
/// 1), the burst's `step` in it (from 1) and what it is (`STEP_KINDS`).
/// Assigned as the model emits the burst, without drawing from the random
/// generator, so a seed gives the same sequence with or without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepMeta {
    pub task: u32,
    pub step: u32,
    pub kind: &'static str,
}

/// What an agent does next.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Transfer these objects, at most `parallel` at a time.
    Burst {
        fetches: Vec<Fetch>,
        parallel: usize,
        meta: StepMeta,
    },
    /// Run a task graph: each node once the nodes it depends on have ended
    /// ok, at most `parallel` at a time (`graph::Tracker`).
    Graph {
        nodes: Vec<GraphNode>,
        parallel: usize,
    },
    /// Stay idle until this long after the agent started.
    Until(Duration),
    /// Nothing more to do (script finished).
    Done,
}

/// One node of a task graph: an object, where it sits in the task, and the
/// nodes (earlier in the list) it waits for.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphNode {
    pub fetch: Fetch,
    pub meta: StepMeta,
    pub deps: Vec<usize>,
}

/// splitmix64: small, seedable, good enough for traffic models.
pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub(crate) fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Exponential with this mean: the gaps of a random (Poisson) arrival.
    pub(crate) fn exponential(&mut self, mean: f64) -> f64 {
        -mean * (1.0 - self.unit()).max(1e-12).ln()
    }

    fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    /// Log-normal with the given median; sigma is the spread of ln(x).
    fn lognormal(&mut self, median: f64, sigma: f64) -> f64 {
        let u1 = self.unit().max(1e-12);
        let u2 = self.unit();
        let z = (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos();
        median * (sigma * z).exp()
    }

    fn secs(&mut self, median: f64, sigma: f64, lo: f64, hi: f64) -> Duration {
        Duration::from_secs_f64(self.lognormal(median, sigma).clamp(lo, hi))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Web,
    Mail,
    Video,
    Social,
    Agent,
    WebSearch,
}

/// Queued plan item; Think turns into an absolute `Until` when reached.
enum Plan {
    Burst(Vec<Fetch>, usize, StepMeta),
    Think(Duration),
}

/// A simulated user. `next_step(now)` says what it does next, `now` being the
/// time since it started.
pub struct Agent {
    rng: Rng,
    scale: f64,
    mixed: bool,
    mode: Mode,
    mode_until: Duration,
    script: Option<VecDeque<AppAction>>,
    queue: VecDeque<Plan>,
    /// Mail sync timer (mail profile, and in the background of "mixed").
    mail_at: Duration,
    /// Video: next chunk due, and end of the current viewing session.
    video_due: Duration,
    video_end: Duration,
    video_bytes: u64,
    /// AI agent: bytes of context the next LLM call sends, and LLM calls left
    /// in the current task.
    context: u64,
    turns_left: u32,
    placement: Placement,
    /// A cloud tool round left the provider's reply open: the next turn
    /// resumes it instead of sending a new request.
    in_call: bool,
    /// Tasks started so far, and the foreground task with its last step
    /// (background work, like mail in "mixed", is a task of its own).
    tasks: u32,
    task: u32,
    step: u32,
    /// websearch: a task has run; the user reads the answer next.
    answered: bool,
}

impl Agent {
    pub fn new(profile: &str, seed: u64, scale: f64, actions: &[AppAction]) -> Self {
        let mut rng = Rng(seed ^ 0x5DEE_CE66_D1CE_4E5B);
        let mode = match profile {
            "mail" => Mode::Mail,
            "video" => Mode::Video,
            "social" => Mode::Social,
            "agent" => Mode::Agent,
            "websearch" => Mode::WebSearch,
            _ => Mode::Web,
        };
        let mail_at = Duration::from_secs_f64(rng.uniform(5.0, 60.0));
        Self {
            rng,
            scale: if scale > 0.0 { scale } else { 1.0 },
            mixed: profile == "mixed",
            mode,
            mode_until: Duration::ZERO,
            script: (profile == "script").then(|| actions.iter().cloned().collect()),
            queue: VecDeque::new(),
            mail_at,
            video_due: Duration::ZERO,
            video_end: Duration::ZERO,
            video_bytes: 0,
            context: 0,
            turns_left: 0,
            placement: Placement::Device,
            in_call: false,
            tasks: 0,
            task: 0,
            step: 0,
            answered: false,
        }
    }

    /// Start a new foreground task.
    fn new_task(&mut self) {
        self.tasks += 1;
        self.task = self.tasks;
        self.step = 0;
    }

    /// The next step of the foreground task.
    fn meta(&mut self, kind: &'static str) -> StepMeta {
        self.step += 1;
        StepMeta {
            task: self.task,
            step: self.step,
            kind,
        }
    }

    /// A one-step task beside the foreground one.
    fn solo(&mut self, kind: &'static str) -> StepMeta {
        self.tasks += 1;
        StepMeta {
            task: self.tasks,
            step: 1,
            kind,
        }
    }

    pub fn with_placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }

    fn bytes(&mut self, median: f64, sigma: f64, lo: f64, hi: f64) -> u64 {
        (self.rng.lognormal(median, sigma).clamp(lo, hi) * self.scale).max(1.0) as u64
    }

    fn fetch(
        &mut self,
        activity: u8,
        upload: bool,
        median: f64,
        sigma: f64,
        lo: f64,
        hi: f64,
    ) -> Fetch {
        let bytes = self.bytes(median, sigma, lo, hi);
        if upload {
            Fetch::upload(activity, bytes)
        } else {
            Fetch::download(activity, bytes)
        }
    }

    /// The whole next task as one graph, for open loop: the steps up to the
    /// user's next idle pause (which is dropped — in open loop the task's
    /// arrival replaces it). A burst's objects wait for every object of the
    /// burst before (closed loop runs them that way too); a graph keeps its
    /// own dependencies. Same seed, same tasks as closed loop. `None` when
    /// the model has nothing more (a finished script).
    pub fn next_task(&mut self, now: Duration) -> Option<Vec<GraphNode>> {
        let mut nodes: Vec<GraphNode> = Vec::new();
        let mut prev: Vec<usize> = Vec::new();
        for _ in 0..1000 {
            match self.next_step(now) {
                Step::Until(_) if nodes.is_empty() => continue,
                Step::Until(_) => break,
                Step::Done => break,
                Step::Burst { fetches, meta, .. } => {
                    let first = nodes.len();
                    for fetch in fetches {
                        nodes.push(GraphNode {
                            fetch,
                            meta,
                            deps: prev.clone(),
                        });
                    }
                    prev = (first..nodes.len()).collect();
                }
                Step::Graph { nodes: g, .. } => {
                    let first = nodes.len();
                    let sinks: Vec<usize> = (0..g.len())
                        .filter(|i| !g.iter().any(|n| n.deps.contains(i)))
                        .map(|i| first + i)
                        .collect();
                    for n in g {
                        let deps = if n.deps.is_empty() {
                            prev.clone()
                        } else {
                            n.deps.iter().map(|d| first + d).collect()
                        };
                        nodes.push(GraphNode { deps, ..n });
                    }
                    prev = sinks;
                }
            }
        }
        (!nodes.is_empty()).then_some(nodes)
    }

    pub fn next_step(&mut self, now: Duration) -> Step {
        if let Some(script) = self.script.as_mut() {
            let step = match script.front() {
                None => Err(Step::Done),
                Some(a) if now < Duration::from_millis(a.at_ms) => {
                    Err(Step::Until(Duration::from_millis(a.at_ms)))
                }
                Some(_) => {
                    let a = script.pop_front().unwrap();
                    let activity = activity_code(a.activity.as_deref().unwrap_or("other"));
                    let f = if a.kind == "up" {
                        // a rate paces the upload on the phone
                        Fetch {
                            rate_bps: a.rate_kbps.unwrap_or(0).saturating_mul(1000),
                            ..Fetch::upload(activity, a.bytes.max(1))
                        }
                    } else {
                        Fetch {
                            up: a.up_bytes.unwrap_or(0),
                            delay_ms: a.delay_ms.unwrap_or(0),
                            rate_bps: a.rate_kbps.unwrap_or(0).saturating_mul(1000),
                            chunk: a.chunk_bytes.unwrap_or(0),
                            ..Fetch::download(activity, a.bytes.max(1))
                        }
                    };
                    let n = a.count.unwrap_or(1).max(1) as usize;
                    let parallel = (a.parallel.unwrap_or(1).max(1) as usize).min(MAX_PARALLEL);
                    Ok((f, n, parallel))
                }
            };
            return match step {
                Err(s) => s,
                Ok((f, n, parallel)) => Step::Burst {
                    fetches: vec![f; n],
                    parallel,
                    meta: self.solo("script_action"),
                },
            };
        }
        // "mixed": mail syncs in the background whatever the user is doing
        if self.mixed && self.mode != Mode::Mail && now >= self.mail_at {
            return self.mail(now);
        }
        if let Some(p) = self.queue.pop_front() {
            return match p {
                Plan::Burst(fetches, parallel, meta) => Step::Burst {
                    fetches,
                    parallel,
                    meta,
                },
                Plan::Think(d) => Step::Until(now + d),
            };
        }
        if self.mixed && now >= self.mode_until {
            let r = self.rng.unit();
            self.mode = if r < 0.45 {
                Mode::Web
            } else if r < 0.80 {
                Mode::Social
            } else {
                Mode::Video
            };
            self.mode_until = now + Duration::from_secs_f64(self.rng.uniform(120.0, 300.0));
            self.video_end = Duration::ZERO;
        }
        match self.mode {
            Mode::Web => self.web_page(),
            Mode::Mail => {
                if now >= self.mail_at {
                    self.mail(now)
                } else {
                    Step::Until(self.mail_at)
                }
            }
            Mode::Video => self.video(now),
            Mode::Social => self.feed(),
            Mode::Agent => self.agent(now),
            Mode::WebSearch => self.websearch(now),
        }
    }

    /// An AI agent at work: a task is a loop of LLM calls. Each call sends the
    /// whole context up (prompt caching saves compute, not bytes), waits for
    /// the first token, then streams the reply. Unless it is the task's last
    /// turn, the reply asks for tools — MCP calls and web fetches, run in
    /// parallel — whose results join the context for the next turn. Tools on
    /// the phone end the call: their results go up with the next one. Tools in
    /// the cloud run inside it: the reply pauses for the slowest, carries the
    /// results' text down and resumes with the next turn (`llm_resume`), no
    /// new request. Between tasks the user reads the answer and types the
    /// next request.
    fn agent(&mut self, now: Duration) -> Step {
        if self.turns_left == 0 {
            if self.context > 0 {
                self.context = 0;
                return Step::Until(now + self.rng.secs(45.0, 0.7, 5.0, 300.0));
            }
            self.turns_left = self.rng.lognormal(6.0, 0.6).clamp(1.0, 30.0) as u32;
            self.new_task();
            // system prompt, tool definitions and the user's request
            self.context = self.rng.lognormal(12e3, 0.4).clamp(4e3, 60e3) as u64;
            self.in_call = false;
        }
        self.turns_left -= 1;
        let tokens = self.rng.lognormal(250.0, 0.9).clamp(20.0, 3000.0);
        let tokens_per_s = self.rng.uniform(40.0, 90.0);
        let stream = Fetch {
            down: (((tokens * SSE_BYTES_PER_TOKEN + 1200.0) * self.scale) as u64).max(1),
            rate_bps: (tokens_per_s * SSE_BYTES_PER_TOKEN * 8.0) as u32,
            chunk: SSE_EVENT,
            ..Fetch::download(LLM_CALL, 0)
        };
        let llm = if self.in_call {
            // the provider processes the tool results, then generates on
            Fetch {
                activity: LLM_RESUME,
                delay_ms: (self.rng.lognormal(0.5, 0.5).clamp(0.1, 5.0) * 1000.0) as u32,
                ..stream
            }
        } else {
            Fetch {
                up: ((self.context as f64 * self.scale) as u64).max(1),
                // time to first token
                delay_ms: (self.rng.lognormal(0.9, 0.5).clamp(0.25, 10.0) * 1000.0) as u32,
                ..stream
            }
        };
        self.context += tokens as u64 * CONTEXT_BYTES_PER_TOKEN;
        self.in_call = false;
        let meta = self.meta("llm_turn");
        if self.turns_left > 0 {
            let r = self.rng.unit();
            let n = if r < 0.6 {
                1
            } else if r < 0.85 {
                2
            } else {
                3 + (self.rng.next() % 2) as usize
            };
            let tools: Vec<Fetch> = (0..n).map(|_| self.tool()).collect();
            for t in &tools {
                self.context += t.down.min(TOOL_RESULT_MAX) + 300;
            }
            if self.context > COMPACT_AT {
                self.context = COMPACT_TO;
            }
            let (here, cloud): (Vec<Fetch>, Vec<Fetch>) =
                tools.into_iter().partition(|t| self.placement.on_device(t));
            if !cloud.is_empty() {
                let result = Fetch {
                    delay_ms: cloud.iter().map(|t| t.delay_ms).max().unwrap_or(0),
                    ..Fetch::download(
                        CLOUD_TOOL,
                        cloud
                            .iter()
                            .map(|t| t.down.min(TOOL_RESULT_MAX) + 300)
                            .sum(),
                    )
                };
                let m = self.meta("cloud_tools");
                self.queue.push_back(Plan::Burst(vec![result], 1, m));
            }
            if here.is_empty() {
                self.in_call = true;
            } else {
                let n = here.len();
                let m = self.meta("tools");
                self.queue.push_back(Plan::Burst(here, n, m));
            }
        }
        Step::Burst {
            fetches: vec![llm],
            parallel: 1,
            meta,
        }
    }

    /// An LLM call's reply stream after `ttft_s` (time to first token):
    /// `tokens` at 40–90 tokens/s in server-sent events, `up` bytes sent
    /// first (the context; 0 for a resumed reply).
    fn llm(&mut self, activity: u8, up: u64, ttft_s: f64, tokens: f64) -> Fetch {
        let tokens_per_s = self.rng.uniform(40.0, 90.0);
        Fetch {
            up: if up == 0 {
                0
            } else {
                ((up as f64 * self.scale) as u64).max(1)
            },
            delay_ms: (ttft_s * 1000.0) as u32,
            down: (((tokens * SSE_BYTES_PER_TOKEN + 1200.0) * self.scale) as u64).max(1),
            rate_bps: (tokens_per_s * SSE_BYTES_PER_TOKEN * 8.0) as u32,
            chunk: SSE_EVENT,
            ..Fetch::download(activity, 0)
        }
    }

    /// An AI agent answering by web search: per task one graph — an LLM call
    /// that plans the search, a search API call, 2–6 page fetches in
    /// parallel, an LLM call that reads them (their text goes up with the
    /// context) and streams the answer; 30 % of tasks search a second round
    /// first. Placement: device runs the search and the fetches on the
    /// phone; cloud runs both at the provider inside the call (a pause, then
    /// their text down: `cloud_tool`, and the reply resumes: `llm_resume`);
    /// hybrid searches at the provider and fetches the pages on the phone.
    /// Between tasks the user reads the answer (5–60 s).
    fn websearch(&mut self, now: Duration) -> Step {
        if self.answered {
            self.answered = false;
            return Step::Until(now + self.rng.secs(15.0, 0.7, 5.0, 60.0));
        }
        self.answered = true;
        self.new_task();
        let mut nodes: Vec<GraphNode> = Vec::new();
        let add = |nodes: &mut Vec<GraphNode>, fetch, meta, deps: Vec<usize>| {
            nodes.push(GraphNode { fetch, meta, deps });
            nodes.len() - 1
        };
        let mut context = self.rng.lognormal(8e3, 0.4).clamp(3e3, 30e3) as u64;
        let ttft = self.rng.lognormal(0.9, 0.5).clamp(0.25, 10.0);
        let tokens = self.rng.lognormal(120.0, 0.5).clamp(30.0, 600.0);
        let plan = self.llm(LLM_CALL, context, ttft, tokens);
        context += tokens as u64 * CONTEXT_BYTES_PER_TOKEN;
        let m = self.meta("llm_turn");
        let mut last = add(&mut nodes, plan, m, vec![]);
        let rounds = if self.rng.chance(0.3) { 2 } else { 1 };
        for round in 0..rounds {
            let search = Fetch {
                up: 600,
                delay_ms: (self.rng.lognormal(0.6, 0.5).clamp(0.15, 4.0) * 1000.0) as u32,
                ..Fetch::download(WEB_SEARCH, self.bytes(30e3, 0.5, 5e3, 200e3))
            };
            let n = 2 + (self.rng.next() % 5) as usize;
            let pages: Vec<Fetch> = (0..n)
                .map(|_| Fetch {
                    up: 600,
                    delay_ms: (self.rng.lognormal(0.25, 0.6).clamp(0.03, 3.0) * 1000.0) as u32,
                    ..Fetch::download(WEB_FETCH, self.bytes(150e3, 1.0, 5e3, 3e6))
                })
                .collect();
            let text = |f: &Fetch| f.down.min(TOOL_RESULT_MAX) + 300;
            let found = text(&search) + pages.iter().map(text).sum::<u64>();
            let final_round = round + 1 == rounds;
            let ttft = self.rng.lognormal(1.2, 0.5).clamp(0.3, 12.0);
            let tokens = if final_round {
                self.rng.lognormal(400.0, 0.6).clamp(50.0, 3000.0)
            } else {
                self.rng.lognormal(100.0, 0.5).clamp(20.0, 600.0)
            };
            let slowest = pages.iter().map(|p| p.delay_ms).max().unwrap_or(0);
            match self.placement {
                Placement::Device => {
                    let m = self.meta("search");
                    let s = add(&mut nodes, search, m, vec![last]);
                    let m = self.meta("fetch");
                    let fetched: Vec<usize> = pages
                        .into_iter()
                        .map(|p| add(&mut nodes, p, m, vec![s]))
                        .collect();
                    context += found;
                    let read = self.llm(LLM_CALL, context, ttft, tokens);
                    let m = self.meta("llm_turn");
                    last = add(&mut nodes, read, m, fetched);
                }
                Placement::Cloud => {
                    // the provider searches and fetches inside the call
                    let result = Fetch {
                        delay_ms: search.delay_ms + slowest,
                        ..Fetch::download(CLOUD_TOOL, found)
                    };
                    let m = self.meta("cloud_tools");
                    let r = add(&mut nodes, result, m, vec![last]);
                    let resume = Fetch {
                        activity: LLM_RESUME,
                        ..self.llm(LLM_CALL, 0, ttft * 0.5, tokens)
                    };
                    let m = self.meta("llm_turn");
                    last = add(&mut nodes, resume, m, vec![r]);
                }
                Placement::Hybrid => {
                    // the provider searches; the phone fetches the pages
                    let result = Fetch {
                        delay_ms: search.delay_ms,
                        ..Fetch::download(CLOUD_TOOL, text(&search))
                    };
                    let m = self.meta("cloud_tools");
                    let r = add(&mut nodes, result, m, vec![last]);
                    let m = self.meta("fetch");
                    let fetched: Vec<usize> = pages
                        .into_iter()
                        .map(|p| add(&mut nodes, p, m, vec![r]))
                        .collect();
                    context += found;
                    let read = self.llm(LLM_CALL, context, ttft, tokens);
                    let m = self.meta("llm_turn");
                    last = add(&mut nodes, read, m, fetched);
                }
            }
            context += tokens as u64 * CONTEXT_BYTES_PER_TOKEN;
        }
        Step::Graph {
            nodes,
            parallel: MAX_PARALLEL,
        }
    }

    /// One tool call: an MCP request (small JSON up, the tool's latency, a JSON
    /// result) or a web fetch (GET, the site's first-byte time, the page).
    fn tool(&mut self) -> Fetch {
        if self.rng.chance(0.5) {
            Fetch {
                up: self.bytes(1.2e3, 0.6, 200.0, 20e3),
                delay_ms: (self.rng.lognormal(0.4, 0.8).clamp(0.05, 10.0) * 1000.0) as u32,
                ..Fetch::download(MCP_CALL, self.bytes(4e3, 1.2, 200.0, 300e3))
            }
        } else {
            Fetch {
                up: 600,
                delay_ms: (self.rng.lognormal(0.25, 0.6).clamp(0.03, 3.0) * 1000.0) as u32,
                ..Fetch::download(WEB_FETCH, self.bytes(150e3, 1.0, 5e3, 3e6))
            }
        }
    }

    /// A page view: the HTML alone, then its objects over parallel
    /// connections, then the user reads.
    fn web_page(&mut self) -> Step {
        self.new_task();
        let meta = self.meta("page_html");
        let html = self.fetch(WEB_PAGE, false, 50e3, 0.6, 5e3, 500e3);
        let n = self.rng.lognormal(30.0, 0.5).clamp(5.0, 120.0) as usize;
        let objects = (0..n)
            .map(|_| self.fetch(WEB_OBJECT, false, 20e3, 1.2, 200.0, 3e6))
            .collect();
        let m = self.meta("page_objects");
        self.queue.push_back(Plan::Burst(objects, MAX_PARALLEL, m));
        let read = self.rng.secs(12.0, 0.7, 2.0, 90.0);
        self.queue.push_back(Plan::Think(read));
        Step::Burst {
            fetches: vec![html],
            parallel: 1,
            meta,
        }
    }

    /// Mail sync: headers, sometimes an attachment, rarely a sent mail.
    fn mail(&mut self, now: Duration) -> Step {
        let mut fetches = vec![self.fetch(MAIL_SYNC, false, 15e3, 0.8, 1e3, 200e3)];
        if self.rng.chance(0.15) {
            fetches.push(self.fetch(MAIL_ATTACHMENT, false, 300e3, 1.0, 10e3, 10e6));
        }
        if self.rng.chance(0.05) {
            fetches.push(self.fetch(MAIL_SEND, true, 150e3, 1.2, 2e3, 10e6));
        }
        self.mail_at = now + Duration::from_secs_f64(self.rng.uniform(60.0, 180.0));
        Step::Burst {
            fetches,
            parallel: 1,
            meta: self.solo("mail_sync"),
        }
    }

    /// Segmented video: a few segments back to back to fill the buffer, then
    /// one every SEGMENT; sessions of 1–5 minutes with pauses between.
    fn video(&mut self, now: Duration) -> Step {
        if now >= self.video_end {
            if self.video_end > Duration::ZERO {
                self.video_end = Duration::ZERO;
                let pause = self.rng.secs(30.0, 0.6, 5.0, 120.0);
                return Step::Until(now + pause);
            }
            let mbps = [1.5, 3.0, 5.0][(self.rng.next() % 3) as usize];
            self.video_bytes =
                ((mbps * 1e6 / 8.0 * SEGMENT.as_secs_f64()) * self.scale).max(1.0) as u64;
            self.video_end = now + Duration::from_secs_f64(self.rng.uniform(60.0, 300.0));
            self.video_due = now + SEGMENT * 3;
            let f = Fetch::download(VIDEO_CHUNK, self.video_bytes);
            self.new_task();
            return Step::Burst {
                fetches: vec![f; 3],
                parallel: 1,
                meta: self.meta("video_start"),
            };
        }
        if now < self.video_due {
            return Step::Until(self.video_due.min(self.video_end));
        }
        // a late segment (the buffer ran dry) is fetched at once, not caught up
        self.video_due = (self.video_due + SEGMENT).max(now);
        let f = Fetch::download(VIDEO_CHUNK, self.video_bytes);
        Step::Burst {
            fetches: vec![f],
            parallel: 1,
            meta: self.meta("video_segment"),
        }
    }

    /// Feed scrolling: a few images, a pause, now and then a photo upload.
    fn feed(&mut self) -> Step {
        let n = self.rng.uniform(3.0, 9.0) as usize;
        let mut fetches: Vec<Fetch> = (0..n)
            .map(|_| self.fetch(FEED_IMAGE, false, 120e3, 0.8, 5e3, 2e6))
            .collect();
        if self.rng.chance(0.03) {
            fetches.push(self.fetch(PHOTO_UPLOAD, true, 2.5e6, 0.4, 300e3, 12e6));
        }
        let pause = self.rng.secs(4.0, 0.6, 1.0, 20.0);
        self.queue.push_back(Plan::Think(pause));
        self.new_task();
        Step::Burst {
            fetches,
            parallel: 4,
            meta: self.meta("feed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps(profile: &str, seed: u64, secs: u64) -> (u64, u64, Vec<Step>) {
        // walk the agent through `secs` of simulated time, transfers taking 0 s
        let mut a = Agent::new(profile, seed, 1.0, &[]);
        let (mut now, mut down, mut up, mut out) = (Duration::ZERO, 0, 0, vec![]);
        while now < Duration::from_secs(secs) && out.len() < 100_000 {
            let s = a.next_step(now);
            match &s {
                Step::Until(t) => {
                    assert!(*t > now, "{profile}: idle step does not advance time");
                    now = *t;
                }
                Step::Burst {
                    fetches, parallel, ..
                } => {
                    assert!(!fetches.is_empty() && *parallel >= 1 && *parallel <= MAX_PARALLEL);
                    for f in fetches {
                        up += f.up;
                        down += f.down;
                    }
                }
                Step::Graph { nodes, parallel } => {
                    assert!(!nodes.is_empty() && *parallel >= 1 && *parallel <= MAX_PARALLEL);
                    for n in nodes {
                        up += n.fetch.up;
                        down += n.fetch.down;
                    }
                }
                Step::Done => break,
            }
            out.push(s);
        }
        (down, up, out)
    }

    #[test]
    fn agents_are_reproducible_and_plausible() {
        assert_eq!(steps("mixed", 7, 600).2, steps("mixed", 7, 600).2);
        assert_ne!(steps("mixed", 7, 600).2, steps("mixed", 8, 600).2);
        // average downlink rates over 10 minutes, Mbit/s
        let rate = |p: &str| steps(p, 1, 600).0 as f64 * 8.0 / 600.0 / 1e6;
        let (web, mail, video, social) = (rate("web"), rate("mail"), rate("video"), rate("social"));
        assert!((0.1..3.0).contains(&web), "web {web}");
        assert!(mail < 0.1, "mail {mail}");
        assert!((0.8..5.0).contains(&video), "video {video}");
        assert!((0.1..3.0).contains(&social), "social {social}");
    }

    #[test]
    fn web_page_is_html_then_parallel_objects_then_reading() {
        let mut a = Agent::new("web", 3, 1.0, &[]);
        let Step::Burst {
            fetches,
            parallel: 1,
            ..
        } = a.next_step(Duration::ZERO)
        else {
            panic!()
        };
        assert_eq!(fetches.len(), 1);
        assert_eq!(fetches[0].activity, WEB_PAGE);
        let Step::Burst {
            fetches, parallel, ..
        } = a.next_step(Duration::ZERO)
        else {
            panic!()
        };
        assert!(
            fetches.len() >= 5
                && fetches
                    .iter()
                    .all(|f| f.activity == WEB_OBJECT && f.up == 0)
        );
        assert_eq!(parallel, MAX_PARALLEL);
        let Step::Until(t) = a.next_step(Duration::from_secs(1)) else {
            panic!()
        };
        assert!(t >= Duration::from_secs(3));
    }

    #[test]
    fn script_runs_actions_in_order_then_stops() {
        let actions = vec![
            AppAction {
                at_ms: 0,
                kind: "down".into(),
                bytes: 1000,
                count: Some(3),
                parallel: Some(9),
                activity: Some("web_object".into()),
                ..AppAction::default()
            },
            AppAction {
                at_ms: 5000,
                kind: "up".into(),
                bytes: 2000,
                ..AppAction::default()
            },
            AppAction {
                at_ms: 6000,
                kind: "down".into(),
                bytes: 3000,
                up_bytes: Some(500),
                delay_ms: Some(800),
                rate_kbps: Some(24),
                activity: Some("llm_call".into()),
                ..AppAction::default()
            },
        ];
        let mut a = Agent::new("script", 0, 1.0, &actions);
        // each action is a task of its own
        let action = |task| StepMeta {
            task,
            step: 1,
            kind: "script_action",
        };
        assert_eq!(
            a.next_step(Duration::ZERO),
            Step::Burst {
                fetches: vec![Fetch::download(WEB_OBJECT, 1000); 3],
                parallel: MAX_PARALLEL,
                meta: action(1),
            }
        );
        assert_eq!(
            a.next_step(Duration::from_secs(1)),
            Step::Until(Duration::from_secs(5))
        );
        assert_eq!(
            a.next_step(Duration::from_secs(5)),
            Step::Burst {
                fetches: vec![Fetch::upload(0, 2000)],
                parallel: 1,
                meta: action(2),
            }
        );
        let llm = Fetch {
            up: 500,
            delay_ms: 800,
            rate_bps: 24_000,
            ..Fetch::download(LLM_CALL, 3000)
        };
        assert_eq!(
            a.next_step(Duration::from_secs(6)),
            Step::Burst {
                fetches: vec![llm],
                parallel: 1,
                meta: action(3),
            }
        );
        assert_eq!(a.next_step(Duration::from_secs(7)), Step::Done);
    }

    /// (llm_call, llm_resume, mcp, web_fetch, cloud_tool) objects and bytes up
    /// over half an hour of an agent with tools placed `p`.
    fn agent_mix(p: Placement) -> ([usize; 5], u64, u64) {
        let mut a = Agent::new("agent", 9, 1.0, &[]).with_placement(p);
        let (mut now, mut n, mut up, mut down) = (Duration::ZERO, [0usize; 5], 0, 0);
        while now < Duration::from_secs(1800) {
            match a.next_step(now) {
                Step::Burst { fetches, .. } => {
                    for f in &fetches {
                        let i = [LLM_CALL, LLM_RESUME, MCP_CALL, WEB_FETCH, CLOUD_TOOL]
                            .iter()
                            .position(|c| *c == f.activity)
                            .unwrap();
                        n[i] += 1;
                        up += f.up;
                        down += f.down;
                        now += f.server_time();
                    }
                }
                Step::Graph { .. } => unreachable!("agent emits bursts"),
                Step::Until(t) => now = t,
                Step::Done => unreachable!(),
            }
        }
        (n, up, down)
    }

    #[test]
    fn tool_placement_moves_tool_traffic_off_the_phone() {
        let (dev, dev_up, dev_down) = agent_mix(Placement::Device);
        assert!(
            dev[2] > 0 && dev[3] > 0 && dev[1] == 0 && dev[4] == 0,
            "device {dev:?}"
        );
        let (cloud, cloud_up, cloud_down) = agent_mix(Placement::Cloud);
        assert!(
            cloud[2] == 0 && cloud[3] == 0 && cloud[1] > 0 && cloud[4] > 0,
            "cloud {cloud:?}"
        );
        // one request per task: no context resent within it
        assert!(cloud[0] < cloud[1], "cloud {cloud:?}");
        assert!(
            cloud_up < dev_up / 3,
            "cloud up {cloud_up} vs device {dev_up}"
        );
        assert!(
            cloud_down < dev_down / 2,
            "cloud down {cloud_down} vs device {dev_down}"
        );
        let (hyb, _, _) = agent_mix(Placement::Hybrid);
        assert!(hyb[2] > 0 && hyb[3] == 0 && hyb[4] > 0, "hybrid {hyb:?}");
    }

    #[test]
    fn agent_alternates_llm_calls_and_tools_with_a_growing_context() {
        let mut a = Agent::new("agent", 4, 1.0, &[]);
        let (mut now, mut last_up, mut tasks, mut calls) = (Duration::ZERO, 0u64, 0, 0);
        let mut prev_llm = false;
        for _ in 0..2000 {
            match a.next_step(now) {
                Step::Burst {
                    fetches, parallel, ..
                } if fetches[0].activity == LLM_CALL => {
                    let f = fetches[0];
                    assert_eq!((fetches.len(), parallel), (1, 1));
                    assert!(f.delay_ms >= 250 && f.rate_bps > 0 && f.chunk == SSE_EVENT);
                    // the context only grows within a task (until compaction)
                    assert!(
                        f.up > last_up || f.up == COMPACT_TO,
                        "call {calls}: {} after {last_up}",
                        f.up
                    );
                    last_up = f.up;
                    calls += 1;
                    prev_llm = true;
                    now += f.server_time();
                }
                Step::Burst {
                    fetches, parallel, ..
                } => {
                    assert!(prev_llm, "tools follow an LLM call");
                    assert_eq!(parallel, fetches.len());
                    assert!(fetches
                        .iter()
                        .all(|f| matches!(f.activity, MCP_CALL | WEB_FETCH)
                            && f.up > 0
                            && f.down > 0));
                    prev_llm = false;
                }
                Step::Until(t) => {
                    assert!(t > now);
                    now = t;
                    tasks += 1;
                    calls = 0;
                    last_up = 0;
                }
                Step::Graph { .. } => unreachable!("agent emits bursts"),
                Step::Done => panic!(),
            }
        }
        assert!(tasks > 10, "tasks {tasks}");
        // average: a few hundred kbit/s down, mostly LLM context up
        let (down, up, _) = steps("agent", 1, 1800);
        let (d, u) = (
            down as f64 * 8.0 / 1800.0 / 1e6,
            up as f64 * 8.0 / 1800.0 / 1e6,
        );
        assert!((0.02..1.0).contains(&d), "agent down {d} Mbit/s");
        assert!((0.02..1.0).contains(&u), "agent up {u} Mbit/s");
    }

    /// (task, step, kind) of every burst over `secs` of simulated time.
    fn metas(profile: &str, seed: u64, secs: u64) -> Vec<StepMeta> {
        steps(profile, seed, secs)
            .2
            .into_iter()
            .flat_map(|s| match s {
                Step::Burst { meta, .. } => vec![meta],
                Step::Graph { nodes, .. } => nodes.iter().map(|n| n.meta).collect(),
                _ => vec![],
            })
            .collect()
    }

    /// The task graphs of a websearch agent with tools placed `p`.
    fn graphs(p: Placement, seed: u64, n: usize) -> Vec<Vec<GraphNode>> {
        let mut a = Agent::new("websearch", seed, 1.0, &[]).with_placement(p);
        let (mut now, mut out) = (Duration::ZERO, vec![]);
        while out.len() < n {
            match a.next_step(now) {
                Step::Graph { nodes, parallel } => {
                    assert_eq!(parallel, MAX_PARALLEL);
                    out.push(nodes);
                }
                Step::Until(t) => now = t,
                s => panic!("websearch emitted {s:?}"),
            }
        }
        out
    }

    /// Device: plan → search → 2–6 fetches (each only after the search) →
    /// a reading call that waits for every fetch and sends their text up.
    #[test]
    fn websearch_is_a_search_graph() {
        let gs = graphs(Placement::Device, 3, 40);
        assert_eq!(gs, graphs(Placement::Device, 3, 40), "seeded");
        let mut second_rounds = 0;
        for g in &gs {
            assert_eq!(
                (g[0].fetch.activity, g[0].meta.kind),
                (LLM_CALL, "llm_turn")
            );
            assert!(g[0].deps.is_empty());
            let mut i = 1;
            let mut last = 0;
            while i < g.len() {
                assert_eq!(
                    (g[i].fetch.activity, g[i].deps.as_slice()),
                    (WEB_SEARCH, [last].as_slice())
                );
                let s = i;
                i += 1;
                let fetches: Vec<usize> = (i..g.len())
                    .take_while(|&k| g[k].fetch.activity == WEB_FETCH)
                    .collect();
                assert!(
                    (2..=6).contains(&fetches.len()),
                    "{} fetches",
                    fetches.len()
                );
                assert!(fetches
                    .iter()
                    .all(|&k| g[k].deps == [s] && g[k].meta.kind == "fetch"));
                i += fetches.len();
                let read = &g[i];
                assert_eq!((read.fetch.activity, &read.deps), (LLM_CALL, &fetches));
                // the pages' text goes up with the context
                assert!(read.fetch.up > g[0].fetch.up + 5_000);
                last = i;
                i += 1;
                if i < g.len() {
                    second_rounds += 1;
                }
            }
            // steps count up through the graph, one task per graph
            assert!(g
                .windows(2)
                .all(|w| w[1].meta.step >= w[0].meta.step && w[1].meta.task == w[0].meta.task));
        }
        assert!(
            (4..=24).contains(&second_rounds),
            "{second_rounds} second rounds of 40"
        );
    }

    /// Open loop takes whole tasks: the same objects in the same order as
    /// closed loop's steps (same seed), a burst waiting for the one before.
    #[test]
    fn next_task_is_closed_loops_task_as_one_graph() {
        for (profile, p) in [
            ("agent", Placement::Device),
            ("agent", Placement::Cloud),
            ("websearch", Placement::Hybrid),
        ] {
            let mut closed = Agent::new(profile, 8, 1.0, &[]).with_placement(p);
            let mut open = Agent::new(profile, 8, 1.0, &[]).with_placement(p);
            let mut now = Duration::ZERO;
            for _ in 0..30 {
                // closed loop: the steps up to the next idle pause
                let mut want: Vec<(Fetch, StepMeta)> = vec![];
                loop {
                    match closed.next_step(now) {
                        Step::Until(t) if want.is_empty() => now = t,
                        Step::Until(t) => {
                            now = t;
                            break;
                        }
                        Step::Burst { fetches, meta, .. } => {
                            want.extend(fetches.into_iter().map(|f| (f, meta)))
                        }
                        Step::Graph { nodes, .. } => {
                            want.extend(nodes.into_iter().map(|n| (n.fetch, n.meta)))
                        }
                        Step::Done => unreachable!(),
                    }
                }
                let got = open.next_task(now).unwrap();
                let got_pairs: Vec<(Fetch, StepMeta)> =
                    got.iter().map(|n| (n.fetch, n.meta)).collect();
                assert_eq!(got_pairs, want, "{profile}");
                // every node after the first waits for something earlier
                assert!(
                    got[0].deps.is_empty()
                        && got[1..]
                            .iter()
                            .all(|n| !n.deps.is_empty() && n.deps.iter().all(|&d| d < got.len()))
                );
            }
        }
    }

    /// Cloud: no search or fetch traffic from the phone — the provider's
    /// result text comes down inside the call, which then resumes.
    #[test]
    fn websearch_in_the_cloud_and_split() {
        for g in graphs(Placement::Cloud, 5, 20) {
            assert!(g
                .iter()
                .all(|n| ![WEB_SEARCH, WEB_FETCH].contains(&n.fetch.activity)));
            assert_eq!(g[1].fetch.activity, CLOUD_TOOL);
            assert_eq!((g[2].fetch.activity, g[2].fetch.up), (LLM_RESUME, 0));
            assert!(g.iter().skip(1).all(|n| n.deps.len() == 1));
        }
        for g in graphs(Placement::Hybrid, 5, 20) {
            assert!(g.iter().all(|n| n.fetch.activity != WEB_SEARCH));
            assert_eq!(g[1].fetch.activity, CLOUD_TOOL);
            assert!(g
                .iter()
                .any(|n| n.fetch.activity == WEB_FETCH && n.deps == [1]));
        }
    }

    /// Tasks and steps are reproducible, and attaching them changed nothing
    /// the seed decides: the transfers are the same as without them.
    #[test]
    fn tasks_and_steps_are_reproducible() {
        for p in ["web", "mail", "video", "social", "mixed", "agent"] {
            let m = metas(p, 5, 900);
            assert!(!m.is_empty(), "{p}");
            assert_eq!(m, metas(p, 5, 900), "{p}");
            for x in &m {
                assert!(x.task >= 1 && x.step >= 1, "{p}: {x:?}");
                assert!(STEP_KINDS.contains(&x.kind), "{p}: {}", x.kind);
            }
        }
    }

    /// An agent task: LLM turns with tool steps between them, steps counting
    /// up; a new task starts at step 1 with an LLM turn.
    #[test]
    fn agent_tasks_are_llm_turns_and_tools() {
        let m = metas("agent", 9, 1800);
        assert!(
            m.iter().map(|x| x.task).max().unwrap() >= 3,
            "several tasks"
        );
        for w in m.windows(2) {
            let (a, b) = (w[0], w[1]);
            if b.task == a.task {
                assert_eq!(b.step, a.step + 1, "{a:?} -> {b:?}");
            } else {
                assert_eq!((b.task, b.step, b.kind), (a.task + 1, 1, "llm_turn"));
            }
        }
        assert!(m.iter().any(|x| x.kind == "tools"));
    }

    /// A page view is one task: its HTML, then its objects.
    #[test]
    fn a_page_view_is_one_task() {
        let m = metas("web", 3, 120);
        assert_eq!((m[0].task, m[0].step, m[0].kind), (1, 1, "page_html"));
        assert_eq!((m[1].task, m[1].step, m[1].kind), (1, 2, "page_objects"));
        assert_eq!((m[2].task, m[2].step), (2, 1));
    }

    /// The burst kinds and outcomes logged are `enums.yaml`'s, in order.
    #[test]
    fn logged_kinds_and_outcomes_match_the_contract() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../contracts/enums/enums.yaml");
        let doc: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let list = |k: &str| -> Vec<String> {
            doc[k]
                .as_sequence()
                .unwrap_or_else(|| panic!("enums.yaml {k}"))
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(list("app_step_kinds"), STEP_KINDS);
        assert_eq!(
            list("app_outcomes"),
            crate::app_traffic::Outcome::ALL.map(crate::app_traffic::Outcome::as_str)
        );
        assert_eq!(
            list("app_engines"),
            crate::app_traffic::Engine::ALL.map(crate::app_traffic::Engine::as_str)
        );
    }

    #[test]
    fn activity_codes_index_the_wire_table() {
        let codes = [
            (WEB_PAGE, "web_page"),
            (WEB_OBJECT, "web_object"),
            (MAIL_SYNC, "mail_sync"),
            (MAIL_ATTACHMENT, "mail_attachment"),
            (MAIL_SEND, "mail_send"),
            (VIDEO_CHUNK, "video_chunk"),
            (FEED_IMAGE, "feed_image"),
            (PHOTO_UPLOAD, "photo_upload"),
            (LLM_CALL, "llm_call"),
            (MCP_CALL, "mcp_call"),
            (WEB_FETCH, "web_fetch"),
            (CLOUD_TOOL, "cloud_tool"),
            (LLM_RESUME, "llm_resume"),
        ];
        for (code, name) in codes {
            assert_eq!(activity_name(code), name);
        }
    }
}

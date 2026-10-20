//! The simulated users: what each profile asks for and when (seeded,
//! network-independent). `runner` turns their steps into connections.

use std::collections::VecDeque;
use std::time::Duration;

use serde::Deserialize;

/// Activity codes and names live with the wire format (the server logs them
/// too); the model's codes below index that table.
pub use crate::exchange::{activity_code, activity_name, ACTIVITIES};
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

pub const PROFILES: [&str; 7] = ["web", "mail", "video", "social", "mixed", "agent", "script"];

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
    /// "down" only: pace the response at this rate (a token stream).
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

/// What an agent does next.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Transfer these objects, at most `parallel` at a time.
    Burst {
        fetches: Vec<Fetch>,
        parallel: usize,
    },
    /// Stay idle until this long after the agent started.
    Until(Duration),
    /// Nothing more to do (script finished).
    Done,
}

/// splitmix64: small, seedable, good enough for traffic models.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
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
}

/// Queued plan item; Think turns into an absolute `Until` when reached.
enum Plan {
    Burst(Vec<Fetch>, usize),
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
}

impl Agent {
    pub fn new(profile: &str, seed: u64, scale: f64, actions: &[AppAction]) -> Self {
        let mut rng = Rng(seed ^ 0x5DEE_CE66_D1CE_4E5B);
        let mode = match profile {
            "mail" => Mode::Mail,
            "video" => Mode::Video,
            "social" => Mode::Social,
            "agent" => Mode::Agent,
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

    pub fn next_step(&mut self, now: Duration) -> Step {
        if let Some(script) = self.script.as_mut() {
            return match script.front() {
                None => Step::Done,
                Some(a) if now < Duration::from_millis(a.at_ms) => {
                    Step::Until(Duration::from_millis(a.at_ms))
                }
                Some(_) => {
                    let a = script.pop_front().unwrap();
                    let activity = activity_code(a.activity.as_deref().unwrap_or("other"));
                    let f = if a.kind == "up" {
                        Fetch::upload(activity, a.bytes.max(1))
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
                    Step::Burst {
                        fetches: vec![f; n],
                        parallel,
                    }
                }
            };
        }
        // "mixed": mail syncs in the background whatever the user is doing
        if self.mixed && self.mode != Mode::Mail && now >= self.mail_at {
            return self.mail(now);
        }
        if let Some(p) = self.queue.pop_front() {
            return match p {
                Plan::Burst(fetches, parallel) => Step::Burst { fetches, parallel },
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
                self.queue.push_back(Plan::Burst(vec![result], 1));
            }
            if here.is_empty() {
                self.in_call = true;
            } else {
                let n = here.len();
                self.queue.push_back(Plan::Burst(here, n));
            }
        }
        Step::Burst {
            fetches: vec![llm],
            parallel: 1,
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
        let html = self.fetch(WEB_PAGE, false, 50e3, 0.6, 5e3, 500e3);
        let n = self.rng.lognormal(30.0, 0.5).clamp(5.0, 120.0) as usize;
        let objects = (0..n)
            .map(|_| self.fetch(WEB_OBJECT, false, 20e3, 1.2, 200.0, 3e6))
            .collect();
        self.queue.push_back(Plan::Burst(objects, MAX_PARALLEL));
        let read = self.rng.secs(12.0, 0.7, 2.0, 90.0);
        self.queue.push_back(Plan::Think(read));
        Step::Burst {
            fetches: vec![html],
            parallel: 1,
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
            return Step::Burst {
                fetches: vec![f; 3],
                parallel: 1,
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
        Step::Burst {
            fetches,
            parallel: 4,
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
                Step::Burst { fetches, parallel } => {
                    assert!(!fetches.is_empty() && *parallel >= 1 && *parallel <= MAX_PARALLEL);
                    for f in fetches {
                        up += f.up;
                        down += f.down;
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
        } = a.next_step(Duration::ZERO)
        else {
            panic!()
        };
        assert_eq!(fetches.len(), 1);
        assert_eq!(fetches[0].activity, WEB_PAGE);
        let Step::Burst { fetches, parallel } = a.next_step(Duration::ZERO) else {
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
        assert_eq!(
            a.next_step(Duration::ZERO),
            Step::Burst {
                fetches: vec![Fetch::download(WEB_OBJECT, 1000); 3],
                parallel: MAX_PARALLEL
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
                parallel: 1
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
                parallel: 1
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
                Step::Burst { fetches, parallel } if fetches[0].activity == LLM_CALL => {
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
                Step::Burst { fetches, parallel } => {
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

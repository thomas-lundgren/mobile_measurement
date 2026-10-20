//! Application-like background traffic: simulated users ("agents") that
//! browse the web, sync mail, watch video or scroll a feed, so a test can show
//! what everyday app load does to the realtime flows running beside it.
//!
//! The UE runs the agents and opens every connection (invariant 5); the server
//! side is a plain object server (data-generator `spawn_app_server`). Each
//! request on a TCP connection asks for `size` bytes down, or announces `size`
//! bytes up (`packet::AppRequest`). Both nodes log every object as rows sharing
//! its `object_id` (the row's sequence_number), tagged with `app_activity` and
//! `app_object_bytes`: the request (SENT, 24 B), then the data (RECEIVED chunks
//! of a download; SENT chunks of an upload plus the server's RECEIVED ack). So
//! analysis gets request → first byte → completion for every object.
//!
//! Agents are seeded: what they ask for is reproducible, when it completes is
//! up to the network. `profile = "script"` runs an explicit action timeline
//! instead — the hook for an AI agent that plays a user and writes the plan.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::log_writer::{Direction, LogSink, PacketRow, PacketTiming, Protocol};
use crate::packet::AppRequest;
use crate::timestamp::Timestamp;

/// Activity codes on the wire (the index) and the names logged for them.
pub const ACTIVITIES: [&str; 9] = [
    "other",
    "web_page",
    "web_object",
    "mail_sync",
    "mail_attachment",
    "mail_send",
    "video_chunk",
    "feed_image",
    "photo_upload",
];
const WEB_PAGE: u8 = 1;
const WEB_OBJECT: u8 = 2;
const MAIL_SYNC: u8 = 3;
const MAIL_ATTACHMENT: u8 = 4;
const MAIL_SEND: u8 = 5;
const VIDEO_CHUNK: u8 = 6;
const FEED_IMAGE: u8 = 7;
const PHOTO_UPLOAD: u8 = 8;

pub const PROFILES: [&str; 6] = ["web", "mail", "video", "social", "mixed", "script"];

/// Objects in flight at once per agent (a browser's per-host connection cap).
pub const MAX_PARALLEL: usize = 6;
const CHUNK: usize = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_millis(200);
/// Give up on an object (and its connection) after this long.
const OBJECT_TIMEOUT: Duration = Duration::from_secs(120);
/// Idle connections are closed after this long, as browsers and apps do; the
/// next burst then pays for a new TCP handshake (and slow start).
const IDLE_CLOSE: Duration = Duration::from_secs(20);
/// Video segment length: one chunk is due every SEGMENT.
const SEGMENT: Duration = Duration::from_secs(4);

pub fn activity_code(name: &str) -> u8 {
    ACTIVITIES.iter().position(|a| *a == name).unwrap_or(0) as u8
}

pub fn activity_name(code: u8) -> &'static str {
    ACTIVITIES.get(code as usize).copied().unwrap_or("other")
}

/// One action of a scripted timeline (`profile = "script"`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
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
}

/// One object transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fetch {
    pub upload: bool,
    pub activity: u8,
    pub bytes: u64,
}

/// What an agent does next.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Transfer these objects, at most `parallel` at a time.
    Burst { fetches: Vec<Fetch>, parallel: usize },
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
}

impl Agent {
    pub fn new(profile: &str, seed: u64, scale: f64, actions: &[AppAction]) -> Self {
        let mut rng = Rng(seed ^ 0x5DEE_CE66_D1CE_4E5B);
        let mode = match profile {
            "mail" => Mode::Mail,
            "video" => Mode::Video,
            "social" => Mode::Social,
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
        }
    }

    fn bytes(&mut self, median: f64, sigma: f64, lo: f64, hi: f64) -> u64 {
        (self.rng.lognormal(median, sigma).clamp(lo, hi) * self.scale).max(1.0) as u64
    }

    fn fetch(&mut self, activity: u8, upload: bool, median: f64, sigma: f64, lo: f64, hi: f64) -> Fetch {
        Fetch { upload, activity, bytes: self.bytes(median, sigma, lo, hi) }
    }

    pub fn next_step(&mut self, now: Duration) -> Step {
        if let Some(script) = self.script.as_mut() {
            return match script.front() {
                None => Step::Done,
                Some(a) if now < Duration::from_millis(a.at_ms) => Step::Until(Duration::from_millis(a.at_ms)),
                Some(_) => {
                    let a = script.pop_front().unwrap();
                    let f = Fetch {
                        upload: a.kind == "up",
                        activity: activity_code(a.activity.as_deref().unwrap_or("other")),
                        bytes: a.bytes.max(1),
                    };
                    let n = a.count.unwrap_or(1).max(1) as usize;
                    let parallel = (a.parallel.unwrap_or(1).max(1) as usize).min(MAX_PARALLEL);
                    Step::Burst { fetches: vec![f; n], parallel }
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
            self.mode = if r < 0.45 { Mode::Web } else if r < 0.80 { Mode::Social } else { Mode::Video };
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
        }
    }

    /// A page view: the HTML alone, then its objects over parallel
    /// connections, then the user reads.
    fn web_page(&mut self) -> Step {
        let html = self.fetch(WEB_PAGE, false, 50e3, 0.6, 5e3, 500e3);
        let n = self.rng.lognormal(30.0, 0.5).clamp(5.0, 120.0) as usize;
        let objects = (0..n).map(|_| self.fetch(WEB_OBJECT, false, 20e3, 1.2, 200.0, 3e6)).collect();
        self.queue.push_back(Plan::Burst(objects, MAX_PARALLEL));
        let read = self.rng.secs(12.0, 0.7, 2.0, 90.0);
        self.queue.push_back(Plan::Think(read));
        Step::Burst { fetches: vec![html], parallel: 1 }
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
        Step::Burst { fetches, parallel: 1 }
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
            self.video_bytes = ((mbps * 1e6 / 8.0 * SEGMENT.as_secs_f64()) * self.scale).max(1.0) as u64;
            self.video_end = now + Duration::from_secs_f64(self.rng.uniform(60.0, 300.0));
            self.video_due = now + SEGMENT * 3;
            let f = Fetch { upload: false, activity: VIDEO_CHUNK, bytes: self.video_bytes };
            return Step::Burst { fetches: vec![f; 3], parallel: 1 };
        }
        if now < self.video_due {
            return Step::Until(self.video_due.min(self.video_end));
        }
        // a late segment (the buffer ran dry) is fetched at once, not caught up
        self.video_due = (self.video_due + SEGMENT).max(now);
        let f = Fetch { upload: false, activity: VIDEO_CHUNK, bytes: self.video_bytes };
        Step::Burst { fetches: vec![f], parallel: 1 }
    }

    /// Feed scrolling: a few images, a pause, now and then a photo upload.
    fn feed(&mut self) -> Step {
        let n = self.rng.uniform(3.0, 9.0) as usize;
        let mut fetches: Vec<Fetch> = (0..n).map(|_| self.fetch(FEED_IMAGE, false, 120e3, 0.8, 5e3, 2e6)).collect();
        if self.rng.chance(0.03) {
            fetches.push(self.fetch(PHOTO_UPLOAD, true, 2.5e6, 0.4, 300e3, 12e6));
        }
        let pause = self.rng.secs(4.0, 0.6, 1.0, 20.0);
        self.queue.push_back(Plan::Think(pause));
        Step::Burst { fetches, parallel: 4 }
    }
}

/// Where the runner reports transferred bytes (the engine's flow counters).
pub trait Tally: Send + Sync {
    fn sent(&self, bytes: u64);
    fn received(&self, bytes: u64);
}

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
}

struct Ctx<'a> {
    cfg: &'a AppTraffic,
    sink: &'a Mutex<LogSink>,
    tally: &'a dyn Tally,
    stop: &'a AtomicBool,
}

/// Run the flow's agents until `stop`. Each agent is a thread with its own
/// connection pool; a script runs on one agent only.
pub fn run(cfg: &AppTraffic, sink: LogSink, tally: &dyn Tally, stop: &AtomicBool) {
    let sink = Mutex::new(sink);
    let ctx = Ctx { cfg, sink: &sink, tally, stop };
    let workers = if cfg.profile == "script" { 1 } else { cfg.workers.clamp(1, 16) };
    let start = Instant::now();
    std::thread::scope(|s| {
        for w in 0..workers {
            let ctx = &ctx;
            s.spawn(move || worker(ctx, w, start));
        }
    });
}

fn worker(ctx: &Ctx, w: u32, start: Instant) {
    let seed = ctx.cfg.seed.wrapping_add((w as u64).wrapping_mul(0x9E37_79B9));
    let mut agent = Agent::new(&ctx.cfg.profile, seed, ctx.cfg.scale, &ctx.cfg.actions);
    let mut pool: Vec<TcpStream> = Vec::new();
    let mut episode = 0u64;
    while !ctx.stop.load(Ordering::Acquire) {
        match agent.next_step(start.elapsed()) {
            Step::Until(t) => {
                let idle_from = Instant::now();
                while !ctx.stop.load(Ordering::Acquire) && start.elapsed() < t {
                    std::thread::sleep((t.saturating_sub(start.elapsed())).min(Duration::from_millis(100)));
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
fn burst(ctx: &Ctx, mut pool: Vec<TcpStream>, fetches: &[Fetch], parallel: usize, base: u64) -> Vec<TcpStream> {
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
                    if ctx.stop.load(Ordering::Acquire) || transfer(ctx, &mut conn, f, base | i as u64).is_err() {
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

fn row(flow_id: u32, object_id: u64, direction: Direction, size: u32, activity: u8, object_bytes: u64) -> PacketRow {
    PacketRow {
        flow_id,
        sequence_number: object_id,
        direction,
        timestamp: Timestamp::now(),
        payload_size: size,
        protocol: Protocol::Tcp,
        local_port: None,
        remote_port: None,
        sender: None,
        expected_interval_ns: None,
        actual_interval_ns: None,
        dscp_sent: None,
        dscp_recv: None,
        timing: PacketTiming {
            app_activity: Some(activity_name(activity)),
            app_object_bytes: Some(object_bytes),
            ..PacketTiming::default()
        },
    }
}

/// Should a blocked read/write give up? (stopped, or the object took too long)
fn give_up(stop: &AtomicBool, deadline: Instant) -> bool {
    stop.load(Ordering::Acquire) || Instant::now() >= deadline
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

/// `write_all` that survives the socket's short write timeout.
pub fn write_all_until(conn: &mut TcpStream, mut buf: &[u8], stop: &AtomicBool, deadline: Instant) -> io::Result<()> {
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
pub fn read_exact_until(conn: &mut TcpStream, mut buf: &mut [u8], stop: &AtomicBool, deadline: Instant) -> io::Result<()> {
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
    let deadline = Instant::now() + OBJECT_TIMEOUT;
    let req = AppRequest {
        kind: if f.upload { AppRequest::UPLOAD } else { AppRequest::DOWNLOAD },
        activity: f.activity,
        flow_id,
        object_id,
        size: f.bytes,
    };
    let push = |r: PacketRow| {
        ctx.sink.lock().unwrap().push(r);
    };
    let head = req.to_bytes();
    push(row(flow_id, object_id, Direction::Sent, head.len() as u32, f.activity, f.bytes));
    write_all_until(conn, &head, ctx.stop, deadline)?;
    ctx.tally.sent(head.len() as u64);

    let mut buf = vec![0u8; CHUNK];
    if f.upload {
        let mut left = f.bytes;
        while left > 0 {
            let n = left.min(CHUNK as u64) as usize;
            write_all_until(conn, &buf[..n], ctx.stop, deadline)?;
            push(row(flow_id, object_id, Direction::Sent, n as u32, f.activity, f.bytes));
            ctx.tally.sent(n as u64);
            left -= n as u64;
        }
        let mut ack = [0u8; AppRequest::WIRE_LEN];
        read_exact_until(conn, &mut ack, ctx.stop, deadline)?;
        push(row(flow_id, object_id, Direction::Received, ack.len() as u32, f.activity, f.bytes));
        ctx.tally.received(ack.len() as u64);
    } else {
        let mut left = f.bytes;
        while left > 0 {
            let want = left.min(CHUNK as u64) as usize;
            match conn.read(&mut buf[..want]) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    push(row(flow_id, object_id, Direction::Received, n as u32, f.activity, f.bytes));
                    ctx.tally.received(n as u64);
                    left -= n as u64;
                }
                Err(e) if is_timeout(&e) || e.kind() == io::ErrorKind::Interrupted => {
                    if give_up(ctx.stop, deadline) {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
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
                        if f.upload { up += f.bytes } else { down += f.bytes }
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
        let Step::Burst { fetches, parallel: 1 } = a.next_step(Duration::ZERO) else { panic!() };
        assert_eq!(fetches.len(), 1);
        assert_eq!(fetches[0].activity, WEB_PAGE);
        let Step::Burst { fetches, parallel } = a.next_step(Duration::ZERO) else { panic!() };
        assert!(fetches.len() >= 5 && fetches.iter().all(|f| f.activity == WEB_OBJECT && !f.upload));
        assert_eq!(parallel, MAX_PARALLEL);
        let Step::Until(t) = a.next_step(Duration::from_secs(1)) else { panic!() };
        assert!(t >= Duration::from_secs(3));
    }

    #[test]
    fn script_runs_actions_in_order_then_stops() {
        let actions = vec![
            AppAction { at_ms: 0, kind: "down".into(), bytes: 1000, count: Some(3), parallel: Some(9), activity: Some("web_object".into()) },
            AppAction { at_ms: 5000, kind: "up".into(), bytes: 2000, count: None, parallel: None, activity: None },
        ];
        let mut a = Agent::new("script", 0, 1.0, &actions);
        assert_eq!(
            a.next_step(Duration::ZERO),
            Step::Burst { fetches: vec![Fetch { upload: false, activity: WEB_OBJECT, bytes: 1000 }; 3], parallel: MAX_PARALLEL }
        );
        assert_eq!(a.next_step(Duration::from_secs(1)), Step::Until(Duration::from_secs(5)));
        assert_eq!(
            a.next_step(Duration::from_secs(5)),
            Step::Burst { fetches: vec![Fetch { upload: true, activity: 0, bytes: 2000 }], parallel: 1 }
        );
        assert_eq!(a.next_step(Duration::from_secs(6)), Step::Done);
    }

    #[test]
    fn activity_names_round_trip() {
        for (i, name) in ACTIVITIES.iter().enumerate() {
            assert_eq!(activity_code(name) as usize, i);
            assert_eq!(activity_name(i as u8), *name);
        }
        assert_eq!(activity_code("nonsense"), 0);
    }
}

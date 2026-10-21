//! mp-probe: on-phone collector for what the Logger app cannot read itself.
//!
//! Started over adb, it runs as the shell user, which may dump qdiscs over
//! netlink, read `/proc/net/snmp` and read the radio log. A process started
//! with `setsid nohup` survives unplugging, so it keeps recording on the road
//! until the phone reboots. It records, on the phone's UTC clock (the same
//! clock as the Logger's rows), as JSON lines:
//!
//! - `{"k":"q",…}` root qdisc of every modem / Wi-Fi link carrying traffic,
//!   polled at `--hz` (default 100) while a test runs, else at `--idle-hz`
//!   (default 2), and written on change or every second:
//!   bytes/packets sent, drops, overlimits, **requeues** (the driver refused a
//!   packet because the modem stopped the queue: flow control) and backlog.
//! - `{"k":"u",…}` UDP counters from `/proc/net/snmp` (SndbufErrors: sends
//!   refused for a full socket buffer; RcvbufErrors; InErrors) on change.
//! - `{"k":"l",…}` radio-log lines: physical channel configs, link-capacity
//!   estimates, signal-strength / network-state / data-call indications.
//!
//! Lines go to hourly files `<dir>/probe-<YYYYMMDDHH>.jsonl` (kept
//! `--keep-hours`). The Logger talks to it over `127.0.0.1:<port>`:
//! `FAST <seconds>\n` → poll at `--hz` for that long (a test starts; `FAST 0`
//! when it ends), so the 100 Hz wake-ups never run outside a measurement;
//! `RANGE <t0_ns> <t1_ns>\n` → the matching lines;
//! `PING\n` → `PONG <version> <started_utc_ns>`.
//!
//! Both interfaces are specified in `contracts/internal/mp_probe.yaml`
//! (vectors in `contracts/testvectors/mp_probe/`); the tests below hold this
//! file to it.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod netlink;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const HEARTBEAT: Duration = Duration::from_secs(1);
const SNMP_EVERY: Duration = Duration::from_millis(100);
/// Longest fast-polling window one FAST command can ask for.
const FAST_MAX_S: u64 = 3600;
/// Control port unless --port says otherwise.
const DEFAULT_PORT: u16 = 47600;

fn utc_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Hour key (UTC) of a timestamp: the file a line belongs to.
fn hour_key(t_ns: u64) -> u64 {
    t_ns / 3_600_000_000_000
}

fn file_for(dir: &Path, hour: u64) -> PathBuf {
    let secs = (hour * 3600) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::gmtime_r(&secs, &mut tm) };
    dir.join(format!(
        "probe-{:04}{:02}{:02}{:02}.jsonl",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour
    ))
}

/// Appends lines to the current hour's file; shared with the server, which
/// flushes it before answering so a fetch sees everything up to now.
struct Store {
    dir: PathBuf,
    hour: u64,
    out: Option<BufWriter<File>>,
    keep_hours: u64,
}

impl Store {
    fn write(&mut self, t_ns: u64, line: &str) {
        let h = hour_key(t_ns);
        if self.out.is_none() || h != self.hour {
            if let Some(mut w) = self.out.take() {
                let _ = w.flush();
            }
            self.hour = h;
            self.out = OpenOptions::new()
                .create(true)
                .append(true)
                .open(file_for(&self.dir, h))
                .ok()
                .map(BufWriter::new);
            self.prune(h);
        }
        if let Some(w) = self.out.as_mut() {
            let _ = writeln!(w, "{line}");
        }
    }

    fn flush(&mut self) {
        if let Some(w) = self.out.as_mut() {
            let _ = w.flush();
        }
    }

    fn prune(&self, now_hour: u64) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(stamp) = name
                .strip_prefix("probe-")
                .and_then(|s| s.strip_suffix(".jsonl"))
            else {
                continue;
            };
            // keep anything we cannot date; delete hours older than keep_hours
            if let Some(age) = hours_since(stamp, now_hour) {
                if age > self.keep_hours {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }
}

fn hours_since(stamp: &str, now_hour: u64) -> Option<u64> {
    if stamp.len() != 10 {
        return None;
    }
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = stamp[0..4].parse::<i32>().ok()? - 1900;
    tm.tm_mon = stamp[4..6].parse::<i32>().ok()? - 1;
    tm.tm_mday = stamp[6..8].parse().ok()?;
    tm.tm_hour = stamp[8..10].parse().ok()?;
    let secs = unsafe { libc::timegm(&mut tm) };
    Some(now_hour.saturating_sub(secs as u64 / 3600))
}

type Shared = Arc<Mutex<Store>>;

fn emit(store: &Shared, t: u64, v: serde_json::Value) {
    store.lock().unwrap().write(t, &v.to_string());
}

fn period(hz: u32) -> Duration {
    Duration::from_micros(1_000_000 / hz.max(1) as u64)
}

/// qdisc + UDP counters. Links named rmnet* / wlan* that have sent packets.
/// Polls at `hz` until `fast_until` (UTC ns), else at `idle_hz`.
fn poll_loop(store: Shared, hz: u32, idle_hz: u32, fast_until: Arc<AtomicU64>) {
    let mut nl = match netlink::QdiscDumper::new() {
        Ok(n) => Some(n),
        Err(e) => {
            eprintln!("netlink unavailable ({e}); qdisc stats off");
            None
        }
    };
    let mut last_q: HashMap<String, ([u64; 5], Instant)> = HashMap::new();
    let mut last_u: Option<(Vec<u64>, Instant)> = None;
    let mut next_snmp = Instant::now();
    let mut next = Instant::now();
    loop {
        let t = utc_ns();
        if let Some(d) = nl.as_mut() {
            if let Ok(qs) = d.dump_roots() {
                for q in qs {
                    if !(q.dev.starts_with("rmnet") || q.dev.starts_with("wlan")) || q.packets == 0
                    {
                        continue;
                    }
                    let key = [
                        q.packets as u64,
                        q.requeues as u64,
                        q.drops as u64,
                        q.backlog as u64,
                        q.qlen as u64,
                    ];
                    let fresh = match last_q.get(&q.dev) {
                        Some((k, at)) => *k != key || at.elapsed() >= HEARTBEAT,
                        None => true,
                    };
                    if fresh {
                        emit(&store, t, qdisc_record(t, &q));
                        last_q.insert(q.dev.clone(), (key, Instant::now()));
                    }
                }
            }
        }
        if Instant::now() >= next_snmp {
            next_snmp = Instant::now() + SNMP_EVERY;
            if let Some(u) = read_udp_snmp() {
                let vals: Vec<u64> = u.values().copied().collect();
                let fresh = match &last_u {
                    Some((v, at)) => *v != vals || at.elapsed() >= HEARTBEAT * 10,
                    None => true,
                };
                if fresh {
                    emit(&store, t, udp_record(t, &u));
                    last_u = Some((vals, Instant::now()));
                }
            }
        }
        next += if t < fast_until.load(Ordering::Relaxed) {
            period(hz)
        } else {
            period(idle_hz)
        };
        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}

/// UDP counters from /proc/net/snmp (the "Udp:" header + value lines), in
/// a stable order.
fn read_udp_snmp() -> Option<std::collections::BTreeMap<String, u64>> {
    parse_udp_snmp(&fs::read_to_string("/proc/net/snmp").ok()?)
}

fn parse_udp_snmp(text: &str) -> Option<std::collections::BTreeMap<String, u64>> {
    let mut lines = text.lines().filter(|l| l.starts_with("Udp:"));
    let head: Vec<&str> = lines.next()?.split_whitespace().skip(1).collect();
    let vals: Vec<&str> = lines.next()?.split_whitespace().skip(1).collect();
    Some(
        head.iter()
            .zip(vals)
            .filter_map(|(k, v)| Some((k.to_string(), v.parse().ok()?)))
            .collect(),
    )
}

/// Radio-log events worth keeping (parsed on the server side).
fn keep_log_line(line: &str) -> bool {
    const TAGS: [&str; 5] = [
        "UNSOL_PHYSICAL_CHANNEL_CONFIG",
        "UNSOL_LCE_INFO_RECV",
        "UNSOL_SIGNAL_STRENGTH",
        "UNSOL_DATA_CALL_LIST_CHANGED",
        "UNSOL_RESPONSE_NETWORK_STATE_CHANGED",
    ];
    (line.contains("[UNSL]") || line.contains("UNSOLICITED"))
        && TAGS.iter().any(|t| line.contains(t))
}

/// `logcat -v epoch` line → its UTC ns timestamp.
fn log_time(line: &str) -> Option<u64> {
    let ts = line.split_whitespace().next()?;
    let (s, frac) = ts.split_once('.')?;
    let ns: u64 = format!("{:0<9}", frac).get(..9)?.parse().ok()?;
    Some(s.parse::<u64>().ok()? * 1_000_000_000 + ns)
}

fn logcat_loop(store: Shared) {
    loop {
        let child = Command::new("logcat")
            .args(["-b", "radio", "-v", "epoch", "-T", "1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut c) = child {
            if let Some(out) = c.stdout.take() {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if keep_log_line(&line) {
                        let t = log_time(&line).unwrap_or_else(utc_ns);
                        emit(&store, t, log_record(t, &line));
                    }
                }
            }
            let _ = c.wait();
        }
        thread::sleep(Duration::from_secs(5)); // logcat ended: restart
    }
}

/// Lines with t0 <= t <= t1 from the hour files covering the window.
fn range(dir: &Path, t0: u64, t1: u64, out: &mut impl Write) -> io::Result<()> {
    for h in hour_key(t0)..=hour_key(t1) {
        let Ok(f) = File::open(file_for(dir, h)) else {
            continue;
        };
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            let t = line
                .strip_prefix("{\"t\":")
                .or_else(|| line.find("\"t\":").map(|i| &line[i + 4..]))
                .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|s| s.parse::<u64>().ok());
            if matches!(t, Some(t) if t >= t0 && t <= t1) {
                writeln!(out, "{line}")?;
            }
        }
    }
    Ok(())
}

/// A request on the control port (`contracts/internal/mp_probe.yaml`).
#[derive(Debug, PartialEq)]
enum Cmd {
    Ping,
    /// Fast-polling window in seconds, capped at `FAST_MAX_S`.
    Fast(u64),
    /// Records with t0 <= t <= t1 (UTC ns).
    Range(u64, u64),
}

/// One request line; None for anything malformed (no reply).
fn parse_command(line: &str) -> Option<Cmd> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    match parts.as_slice() {
        ["PING"] => Some(Cmd::Ping),
        ["FAST", secs] => Some(Cmd::Fast(secs.parse::<u64>().ok()?.min(FAST_MAX_S))),
        ["RANGE", a, b] => Some(Cmd::Range(a.parse().ok()?, b.parse().ok()?)),
        _ => None,
    }
}

// --- records (contracts/internal/mp_probe.yaml `records`) --------------------

fn start_record(t: u64, hz: u32, idle_hz: u32) -> serde_json::Value {
    serde_json::json!({"t": t, "k": "start", "version": VERSION, "hz": hz, "idle_hz": idle_hz})
}

fn fast_record(t: u64, secs: u64) -> serde_json::Value {
    serde_json::json!({"t": t, "k": "fast", "s": secs})
}

fn qdisc_record(t: u64, q: &netlink::Qdisc) -> serde_json::Value {
    serde_json::json!({
        "t": t, "k": "q", "dev": q.dev, "qdisc": q.kind, "b": q.bytes, "p": q.packets,
        "d": q.drops, "o": q.overlimits, "r": q.requeues, "bb": q.backlog, "bp": q.qlen,
    })
}

fn udp_record(t: u64, counters: &std::collections::BTreeMap<String, u64>) -> serde_json::Value {
    let mut obj = serde_json::json!({"t": t, "k": "u"});
    for (k, v) in counters {
        obj[k.as_str()] = serde_json::json!(v);
    }
    obj
}

fn log_record(t: u64, line: &str) -> serde_json::Value {
    serde_json::json!({"t": t, "k": "l", "line": line.trim()})
}

fn serve(store: Shared, port: u16, started: u64, fast_until: Arc<AtomicU64>) {
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot listen on 127.0.0.1:{port}: {e}");
            return;
        }
    };
    for stream in listener.incoming().flatten() {
        let store = store.clone();
        let fast_until = fast_until.clone();
        thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut line = String::new();
            let mut rd = BufReader::new(&stream);
            if rd.read_line(&mut line).is_err() {
                return;
            }
            let mut w = BufWriter::new(&stream);
            match parse_command(&line) {
                Some(Cmd::Ping) => {
                    let _ = writeln!(w, "PONG {VERSION} {started}");
                }
                Some(Cmd::Fast(secs)) => {
                    let t = utc_ns();
                    fast_until.store(t + secs * 1_000_000_000, Ordering::Relaxed);
                    emit(&store, t, fast_record(t, secs));
                    let _ = writeln!(w, "OK");
                }
                Some(Cmd::Range(t0, t1)) => {
                    let dir = {
                        let mut s = store.lock().unwrap();
                        s.flush();
                        s.dir.clone()
                    };
                    let _ = range(&dir, t0, t1, &mut w);
                }
                None => {}
            }
            let _ = w.flush();
        });
    }
}

fn arg(args: &[String], name: &str, default: &str) -> String {
    args.windows(2)
        .find(|w| w[0] == name)
        .map(|w| w[1].clone())
        .unwrap_or_else(|| default.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--once") {
        // diagnostics: one qdisc dump + UDP counters to stdout
        match netlink::QdiscDumper::new().and_then(|mut d| d.dump_roots()) {
            Ok(qs) => {
                for q in qs {
                    println!(
                        "{} {} sent {}B/{}p drops {} requeues {} backlog {}B/{}p",
                        q.dev, q.kind, q.bytes, q.packets, q.drops, q.requeues, q.backlog, q.qlen
                    );
                }
            }
            Err(e) => println!("netlink: {e}"),
        }
        println!("udp: {:?}", read_udp_snmp());
        return;
    }
    let dir = PathBuf::from(arg(&args, "--dir", "/data/local/tmp/mp/probe"));
    let port: u16 = arg(&args, "--port", &DEFAULT_PORT.to_string())
        .parse()
        .unwrap_or(DEFAULT_PORT);
    let hz: u32 = arg(&args, "--hz", "100").parse().unwrap_or(100);
    let idle_hz: u32 = arg(&args, "--idle-hz", "2").parse().unwrap_or(2);
    let keep_hours: u64 = arg(&args, "--keep-hours", "48").parse().unwrap_or(48);
    let _ = fs::create_dir_all(&dir);
    let started = utc_ns();
    let store: Shared = Arc::new(Mutex::new(Store {
        dir,
        hour: 0,
        out: None,
        keep_hours,
    }));
    emit(&store, started, start_record(started, hz, idle_hz));

    let fast_until = Arc::new(AtomicU64::new(0));
    let (s, f) = (store.clone(), fast_until.clone());
    thread::spawn(move || poll_loop(s, hz, idle_hz, f));
    let s = store.clone();
    thread::spawn(move || logcat_loop(s));
    let s = store.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        s.lock().unwrap().flush();
    });
    serve(store, port, started, fast_until);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn contracts() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../contracts")
    }

    fn contract() -> serde_yaml::Value {
        let text = fs::read_to_string(contracts().join("internal/mp_probe.yaml")).unwrap();
        serde_yaml::from_str(&text).unwrap()
    }

    /// A record against the contract's `records`: known kind, every listed
    /// key present with its type, no other key unless the kind is open.
    fn check_record(rec: &Value, contract: &serde_yaml::Value) -> Result<(), String> {
        let kind = rec["k"].as_str().ok_or("no k")?;
        let spec = &contract["records"][kind];
        let keys = spec["keys"]
            .as_mapping()
            .ok_or(format!("unknown kind {kind}"))?;
        let obj = rec.as_object().ok_or("not an object")?;
        for (key, ty) in keys {
            let (key, ty) = (key.as_str().unwrap(), ty.as_str().unwrap());
            let v = obj.get(key).ok_or(format!("{kind}: missing {key}"))?;
            let ok = match ty {
                "str" => v.is_string(),
                "u64" | "u32" => v.is_u64(),
                other => return Err(format!("{kind}: unknown type {other} in the contract")),
            };
            if !ok {
                return Err(format!("{kind}.{key}: not {ty}: {v}"));
            }
        }
        for (key, v) in obj {
            if keys.contains_key(key.as_str()) {
                continue;
            }
            match spec["open_keys"].as_str() {
                Some("u64") if v.is_u64() => {}
                _ => return Err(format!("{kind}: key {key} not in the contract")),
            }
        }
        Ok(())
    }

    #[test]
    fn port_and_commands_match_the_contract() {
        let c = contract();
        assert_eq!(c["port"].as_u64(), Some(DEFAULT_PORT as u64));
        assert_eq!(c["fast_max_s"].as_u64(), Some(FAST_MAX_S));
        let mut names = Vec::new();
        for cmd in c["commands"].as_sequence().unwrap() {
            let name = cmd["name"].as_str().unwrap();
            let args = cmd["args"].as_sequence().unwrap().len();
            names.push(name);
            let line = |n: usize| {
                std::iter::once(name.to_string())
                    .chain((0..n).map(|i| (i + 1).to_string()))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            assert!(
                parse_command(&line(args)).is_some(),
                "{name} with {args} args"
            );
            assert!(
                parse_command(&line(args + 1)).is_none(),
                "{name} with too many args"
            );
        }
        assert_eq!(names, ["PING", "FAST", "RANGE"]);
    }

    #[test]
    fn exchanges_parse_as_the_vectors_say() {
        let text =
            fs::read_to_string(contracts().join("testvectors/mp_probe/exchanges.json")).unwrap();
        let doc: Value = serde_json::from_str(&text).unwrap();
        for case in doc["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let got = parse_command(case["request"].as_str().unwrap());
            let want = if case["reply_pattern"].is_null() {
                None
            } else if let Some(s) = case["fast_s"].as_u64() {
                Some(Cmd::Fast(s))
            } else if let Some(r) = case["range"].as_array() {
                Some(Cmd::Range(r[0].as_u64().unwrap(), r[1].as_u64().unwrap()))
            } else {
                Some(Cmd::Ping)
            };
            assert_eq!(got, want, "case {name}");
        }
    }

    #[test]
    fn records_follow_the_contract() {
        let c = contract();
        let q = netlink::Qdisc {
            dev: "rmnet_data1".into(),
            kind: "pfifo_fast".into(),
            ..Default::default()
        };
        let udp = parse_udp_snmp("Udp: InDatagrams SndbufErrors\nUdp: 10 3\n").unwrap();
        let t = 1_792_556_320_000_000_000;
        let built = [
            start_record(t, 100, 2),
            fast_record(t, 95),
            qdisc_record(t, &q),
            udp_record(t, &udp),
            log_record(
                t,
                " 1792556321.068 2830 3415 D RILJ : [UNSL]< UNSOL_SIGNAL_STRENGTH ",
            ),
        ];
        let kinds: Vec<&str> = built.iter().map(|r| r["k"].as_str().unwrap()).collect();
        let contract_kinds: Vec<&str> = c["records"]
            .as_mapping()
            .unwrap()
            .keys()
            .map(|k| k.as_str().unwrap())
            .collect();
        assert_eq!(kinds, contract_kinds, "one builder per contract kind");
        for r in &built {
            check_record(r, &c).unwrap();
        }
        // the vectors' sample records too (what the analysis is tested on)
        let sample =
            fs::read_to_string(contracts().join("testvectors/mp_probe/records.jsonl")).unwrap();
        for line in sample.lines() {
            check_record(&serde_json::from_str(line).unwrap(), &c).unwrap();
        }
    }

    #[test]
    fn udp_counters_parse() {
        let text = "Ip: a b\nIp: 1 2\nUdp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors\n\
                    Udp: 10 2 3 40 5 11434611\nUdpLite: InDatagrams\nUdpLite: 0\n";
        let u = parse_udp_snmp(text).unwrap();
        assert_eq!(u["SndbufErrors"], 11434611);
        assert_eq!(u["OutDatagrams"], 40);
    }

    #[test]
    fn radio_lines_are_filtered_and_timed() {
        let pcc = "  1792472351.068  2830  3415 D RILJ    : [UNSL]< UNSOL_PHYSICAL_CHANNEL_CONFIG [{mBand=3}] [PHONE0]";
        assert!(keep_log_line(pcc));
        assert_eq!(log_time(pcc), Some(1_792_472_351_068_000_000));
        assert!(!keep_log_line(
            "1792472351.068 1 2 D Foo: onCapabilitiesChanged"
        ));
    }

    #[test]
    fn hour_files_round_trip_and_range_filters() {
        let dir = std::env::temp_dir().join(format!("mp-probe-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut s = Store {
            dir: dir.clone(),
            hour: 0,
            out: None,
            keep_hours: 48,
        };
        let t = 1_792_472_351_000_000_000u64;
        for k in 0..5u64 {
            let tk = t + k * 1_000_000_000;
            s.write(tk, &format!("{{\"t\":{tk},\"k\":\"q\"}}"));
        }
        s.flush();
        let mut out = Vec::new();
        range(&dir, t + 1_000_000_000, t + 3_000_000_000, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap().lines().count(), 3);
        let name = file_for(&dir, hour_key(t));
        let stamp = name.file_name().unwrap().to_string_lossy()[6..16].to_string();
        assert_eq!(hours_since(&stamp, hour_key(t)), Some(0));
        let _ = fs::remove_dir_all(&dir);
    }
}

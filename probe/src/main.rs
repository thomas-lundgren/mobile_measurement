//! mp-probe: on-phone collector for what the Logger app cannot read itself.
//!
//! Started over adb, it runs as the shell user, which may dump qdiscs over
//! netlink, read `/proc/net/snmp` and read the radio log. A process started
//! with `setsid nohup` survives unplugging, so it keeps recording on the road
//! until the phone reboots. It records, on the phone's UTC clock (the same
//! clock as the Logger's rows), as JSON lines:
//!
//! - `{"k":"q",…}` root qdisc of every modem / Wi-Fi link carrying traffic,
//!   polled at `--hz` (default 100) and written on change or every second:
//!   bytes/packets sent, drops, overlimits, **requeues** (the driver refused a
//!   packet because the modem stopped the queue: flow control) and backlog.
//! - `{"k":"u",…}` UDP counters from `/proc/net/snmp` (SndbufErrors: sends
//!   refused for a full socket buffer; RcvbufErrors; InErrors) on change.
//! - `{"k":"l",…}` radio-log lines: physical channel configs, link-capacity
//!   estimates, signal-strength / network-state / data-call indications.
//!
//! Lines go to hourly files `<dir>/probe-<YYYYMMDDHH>.jsonl` (kept
//! `--keep-hours`). The Logger fetches a test's window over
//! `127.0.0.1:<port>`: `RANGE <t0_ns> <t1_ns>\n` → the matching lines;
//! `PING\n` → `PONG <version> <started_utc_ns>`.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod netlink;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const HEARTBEAT: Duration = Duration::from_secs(1);
const SNMP_EVERY: Duration = Duration::from_millis(100);

fn utc_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
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
        let Ok(entries) = fs::read_dir(&self.dir) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(stamp) = name.strip_prefix("probe-").and_then(|s| s.strip_suffix(".jsonl")) else {
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

/// qdisc + UDP counters. Links named rmnet* / wlan* that have sent packets.
fn poll_loop(store: Shared, hz: u32) {
    let period = Duration::from_micros(1_000_000 / hz.max(1) as u64);
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
                    if !(q.dev.starts_with("rmnet") || q.dev.starts_with("wlan")) || q.packets == 0 {
                        continue;
                    }
                    let key = [q.packets as u64, q.requeues as u64, q.drops as u64, q.backlog as u64, q.qlen as u64];
                    let fresh = match last_q.get(&q.dev) {
                        Some((k, at)) => *k != key || at.elapsed() >= HEARTBEAT,
                        None => true,
                    };
                    if fresh {
                        emit(&store, t, serde_json::json!({
                            "t": t, "k": "q", "dev": q.dev, "qdisc": q.kind, "b": q.bytes, "p": q.packets,
                            "d": q.drops, "o": q.overlimits, "r": q.requeues, "bb": q.backlog, "bp": q.qlen,
                        }));
                        last_q.insert(q.dev.clone(), (key, Instant::now()));
                    }
                }
            }
        }
        if Instant::now() >= next_snmp {
            next_snmp += SNMP_EVERY;
            if let Some(u) = read_udp_snmp() {
                let vals: Vec<u64> = u.values().copied().collect();
                let fresh = match &last_u {
                    Some((v, at)) => *v != vals || at.elapsed() >= HEARTBEAT * 10,
                    None => true,
                };
                if fresh {
                    let mut obj = serde_json::json!({"t": t, "k": "u"});
                    for (k, v) in &u {
                        obj[k.as_str()] = serde_json::json!(v);
                    }
                    emit(&store, t, obj);
                    last_u = Some((vals, Instant::now()));
                }
            }
        }
        next += period;
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
    Some(head.iter().zip(vals).filter_map(|(k, v)| Some((k.to_string(), v.parse().ok()?))).collect())
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
    (line.contains("[UNSL]") || line.contains("UNSOLICITED")) && TAGS.iter().any(|t| line.contains(t))
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
                        emit(&store, t, serde_json::json!({"t": t, "k": "l", "line": line.trim()}));
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
        let Ok(f) = File::open(file_for(dir, h)) else { continue };
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

fn serve(store: Shared, port: u16, started: u64) {
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot listen on 127.0.0.1:{port}: {e}");
            return;
        }
    };
    for stream in listener.incoming().flatten() {
        let store = store.clone();
        thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut line = String::new();
            let mut rd = BufReader::new(&stream);
            if rd.read_line(&mut line).is_err() {
                return;
            }
            let mut w = BufWriter::new(&stream);
            let parts: Vec<&str> = line.split_whitespace().collect();
            match parts.as_slice() {
                ["PING"] => {
                    let _ = writeln!(w, "PONG {VERSION} {started}");
                }
                ["RANGE", a, b] => {
                    let (Ok(t0), Ok(t1)) = (a.parse(), b.parse()) else { return };
                    let dir = {
                        let mut s = store.lock().unwrap();
                        s.flush();
                        s.dir.clone()
                    };
                    let _ = range(&dir, t0, t1, &mut w);
                }
                _ => {}
            }
            let _ = w.flush();
        });
    }
}

fn arg(args: &[String], name: &str, default: &str) -> String {
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone()).unwrap_or_else(|| default.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--once") {
        // diagnostics: one qdisc dump + UDP counters to stdout
        match netlink::QdiscDumper::new().and_then(|mut d| d.dump_roots()) {
            Ok(qs) => {
                for q in qs {
                    println!("{} {} sent {}B/{}p drops {} requeues {} backlog {}B/{}p", q.dev, q.kind, q.bytes,
                        q.packets, q.drops, q.requeues, q.backlog, q.qlen);
                }
            }
            Err(e) => println!("netlink: {e}"),
        }
        println!("udp: {:?}", read_udp_snmp());
        return;
    }
    let dir = PathBuf::from(arg(&args, "--dir", "/data/local/tmp/mp/probe"));
    let port: u16 = arg(&args, "--port", "47600").parse().unwrap_or(47600);
    let hz: u32 = arg(&args, "--hz", "100").parse().unwrap_or(100);
    let keep_hours: u64 = arg(&args, "--keep-hours", "48").parse().unwrap_or(48);
    let _ = fs::create_dir_all(&dir);
    let started = utc_ns();
    let store: Shared = Arc::new(Mutex::new(Store { dir, hour: 0, out: None, keep_hours }));
    emit(&store, started, serde_json::json!({"t": started, "k": "start", "version": VERSION, "hz": hz}));

    let s = store.clone();
    thread::spawn(move || poll_loop(s, hz));
    let s = store.clone();
    thread::spawn(move || logcat_loop(s));
    let s = store.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        s.lock().unwrap().flush();
    });
    serve(store, port, started);
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!keep_log_line("1792472351.068 1 2 D Foo: onCapabilitiesChanged"));
    }

    #[test]
    fn hour_files_round_trip_and_range_filters() {
        let dir = std::env::temp_dir().join(format!("mp-probe-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut s = Store { dir: dir.clone(), hour: 0, out: None, keep_hours: 48 };
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

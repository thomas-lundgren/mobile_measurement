//! Server packet capture per test: headers only, on the ports of the test's
//! TCP flows (bulk and app traffic), into `server/capture.pcap`. Shows what
//! TCP put on the wire next to the app log: segment and ACK sizes,
//! retransmissions, RTT samples and the DSCP that arrived.
//!
//! Best-effort by design (logging-first): a capture that cannot start (no
//! `CAP_NET_RAW` on the binary, binary missing) is logged and the test runs
//! without it. The capture binary needs the capability, not the server:
//! `sudo setcap cap_net_raw,cap_net_admin=eip /usr/bin/tcpdump`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use mp_contracts::Flow;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// File name in the test's `server/` folder; compressed to `.pcap.gz` after
/// the test (readers take the `.pcap` while it exists, else the `.gz`).
pub const FILE: &str = "capture.pcap";
/// Bytes kept per packet: Linux cooked header (≤ 20) + IPv6 (40) + TCP with
/// options (≤ 60) — headers only, never the payload of a full segment.
const SNAPLEN: u32 = 128;
/// Size cap: at most this many packets (≈ 290 MB before compression).
const MAX_PACKETS: u64 = 2_000_000;
/// Hard stop if the test is never stopped (test-control died, say).
const MAX_SECONDS: u64 = 3600;
const LISTEN_TIMEOUT: Duration = Duration::from_secs(3);
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub struct CaptureConfig {
    /// The capture binary (`tcpdump`), which must hold `CAP_NET_RAW`.
    pub binary: String,
    pub interface: String,
}

impl CaptureConfig {
    /// From the environment: `MP_CAPTURE=off` disables, `MP_CAPTURE_BIN` and
    /// `MP_CAPTURE_IFACE` override `tcpdump` on `any`.
    pub fn from_env() -> Option<Self> {
        if std::env::var("MP_CAPTURE").is_ok_and(|v| v == "off" || v == "0") {
            return None;
        }
        Some(CaptureConfig {
            binary: std::env::var("MP_CAPTURE_BIN").unwrap_or_else(|_| "tcpdump".into()),
            interface: std::env::var("MP_CAPTURE_IFACE").unwrap_or_else(|_| "any".into()),
        })
    }
}

/// The BPF filter for a test: every port of a TCP flow. App traffic also
/// moves its UDP bursts over its port, so its filter takes both protocols.
pub fn filter(bound: &[(&Flow, u16)]) -> Option<String> {
    let terms: Vec<String> = bound
        .iter()
        .filter_map(|(flow, port)| match flow {
            Flow::BulkTcp { .. } => Some(format!("tcp port {port}")),
            Flow::AppTraffic { .. } => Some(format!("port {port}")),
            _ => None,
        })
        .collect();
    (!terms.is_empty()).then(|| terms.join(" or "))
}

pub struct Capture {
    child: Child,
    path: PathBuf,
}

/// Start capturing into `dir`; returns once the binary listens (so the UE's
/// first SYN is on file), or `None` when it could not start.
pub async fn start(cfg: &CaptureConfig, dir: &Path, filter: &str) -> Option<Capture> {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("capture: no log dir {}: {e}", dir.display());
        return None;
    }
    let path = dir.join(FILE);
    let spawned = Command::new(&cfg.binary)
        .args(["-i", &cfg.interface, "-n", "-s", &SNAPLEN.to_string()])
        .args(["-c", &MAX_PACKETS.to_string()])
        .args(["-G", &MAX_SECONDS.to_string(), "-W", "1"])
        .arg("-w")
        .arg(&path)
        .arg(filter)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            eprintln!("capture: could not start {}: {e}", cfg.binary);
            return None;
        }
    };
    let mut lines = BufReader::new(child.stderr.take()?).lines();
    let listening = tokio::time::timeout(LISTEN_TIMEOUT, async {
        let mut said = Vec::new();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.contains("listening on") {
                return Ok(());
            }
            said.push(line);
        }
        Err(said.join("; "))
    })
    .await;
    match listening {
        Ok(Ok(())) => {
            // keep draining stderr so the binary never blocks on a full pipe
            tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
            eprintln!("capture: {} ({filter})", path.display());
            Some(Capture { child, path })
        }
        Ok(Err(said)) => {
            eprintln!("capture: binary exited (CAP_NET_RAW missing?): {said}");
            None
        }
        Err(_) => {
            eprintln!("capture: binary did not report listening; stopped");
            let _ = child.kill().await;
            None
        }
    }
}

impl Capture {
    /// Stop (SIGTERM, so the binary flushes its buffer), then compress the
    /// file in the background.
    pub async fn stop(mut self) {
        if let Some(pid) = self.child.id() {
            // SAFETY: plain kill(2) on our own child's pid.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        }
        if tokio::time::timeout(STOP_TIMEOUT, self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
        let path = self.path;
        tokio::spawn(async move {
            match Command::new("gzip").arg("-f").arg(&path).status().await {
                Ok(s) if s.success() => {}
                other => eprintln!("capture: gzip {} failed: {other:?}", path.display()),
            }
        });
    }
}

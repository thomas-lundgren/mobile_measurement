"""USB bench collector: modem-side detail the Logger app cannot get, read
over adb while the phone is plugged into this machine.

- **qdisc** (``tc -s qdisc show``, ~25 Hz): per network device the bytes and
  packets sent, drops, *requeues* and the queue backlog. A requeue is the
  modem driver refusing a packet because the modem stopped its queue — the
  modem's flow control, i.e. the back pressure seen in the timing tests —
  and the backlog is what then waits in the phone's qdisc.
- **radiolog** (``logcat -b radio``): physical channel configs (every
  aggregated carrier: role, RAT, band, PCI, DL/UL channel, frequency and
  bandwidth), the modem's link-capacity estimates (LCE), data-call and
  signal-strength indications.

Both carry the phone's UTC clock (``date +%s%N`` / ``logcat -v epoch``), the
same clock as the Logger's mobile rows, so they join to tests without an
offset. Rows go to ``<out>/<YYYY-MM-DD>/{qdisc,radiolog}_<start>.parquet``,
one segment per ``SEGMENT_S``.

    python -m fieldtest_analysis.bench collect --out ~/mp-data/bench
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import threading
import time
from pathlib import Path

import pandas as pd

SEGMENT_S = 10.0
#: Write an unchanged device's counters at least this often (s).
HEARTBEAT_S = 1.0

QDISC_COLUMNS = ["utc_ns", "dev", "qdisc", "sent_bytes", "sent_pkts", "dropped", "overlimits", "requeues",
                 "backlog_bytes", "backlog_pkts"]  # fmt: skip
RADIOLOG_COLUMNS = ["utc_ns", "kind", "summary", "payload"]

_QHEAD = re.compile(r"^qdisc (\S+) (\S+) dev (\S+) (root|parent \S+)")
_SENT = re.compile(r"Sent (\d+) bytes (\d+) pkt \(dropped (\d+), overlimits (\d+) requeues (\d+)\)")
_BACKLOG = re.compile(r"backlog (\d+)([KMG]?)b (\d+)p")
_UNIT = {"": 1, "K": 1000, "M": 1000**2, "G": 1000**3}


def parse_tc(text: str, utc_ns: int) -> list[dict]:
    """Root qdisc of every device in one ``tc -s qdisc show`` dump."""
    rows, cur = [], None
    for line in text.splitlines():
        m = _QHEAD.match(line.strip())
        if m:
            cur = {"utc_ns": utc_ns, "dev": m.group(3), "qdisc": m.group(1)} if m.group(4) == "root" else None
            continue
        if cur is None:
            continue
        m = _SENT.search(line)
        if m:
            cur.update(sent_bytes=int(m.group(1)), sent_pkts=int(m.group(2)), dropped=int(m.group(3)),
                       overlimits=int(m.group(4)), requeues=int(m.group(5)))  # fmt: skip
            continue
        m = _BACKLOG.search(line)
        if m and "sent_pkts" in cur:
            cur.update(backlog_bytes=int(m.group(1)) * _UNIT[m.group(2)], backlog_pkts=int(m.group(3)))
            rows.append(cur)
            cur = None
    return rows


_LOG_TS = re.compile(r"^\s*(\d+)\.(\d+)\s")
_PCC = re.compile(r"\{([^{}]*)\}")


def _kv(body: str) -> dict:
    out = {}
    for part in re.split(r",\s*(?=m[A-Z])", body):
        if "=" in part:
            k, v = part.split("=", 1)
            out[k.strip()] = v.strip()
    return out


def parse_logcat_line(line: str) -> dict | None:
    """One radio-log line → a radiolog row for the events we keep."""
    m = _LOG_TS.match(line)
    if not m:
        return None
    utc_ns = int(m.group(1)) * 1_000_000_000 + int(m.group(2).ljust(9, "0")[:9])
    if "[UNSL]< UNSOL_PHYSICAL_CHANNEL_CONFIG" in line:
        carriers = [_kv(b) for b in _PCC.findall(line.split("UNSOL_PHYSICAL_CHANNEL_CONFIG", 1)[1])]
        summary = " + ".join(
            "{rat} B{band} PCI {pci} {dl}/{ul} MHz{role}".format(
                rat=c.get("mNetworkType", "?"), band=c.get("mBand", "?"), pci=c.get("mPhysicalCellId", "?"),
                dl=int(c.get("mCellBandwidthDownlinkKhz", 0) or 0) // 1000,
                ul=int(c.get("mCellBandwidthUplinkKhz", 0) or 0) // 1000,
                role="" if c.get("mConnectionStatus") == "PrimaryServing" else " (secondary)",
            )
            for c in carriers
        )  # fmt: skip
        return {"utc_ns": utc_ns, "kind": "channel_config", "summary": summary or "none", "payload": json.dumps(carriers)}
    if "[UNSL]< UNSOL_LCE_INFO_RECV" in line:
        caps = [_kv(b) for b in _PCC.findall(line)]
        c = next((x for x in caps if x.get("mType") == "0"), caps[0] if caps else {})
        dl, ul = c.get("mDownlinkCapacityKbps"), c.get("mUplinkCapacityKbps")
        return {"utc_ns": utc_ns, "kind": "lce", "summary": f"DL {dl} / UL {ul} kbit/s", "payload": json.dumps(caps)}
    for tag, kind in (("UNSOL_DATA_CALL_LIST_CHANGED", "data_call"), ("UNSOL_SIGNAL_STRENGTH", "signal_strength"),
                      ("UNSOL_RESPONSE_NETWORK_STATE_CHANGED", "network_state")):  # fmt: skip
        if tag in line and ("RILC" in line or "[UNSL]" in line):
            return {"utc_ns": utc_ns, "kind": kind, "summary": "", "payload": None}
    return None


class _Segments:
    """Buffers rows and writes a parquet segment every SEGMENT_S."""

    def __init__(self, out: Path, name: str, columns: list[str]):
        self.out, self.name, self.columns = out, name, columns
        self.rows: list[dict] = []
        self.started = time.time()
        self.lock = threading.Lock()

    def add(self, rows: list[dict]) -> None:
        with self.lock:
            self.rows.extend(rows)
            if time.time() - self.started >= SEGMENT_S:
                self._flush()

    def _flush(self) -> None:
        if self.rows:
            day = time.strftime("%Y-%m-%d", time.localtime(self.rows[0]["utc_ns"] / 1e9))
            d = self.out / day
            d.mkdir(parents=True, exist_ok=True)
            df = pd.DataFrame(self.rows).reindex(columns=self.columns)
            tmp = d / f".{self.name}_{int(self.started)}.parquet.tmp"
            df.to_parquet(tmp, index=False)
            tmp.rename(d / f"{self.name}_{int(self.started)}.parquet")
        self.rows, self.started = [], time.time()

    def close(self) -> None:
        with self.lock:
            self._flush()


def _qdisc_loop(seg: _Segments, adb: list[str], stop: threading.Event) -> None:
    script = 'while :; do echo "@T $(date +%s%N)"; tc -s qdisc show; done'
    p = subprocess.Popen([*adb, "exec-out", "sh", "-c", script], stdout=subprocess.PIPE, text=True)
    last: dict[str, tuple] = {}
    buf, ts = [], None
    try:
        for line in p.stdout:
            if stop.is_set():
                break
            if line.startswith("@T "):
                if ts is not None:
                    keep = []
                    for r in parse_tc("".join(buf), ts):
                        # modem and Wi-Fi links that carry traffic only
                        if not r["dev"].startswith(("rmnet", "wlan")) or r["sent_pkts"] == 0:
                            continue
                        key = (r["sent_pkts"], r["requeues"], r["dropped"], r["backlog_pkts"])
                        prev = last.get(r["dev"])
                        if prev is None or prev[0] != key or (ts - prev[1]) / 1e9 >= HEARTBEAT_S:
                            keep.append(r)
                            last[r["dev"]] = (key, ts)
                    seg.add(keep)
                ts, buf = int(line[3:].strip()), []
            else:
                buf.append(line)
    finally:
        p.terminate()


def _logcat_loop(seg: _Segments, adb: list[str], stop: threading.Event) -> None:
    p = subprocess.Popen([*adb, "logcat", "-b", "radio", "-v", "epoch", "-T", "1"], stdout=subprocess.PIPE,
                         text=True, errors="replace")  # fmt: skip
    try:
        for line in p.stdout:
            if stop.is_set():
                break
            r = parse_logcat_line(line)
            if r:
                seg.add([r])
    finally:
        p.terminate()


def collect(out: Path, serial: str | None = None, duration_s: float | None = None) -> None:
    """Run both collectors until the phone disconnects (adb exits), a
    duration elapses or the process is stopped; flushes on exit."""
    adb = ["adb", *(["-s", serial] if serial else [])]
    q = _Segments(out, "qdisc", QDISC_COLUMNS)
    r = _Segments(out, "radiolog", RADIOLOG_COLUMNS)
    stop = threading.Event()
    threads = [threading.Thread(target=_qdisc_loop, args=(q, adb, stop), daemon=True),
               threading.Thread(target=_logcat_loop, args=(r, adb, stop), daemon=True)]  # fmt: skip
    for t in threads:
        t.start()
    t_end = time.time() + duration_s if duration_s else None
    try:
        while all(t.is_alive() for t in threads) and (t_end is None or time.time() < t_end):
            time.sleep(0.5)
    except KeyboardInterrupt:
        pass
    finally:
        stop.set()
        q.close()
        r.close()


def load_window(bench_dir: Path | None, t0_ns: int, t1_ns: int) -> tuple[pd.DataFrame, pd.DataFrame]:
    """Bench rows between t0 and t1 (phone UTC ns); empty frames if none."""
    empty = (pd.DataFrame(columns=QDISC_COLUMNS), pd.DataFrame(columns=RADIOLOG_COLUMNS))
    if bench_dir is None or not Path(bench_dir).is_dir():
        return empty
    days = {time.strftime("%Y-%m-%d", time.localtime(t / 1e9)) for t in (t0_ns, t1_ns)}
    out = []
    for name in ("qdisc", "radiolog"):
        parts = []
        for day in days:
            for f in sorted((Path(bench_dir) / day).glob(f"{name}_*.parquet")):
                start = int(f.stem.split("_")[-1]) * 1_000_000_000
                if start > t1_ns or start + int((SEGMENT_S + 120) * 1e9) < t0_ns:
                    continue  # segment cannot overlap the window
                df = pd.read_parquet(f)
                parts.append(df[(df["utc_ns"] >= t0_ns) & (df["utc_ns"] <= t1_ns)])
        out.append(pd.concat(parts, ignore_index=True) if parts else empty[len(out)])
    return out[0], out[1]


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="fieldtest-bench", description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("collect")
    c.add_argument("--out", required=True)
    c.add_argument("--serial")
    c.add_argument("--duration", type=float)
    args = ap.parse_args(argv)
    if args.cmd == "collect":
        collect(Path(args.out).expanduser(), args.serial, args.duration)
    return 0


if __name__ == "__main__":
    sys.exit(main())

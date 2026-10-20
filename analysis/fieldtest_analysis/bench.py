"""Modem-side detail the Logger app cannot read itself, recorded on the phone
by ``mp-probe`` (probe/): per network device the root-qdisc counters —
bytes and packets sent, drops, *requeues* (the modem driver refusing a
packet because the modem stopped its queue: its flow control, the back
pressure seen in the timing tests) and the backlog waiting in the phone's
qdisc — plus radio-log events: physical channel configs (every aggregated
carrier: role, RAT, band, PCI, bandwidth), the modem's link-capacity
estimates (LCE), data-call and signal-strength indications.

The Logger fetches each test's window from mp-probe and uploads it as
``mobile/probe.jsonl.gz`` (JSON lines; ``k`` = ``q`` qdisc, ``u`` UDP
counters, ``l`` radio-log line). Tests recorded before that came from the
retired USB bench collector, ``<bench_dir>/<YYYY-MM-DD>/{qdisc,radiolog}_*.parquet``,
still read by ``load_window``. Both are on the phone's UTC clock, the same
clock as the Logger's mobile rows, so they join without an offset.
"""

from __future__ import annotations

import gzip
import json
import re
import time
from pathlib import Path

import pandas as pd

#: Segment length of the retired USB collector's files (for load_window).
SEGMENT_S = 10.0
PROBE_LOG = "probe.jsonl.gz"

QDISC_COLUMNS = ["utc_ns", "dev", "qdisc", "sent_bytes", "sent_pkts", "dropped", "overlimits", "requeues",
                 "backlog_bytes", "backlog_pkts"]  # fmt: skip
RADIOLOG_COLUMNS = ["utc_ns", "kind", "summary", "payload"]

#: mp-probe qdisc record keys -> QDISC_COLUMNS
_Q_KEYS = {"t": "utc_ns", "dev": "dev", "qdisc": "qdisc", "b": "sent_bytes", "p": "sent_pkts", "d": "dropped",
           "o": "overlimits", "r": "requeues", "bb": "backlog_bytes", "bp": "backlog_pkts"}  # fmt: skip

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


def read_probe_log(path: Path) -> tuple[pd.DataFrame, pd.DataFrame]:
    """mp-probe records (``probe.jsonl.gz``) as (qdisc, radiolog) frames. A
    line cut off by an interrupted fetch is skipped."""
    q, r = [], []
    try:
        with gzip.open(path, "rt", errors="replace") as f:
            for line in f:
                try:
                    rec = json.loads(line)
                except ValueError:
                    continue
                if rec.get("k") == "q":
                    q.append({col: rec.get(key) for key, col in _Q_KEYS.items()})
                elif rec.get("k") == "l":
                    row = parse_logcat_line(rec.get("line", ""))
                    if row:
                        r.append(row)
    except (OSError, EOFError):
        pass  # truncated gzip: keep what was read
    return pd.DataFrame(q, columns=QDISC_COLUMNS), pd.DataFrame(r, columns=RADIOLOG_COLUMNS)


def load_for_test(test_dir: Path, bench_dir: Path | None, t0_ns: int, t1_ns: int) -> tuple[pd.DataFrame, pd.DataFrame]:
    """The test's mp-probe records between t0 and t1, or — for tests recorded
    before they were uploaded with the test — the USB bench files."""
    path = Path(test_dir) / "mobile" / PROBE_LOG
    if not path.exists():
        return load_window(bench_dir, t0_ns, t1_ns)
    q, r = read_probe_log(path)
    return q[(q["utc_ns"] >= t0_ns) & (q["utc_ns"] <= t1_ns)], r[(r["utc_ns"] >= t0_ns) & (r["utc_ns"] <= t1_ns)]


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

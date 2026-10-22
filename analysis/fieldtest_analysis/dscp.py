"""DSCP experiments across days, for the DSCP page (``viz/dscp.json``).

Every test whose app traffic ran paced UDP bursts (engine ``udp_burst``; the
``dscp_sweep.sh`` phases and ``app-video-udp*``) contributes one row per
burst stream (activity × direction): the marking sent, what arrived (the
receiver's ``dscp_recv`` counts), datagram loss, extra delay and IPDV, the
path (mobile data or Wi-Fi, from the radio log) and the signal. Tests on the
same path and workload less than ``SWEEP_GAP_S`` apart form a *sweep*; the
summary groups every test by path, workload, direction and marking, and
classifies what the network did with the marking.
"""

from __future__ import annotations

import statistics
from collections import Counter

#: Tests further apart than this start a new sweep.
SWEEP_GAP_S = 600
#: A marking counts as kept / re-marked / bleached when this share of the
#: datagrams that arrived carried that value.
DOMINANT = 0.9

WORKLOADS = {
    "video_chunk": "video (8 Mbit/s bursts)",
    "photo_upload": "upload (8 Mbit/s bursts)",
    "voice": "voice (64 kbit/s)",
}


def _median(values):
    v = [x for x in values if x is not None]
    return statistics.median(v) if v else None


def _path(t: dict) -> tuple[str, float | None]:
    """Mobile data or Wi-Fi, from the test's radio log, and its signal (dBm)."""
    tr = Counter(x for x in t.get("transport") or [] if x)
    if tr and tr.most_common(1)[0][0] == "WIFI":
        return "Wi-Fi", _median(t.get("wifi_rssi") or [])
    return "mobile data", _median(t.get("lte_rsrp") or []) or _median(t.get("nr_rsrp") or [])


def _outcome(sent: int | None, arrived: dict) -> str:
    """What the path did with the marking."""
    total = sum(arrived.values())
    if not total:
        return "unknown"
    if sent is None:
        value, n = max(arrived.items(), key=lambda kv: kv[1])
        return "arrived unmarked" if value == 0 and n / total >= DOMINANT else f"arrived as {value}"
    value, n = max(arrived.items(), key=lambda kv: kv[1])
    if n / total < DOMINANT:
        return "mixed"
    if value == sent:
        return "kept"
    if value == 0:
        return "bleached"
    return f"re-marked to {value}"


def rows(tests: list[dict]) -> list[dict]:
    """One row per burst stream of every test that ran UDP bursts, by time."""
    out = []
    for t in sorted(tests, key=lambda t: t["start"]):
        bursts = ((t.get("app") or {}).get("bursts")) or []
        if not bursts:
            continue
        path, signal = _path(t)
        for b in bursts:
            arrived = {int(k): int(v) for k, v in (b.get("dscp_recv") or {}).items()}
            out.append({
                "test_id": t["test_id"], "plan": t.get("plan"), "start": t["start"], "path": path,
                "signal_dbm": None if signal is None else round(signal),
                "activity": b["activity"], "workload": WORKLOADS.get(b["activity"], b["activity"]),
                "direction": b["direction"], "dscp_sent": b.get("dscp_sent"), "arrived_as": arrived,
                "outcome": _outcome(b.get("dscp_sent"), arrived),
                "kept": b.get("dscp_kept"), "loss": b.get("loss"), "planned": b.get("planned"),
                "arrived": b.get("arrived"), "extra_delay_ms": b.get("extra_delay_ms"), "ipdv_ms": b.get("ipdv_ms"),
            })  # fmt: skip
    return out


def sweeps(rs: list[dict]) -> list[dict]:
    """Consecutive tests on one path and workload, at most SWEEP_GAP_S apart."""
    out: list[dict] = []
    for r in rs:
        s = out[-1] if out else None
        if s and s["path"] == r["path"] and s["activity"] == r["activity"] and r["start"] - s["last"] <= SWEEP_GAP_S:
            s["rows"].append(r)
            s["last"] = r["start"]
        else:
            out.append({"start": r["start"], "last": r["start"], "path": r["path"], "activity": r["activity"],
                        "workload": r["workload"], "rows": [r]})  # fmt: skip
    for s in out:
        s["tests"] = len({r["test_id"] for r in s["rows"]})
    return out


def by_marking(rs: list[dict]) -> list[dict]:
    """Every test grouped by path, workload, direction and marking sent."""
    groups: dict[tuple, list[dict]] = {}
    for r in rs:
        key = (r["path"], r["workload"], r["direction"], -1 if r["dscp_sent"] is None else r["dscp_sent"])
        groups.setdefault(key, []).append(r)
    out = []
    for (path, workload, direction, sent), g in sorted(groups.items()):
        arrived: Counter = Counter()
        for r in g:
            arrived.update(r["arrived_as"])
        sent = None if sent == -1 else sent
        out.append({
            "path": path, "workload": workload, "direction": direction, "dscp_sent": sent, "tests": len(g),
            "arrived_as": dict(arrived), "outcome": _outcome(sent, dict(arrived)),
            "kept": _median(r["kept"] for r in g), "loss": _median(r["loss"] for r in g),
            "loss_range": [min(r["loss"] for r in g if r["loss"] is not None), max(r["loss"] for r in g if r["loss"] is not None)]
            if any(r["loss"] is not None for r in g) else None,
            "extra_delay_p99_ms": _median((r["extra_delay_ms"] or {}).get("p99") for r in g),
            "ipdv_p99_ms": _median((r["ipdv_ms"] or {}).get("p99") for r in g),
        })  # fmt: skip
    return out


def summary(tests: list[dict]) -> dict:
    rs = rows(tests)
    return {"rows": rs, "sweeps": sweeps(rs), "by_marking": by_marking(rs)}

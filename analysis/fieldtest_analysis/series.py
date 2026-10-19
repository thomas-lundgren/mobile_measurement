"""Time series for the zoomable charts: per test, 200 ms bins of probe RTT,
one-way delay per direction, TCP throughput per direction, and the radio
trace, all on the **server** UTC clock (mobile times minus the estimated
offset O = mobile - server), then assembled into one file per local day.

    python -m fieldtest_analysis.series <data_root> [--db mp.db] -o <out_dir>

Writes ``<out_dir>/tests/<test_id>.json`` (cache, rebuilt only when a file in
the test directory is newer), ``<out_dir>/series/<YYYY-MM-DD>.json`` and
``<out_dir>/series/days.json``. A test that fails to load is skipped.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path

import numpy as np
import pandas as pd

from . import clock, metrics, schema
from .loader import load_test
from .track import _plan_names, channel_label

BIN_NS = 200_000_000
#: Bump when a test's cached series gains or changes fields (forces a rebuild).
SERIES_VERSION = 2
#: Radio is sampled at ~1 Hz; carry a sample across the 200 ms bins until the next.
RADIO_FILL_BINS = 6

COLUMNS = [
    "rtt_med", "rtt_max",
    "ul_med", "ul_max", "dl_med", "dl_max",
    "down_mbps", "up_mbps",
    "lte_rsrp", "nr_rsrp", "nr_rsrq", "mode",
    "lte_pci", "earfcn", "nr_pci", "nrarfcn",
]  # fmt: skip
RADIO_COLUMNS = ["lte_rsrp", "nr_rsrp", "nr_rsrq", "mode", "lte_pci", "earfcn", "nr_pci", "nrarfcn"]


def _bins(t_ns: pd.Series) -> pd.Series:
    return (t_ns // BIN_NS).astype("int64")


def _delay_stats(t_ns: pd.Series, v_ns: pd.Series, prefix: str) -> pd.DataFrame:
    df = pd.DataFrame({"bin": _bins(t_ns), "v": v_ns.astype("float") / 1e6})
    g = df.groupby("bin")["v"]
    return pd.DataFrame({f"{prefix}_med": g.median(), f"{prefix}_max": g.max()})


def test_series(test_dir: Path, plan: str | None = None) -> dict | None:
    td = load_test(test_dir)
    pk = td.packets
    if pk.empty:
        return None
    off = clock.estimate_offset(td)
    o = float(off.offset_ns) if off.available else 0.0
    pk = pk.assign(
        utc=pd.to_numeric(pk["utc_ns"], errors="coerce").astype("float"),
        size=pd.to_numeric(pk["payload_size"], errors="coerce").fillna(0),
    )
    # Server-clock time for every packet row.
    pk["t"] = np.where(pk["node"] == "mobile", pk["utc"] - o, pk["utc"])
    pk = pk.dropna(subset=["t"])
    parts = []

    # Probe RTT on the mobile's monotonic clock, placed at echo receipt.
    mob = pk[(pk["node"] == "mobile") & (pk["protocol"] == "UDP")]
    sent = mob[mob["direction"] == schema.DIR_SENT]
    recv = mob[mob["direction"] == schema.DIR_RECEIVED]
    keys = ["flow_id", "sequence_number"]
    echo = sent.merge(recv, on=keys, suffixes=("_s", "_r"))
    echo = echo.assign(rtt=echo["mono_ns_r"].astype("float") - echo["mono_ns_s"].astype("float"))
    echo = echo[echo["rtt"] >= 0]
    if not echo.empty:
        parts.append(_delay_stats(echo["t_r"], echo["rtt"], "rtt"))

    # One-way delay, offset-corrected; skipped when no offset estimate exists.
    if off.available:
        owd = metrics.realtime_owd(td, off).dropna(subset=["owd_corrected_ns"])
        owd = owd.assign(utc=owd["utc_ns"].astype("float"))
        owd["t"] = np.where(owd["node"] == "mobile", owd["utc"] - o, owd["utc"])
        for direction, prefix in (("uplink", "ul"), ("downlink", "dl")):
            d = owd[owd["direction_owd"] == direction]
            if not d.empty:
                parts.append(_delay_stats(d["t"], d["owd_corrected_ns"], prefix))

    # TCP goodput per direction (bytes received per bin).
    tcp = pk[(pk["protocol"] == "TCP") & (pk["direction"] == schema.DIR_RECEIVED)]
    for node, col in (("mobile", "down_mbps"), ("server", "up_mbps")):
        d = tcp[tcp["node"] == node]
        if not d.empty:
            b = d.groupby(_bins(d["t"]))["size"].sum() * 8 / (BIN_NS / 1e9) / 1e6
            parts.append(b.rename(col).to_frame())

    radio = td.radio
    neighbors: list[list] = []
    if radio is not None and not radio.empty:
        r = radio.assign(t=pd.to_numeric(radio["utc_ns"], errors="coerce").astype("float") - o)
        r = r.dropna(subset=["t"]).sort_values("t")
        # Neighbor cells as [t_s, pci, rsrp]; the schema carries no channel.
        for t_ns, cells in zip(r["t"], r["neighbor_cells"]):
            if cells is None or (not isinstance(cells, (list, np.ndarray))):
                continue
            for c in cells:
                if c is not None and c.get("rsrp") is not None and c.get("pci") is not None:
                    neighbors.append([round(t_ns / 1e9, 1), int(c["pci"]), int(c["rsrp"])])
        last = r.assign(bin=_bins(r["t"])).groupby("bin").last()
        parts.append(
            pd.DataFrame(
                {
                    "lte_rsrp": last["lte_rsrp"],
                    "nr_rsrp": last["rsrp"],
                    "nr_rsrq": last["rsrq"],
                    "mode": last["network_type"],
                    "lte_pci": last["lte_pci"],
                    "earfcn": last["earfcn"],
                    "nr_pci": last["pci"],
                    "nrarfcn": last["nrarfcn"],
                }
            )
        )

    if not parts:
        return None
    df = pd.concat(parts, axis=1)
    lo, hi = int(df.index.min()), int(df.index.max())
    df = df.reindex(range(lo, hi + 1))
    for col in COLUMNS:
        if col not in df:
            df[col] = None
    for col in ("down_mbps", "up_mbps"):
        if df[col].notna().any():
            df[col] = df[col].fillna(0.0)
    for col in RADIO_COLUMNS:
        df[col] = df[col].ffill(limit=RADIO_FILL_BINS)

    out = {
        "v": SERIES_VERSION,
        "test_id": test_dir.name,
        "neighbors": neighbors,
        "plan": plan,
        "start": lo * BIN_NS / 1e9,
        "end": (hi + 1) * BIN_NS / 1e9,
        "t": [round(b * BIN_NS / 1e9, 1) for b in df.index],
    }
    for col in COLUMNS:
        vals = df[col].astype(object).where(df[col].notna(), None).tolist()
        out[col] = [round(v, 2) if isinstance(v, float) else v for v in vals]
    for col in ("lte_pci", "earfcn", "nr_pci", "nrarfcn"):
        out[col] = [None if v is None else int(v) for v in out[col]]
    return out


def _newest_mtime(d: Path) -> float:
    return max((p.stat().st_mtime for p in d.rglob("*") if p.is_file()), default=0.0)


def _write_json(path: Path, obj) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(obj, separators=(",", ":")))
    os.replace(tmp, path)


def _local_day(epoch_s: float) -> str:
    return time.strftime("%Y-%m-%d", time.localtime(epoch_s))


def build(root: str | Path, out_dir: str | Path, db: str | None = None) -> list[str]:
    """Refresh the per-test cache and rewrite every day file. Returns the days."""
    out = Path(out_dir)
    plans = _plan_names(db)
    tests = []
    for test_dir in sorted(Path(root).glob("ses_*/tst_*")):
        cache = out / "tests" / f"{test_dir.name}.json"
        data = None
        if cache.exists() and cache.stat().st_mtime >= _newest_mtime(test_dir):
            data = json.loads(cache.read_text())
            if data is not None and data.get("v") != SERIES_VERSION:
                data = None
        if data is None:
            try:
                data = test_series(test_dir, plans.get(test_dir.name))
            except Exception as e:  # truncated upload, unreadable parquet, ...
                print(f"skip {test_dir.name}: {e}", file=sys.stderr)
                continue
            _write_json(cache, data)
        if data:
            tests.append(data)

    by_day: dict[str, list[dict]] = {}
    for t in sorted(tests, key=lambda t: t["start"]):
        by_day.setdefault(_local_day(t["start"]), []).append(t)
    for day, day_tests in by_day.items():
        cols = {c: [] for c in ["t", *COLUMNS]}
        for t in day_tests:
            for c in cols:
                cols[c].extend(t[c])
            # A null row after each test so lines don't join across the gap.
            cols["t"].append(round(t["end"] + 0.1, 1))
            for c in COLUMNS:
                cols[c].append(None)
        meta = [{k: t[k] for k in ("test_id", "plan", "start", "end")} for t in day_tests]
        neighbors = [n for t in day_tests for n in t.get("neighbors", [])]
        # Clear-text band + frequency for every channel seen that day.
        channels = {
            "lte": {str(e): channel_label("LTE", e) for e in sorted({v for v in cols["earfcn"] if v is not None})},
            "nr": {str(n): channel_label("NR", n) for n in sorted({v for v in cols["nrarfcn"] if v is not None})},
        }
        _write_json(
            out / "series" / f"{day}.json",
            {"day": day, "tests": meta, "channels": channels, "neighbors": neighbors, **cols},
        )
    days = sorted(by_day)
    _write_json(out / "series" / "days.json", {"days": days})
    return days


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="fieldtest-series")
    ap.add_argument("root", help="data root holding ses_*/tst_* test directories")
    ap.add_argument("--db", help="server SQLite database, for plan names")
    ap.add_argument("-o", "--out-dir", required=True, help="viz directory to write into")
    args = ap.parse_args(argv)
    days = build(args.root, args.out_dir, args.db)
    print(f"wrote {len(days)} day file(s) to {args.out_dir}/series", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

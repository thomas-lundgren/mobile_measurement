"""Time series for the zoomable charts: per test, 200 ms bins of probe RTT,
one-way delay per direction, TCP throughput per direction, and the radio
trace, all on the **server** UTC clock (mobile times minus the estimated
offset O = mobile - server), then assembled into one file per local day.

Delays are binned at the packet's **send** time, so a probe's RTT and its
two one-way legs land in the same bin even while queueing delay swings by
hundreds of ms per second (binned at arrival, they would not, and a bin's
uplink delay could exceed its RTT). Throughput is binned at arrival.

    python -m fieldtest_analysis.series <data_root> [--db mp.db] -o <out_dir>

Writes ``<out_dir>/tests/<test_id>.json`` (cache, rebuilt only when a file in
the test directory is newer), ``<out_dir>/series/<YYYY-MM-DD>.json``,
``<out_dir>/series/days.json`` and ``<out_dir>/findings/<YYYY-MM-DD>.json``
(see findings.py). A test that fails to load is skipped.
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

from . import clock, findings, metrics, schema, stalls, timing
from .loader import load_test
from .track import _plan_names, channel_label

BIN_NS = 200_000_000
#: Bump when a test's cached series gains or changes fields (forces a rebuild).
SERIES_VERSION = 10
#: Radio is sampled at ~1 Hz; carry a sample across the 200 ms bins until the next.
RADIO_FILL_BINS = 6

COLUMNS = [
    "rtt_med", "rtt_max",
    "ul_med", "ul_max", "dl_med", "dl_max",
    "down_mbps", "up_mbps",
    "lte_rsrp", "nr_rsrp", "nr_rsrq", "mode",
    "lte_pci", "earfcn", "nr_pci", "nrarfcn",
    "transport", "wifi_freq_mhz", "wifi_channel", "wifi_bw_mhz", "wifi_standard", "wifi_rssi",
]  # fmt: skip
RADIO_COLUMNS = [
    "lte_rsrp", "nr_rsrp", "nr_rsrq", "mode", "lte_pci", "earfcn", "nr_pci", "nrarfcn",
    "transport", "wifi_freq_mhz", "wifi_channel", "wifi_bw_mhz", "wifi_standard", "wifi_rssi",
]  # fmt: skip


def _bins(t_ns: pd.Series) -> pd.Series:
    return (t_ns // BIN_NS).astype("int64")


def _delay_stats(t_ns: pd.Series, v_ns: pd.Series, prefix: str) -> pd.DataFrame:
    df = pd.DataFrame({"bin": _bins(t_ns), "v": v_ns.astype("float") / 1e6})
    g = df.groupby("bin")["v"]
    return pd.DataFrame({f"{prefix}_med": g.median(), f"{prefix}_max": g.max()})


def _pcts(values_ms) -> dict | None:
    v = pd.to_numeric(pd.Series(values_ms), errors="coerce").dropna()
    if v.empty:
        return None
    return {"p50": round(float(v.median()), 1), "p99": round(float(np.percentile(v, 99)), 1)}


def _udp_loss(td) -> dict:
    """Worst loss ratio per direction over the one-way UDP flows (receiver
    side: server = uplink, mobile = downlink) and the probe (round trip)."""
    pk = td.packets
    udp_flows = set(pk.loc[pk["protocol"] == "UDP", "flow_id"].dropna().astype(int))
    echo_flows = _echo_flows(pk)
    out = {"ul": None, "dl": None, "probe": None}
    for r in metrics.loss(td).itertuples(index=False):
        if r.flow_id not in udp_flows or r.loss_ratio is None or pd.isna(r.loss_ratio):
            continue
        if r.flow_id in echo_flows:
            if r.node != "mobile":
                continue  # the server's view of the probe flow
            key = "probe"
        else:
            key = "ul" if r.node == "server" else "dl"
        out[key] = max(out[key] or 0.0, round(float(r.loss_ratio), 4))
    return out


def _echo_flows(pk) -> set:
    m = pk[pk["node"] == "mobile"]
    sent = set(m.loc[m["direction"] == schema.DIR_SENT, "flow_id"].dropna().astype(int))
    recv = set(m.loc[m["direction"] == schema.DIR_RECEIVED, "flow_id"].dropna().astype(int))
    return sent & recv


def _peer_ips(td) -> list[str]:
    """The UE's address(es) as the server saw them (CLIENT_CONNECTED events):
    which network the test ran over. Empty for tests before the event existed."""
    ev = td.events
    if ev.empty or "event_type" not in ev or "event_metadata" not in ev:
        return []
    ips = set()
    for meta in ev.loc[ev["event_type"] == "CLIENT_CONNECTED", "event_metadata"].dropna():
        try:
            ips.add(json.loads(meta)["peer_ip"])
        except (ValueError, KeyError, TypeError):
            continue
    return sorted(ips)


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

    # Probe RTT on the mobile's monotonic clock, placed at the probe's send.
    keys = ["flow_id", "sequence_number"]
    udp = pk[pk["protocol"] == "UDP"]
    mob, srv = udp[udp["node"] == "mobile"], udp[udp["node"] == "server"]
    sent = mob[mob["direction"] == schema.DIR_SENT]
    recv = mob[mob["direction"] == schema.DIR_RECEIVED]
    echo = sent.merge(recv, on=keys, suffixes=("_s", "_r"))
    echo = echo.assign(rtt=echo["mono_ns_r"].astype("float") - echo["mono_ns_s"].astype("float"))
    echo = echo[echo["rtt"] >= 0]
    if not echo.empty:
        parts.append(_delay_stats(echo["t_s"], echo["rtt"], "rtt"))

    # One-way delay, offset-corrected, placed at send time; skipped when no
    # offset estimate exists. Realtime packets carry their sender's stamp;
    # probe legs come from matching the probe's rows on both nodes.
    ul_t, ul_v, dl_t, dl_v = [], [], [], []
    if off.available:
        owd = metrics.realtime_owd(td, off).dropna(subset=["owd_corrected_ns"])
        owd = owd.assign(utc=owd["utc_ns"].astype("float"), v=owd["owd_corrected_ns"].astype("float"))
        recv_t = np.where(owd["node"] == "mobile", owd["utc"] - o, owd["utc"])
        owd = owd.assign(t_send=recv_t - owd["v"])
        for direction, (ts, vs) in (("uplink", (ul_t, ul_v)), ("downlink", (dl_t, dl_v))):
            d = owd[owd["direction_owd"] == direction]
            ts.append(d["t_send"])
            vs.append(d["v"])
        if not echo.empty:
            srv_recv = srv[srv["direction"] == schema.DIR_RECEIVED][keys + ["t"]].rename(columns={"t": "t_srv_r"})
            srv_sent = srv[srv["direction"] == schema.DIR_SENT][keys + ["t"]].rename(columns={"t": "t_srv_s"})
            legs = echo.merge(srv_recv, on=keys).merge(srv_sent, on=keys)
            ul_t.append(legs["t_s"])
            ul_v.append(legs["t_srv_r"] - legs["t_s"])
            dl_t.append(legs["t_srv_s"])
            dl_v.append(legs["t_r"] - legs["t_srv_s"])
        for prefix, ts, vs in (("ul", ul_t, ul_v), ("dl", dl_t, dl_v)):
            t_all, v_all = pd.concat(ts, ignore_index=True), pd.concat(vs, ignore_index=True)
            if not t_all.empty:
                parts.append(_delay_stats(t_all, v_all, prefix))

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
                    "transport": last["transport"],
                    "wifi_freq_mhz": last["wifi_freq_mhz"],
                    "wifi_channel": last["wifi_channel"],
                    "wifi_bw_mhz": last["wifi_bw_mhz"],
                    "wifi_standard": last["wifi_standard"],
                    "wifi_rssi": last["wifi_rssi"],
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

    tput = {c: df[c][df[c] > 0].median() if df[c].notna().any() else None for c in ("down_mbps", "up_mbps")}
    stats = {
        "rtt_ms": _pcts(echo["rtt"] / 1e6) if not echo.empty else None,
        "ul_ms": _pcts(pd.concat(ul_v) / 1e6) if ul_v else None,
        "dl_ms": _pcts(pd.concat(dl_v) / 1e6) if dl_v else None,
        "down_mbps": None if pd.isna(tput["down_mbps"]) else round(float(tput["down_mbps"]), 1),
        "up_mbps": None if pd.isna(tput["up_mbps"]) else round(float(tput["up_mbps"]), 1),
        "loss": _udp_loss(td),
    }
    timing_streams, timing_detail = timing.summarize(td)
    out = {
        "v": SERIES_VERSION,
        "test_id": test_dir.name,
        "timing": timing_streams,
        "_timing_detail": timing_detail,
        "stats": stats,
        "peers": _peer_ips(td),
        "stalls": stalls.detect(td, off),
        "neighbors": neighbors,
        "plan": plan,
        "start": lo * BIN_NS / 1e9,
        "end": (hi + 1) * BIN_NS / 1e9,
        "t": [round(b * BIN_NS / 1e9, 1) for b in df.index],
    }
    for col in COLUMNS:
        vals = df[col].astype(object).where(df[col].notna(), None).tolist()
        out[col] = [round(v, 2) if isinstance(v, float) else v for v in vals]
    for col in ("lte_pci", "earfcn", "nr_pci", "nrarfcn", "wifi_freq_mhz", "wifi_channel", "wifi_bw_mhz", "wifi_rssi"):
        out[col] = [None if v is None else int(v) for v in out[col]]
    return out


def _newest_mtime(d: Path) -> float:
    return max((p.stat().st_mtime for p in d.rglob("*") if p.is_file()), default=0.0)


def _write_json(path: Path, obj) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    # unique per writer: a cron rebuild may overlap a manual one
    tmp = path.with_suffix(f"{path.suffix}.{os.getpid()}.tmp")
    tmp.write_text(json.dumps(obj, separators=(",", ":")))
    os.replace(tmp, path)


def _reverse_dns(cache_path: Path, ips: set[str], timeout_s: float = 3.0) -> dict[str, str | None]:
    """Reverse-DNS names for the UE's addresses (e.g. "m90-…cust.tele2.se"
    names the operator), cached on disk; a lookup that fails or times out
    is cached as None so it is not retried every build."""
    import concurrent.futures
    import socket

    cache = json.loads(cache_path.read_text()) if cache_path.exists() else {}
    todo = sorted(ips - cache.keys())
    if todo:
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            futures = {ip: pool.submit(socket.gethostbyaddr, ip) for ip in todo}
            for ip, fut in futures.items():
                try:
                    cache[ip] = fut.result(timeout=timeout_s)[0]
                except Exception:
                    cache[ip] = None
        _write_json(cache_path, cache)
    return cache


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
            detail = data.pop("_timing_detail", None) if data else None
            if detail:
                _write_json(out / "timing" / f"{test_dir.name}.json", detail)
            _write_json(cache, data)
        if data:
            tests.append(data)

    by_day: dict[str, list[dict]] = {}
    found: dict[str, dict] = {}
    hosts = _reverse_dns(out / "rdns.json", {ip for t in tests for ip in t.get("peers", [])})
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
        meta = [
            dict(
                {k: t.get(k) for k in ("test_id", "plan", "start", "end", "stats", "timing")},
                peers=[{"ip": ip, "host": hosts.get(ip)} for ip in t.get("peers", [])],
            )
            for t in day_tests
        ]
        neighbors = [n for t in day_tests for n in t.get("neighbors", [])]
        # Stall episodes with the time since the previous one that day (the
        # Pixel's recur on a fixed period, which shows up here).
        episodes, prev = [], {}
        for t in day_tests:
            for e in t.get("stalls", []):
                e = dict(e, test_id=t["test_id"], plan=t["plan"])
                p = prev.get(e["directions"])
                e["since_prev_s"] = round(e["start"] - p, 1) if p is not None else None
                prev[e["directions"]] = e["start"]
                episodes.append(e)
        # Clear-text band + frequency for every channel seen that day.
        channels = {
            "lte": {str(e): channel_label("LTE", e) for e in sorted({v for v in cols["earfcn"] if v is not None})},
            "nr": {str(n): channel_label("NR", n) for n in sorted({v for v in cols["nrarfcn"] if v is not None})},
        }
        day_data = {
            "day": day,
            "tests": meta,
            "channels": channels,
            "neighbors": neighbors,
            "stall_episodes": episodes,
            **cols,
        }
        _write_json(out / "series" / f"{day}.json", day_data)
        found[day] = findings.day_findings(day_data)
        radio = {t["test_id"]: t.get("radio") for t in found[day]["tests"]}
        _write_json(
            out / "spectrogram" / f"{day}.json",
            timing.day_spectrogram([dict(t, radio=radio.get(t["test_id"])) for t in meta], out / "timing"),
        )
    # The pause period is clearer over all days (more intervals).
    all_pauses = [e["start"] for f in found.values() for e in f["link_pauses"]]
    period_all = findings.pause_period(all_pauses)
    for day, f in found.items():
        f["pause_period_all_days"] = period_all
        _write_json(out / "findings" / f"{day}.json", f)
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

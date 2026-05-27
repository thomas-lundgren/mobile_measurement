"""Standard plots for one test (spec §14): RTT/OWD over time, packet timing
(jitter), radio metrics, the GPS route, and a radio↔latency correlation.

Matplotlib only (Agg backend — no display needed). Every plotter tolerates
missing inputs and returns ``None`` when there's nothing to draw, so a
mobile-only or radio-less capture still produces whatever it can.
"""

from __future__ import annotations

from pathlib import Path

import matplotlib

matplotlib.use("Agg")  # headless; must precede pyplot import
import matplotlib.pyplot as plt  # noqa: E402
import numpy as np  # noqa: E402
import pandas as pd  # noqa: E402

from . import clock, metrics  # noqa: E402
from .loader import TestData  # noqa: E402

NS_PER_S = 1e9
NS_PER_MS = 1e6


def _t0(td: TestData) -> int | None:
    """Test start = earliest packet monotonic time (the plot time origin)."""
    mono = pd.to_numeric(td.packets.get("mono_ns"), errors="coerce").dropna()
    return int(mono.min()) if not mono.empty else None


def _rel_s(series: pd.Series, t0: int) -> np.ndarray:
    return (pd.to_numeric(series, errors="coerce").to_numpy(dtype="float") - t0) / NS_PER_S


def _serving_rsrp(radio: pd.DataFrame) -> pd.Series:
    return radio["rsrp"].where(radio["rsrp"].notna(), radio["lte_rsrp"])


def plot_rtt(td: TestData):
    rtt = metrics.probe_rtt(td)
    t0 = _t0(td)
    if rtt.empty or t0 is None:
        return None
    fig, ax = plt.subplots(figsize=(9, 3.5))
    ax.plot(_rel_s(rtt["mono_ns"], t0), rtt["rtt_ns"].astype("float") / NS_PER_MS,
            ".", ms=3, color="tab:blue")
    ax.set(xlabel="time (s)", ylabel="RTT (ms)", title="Probe RTT over time")
    ax.grid(True, alpha=0.3)
    return fig


def plot_owd(td: TestData):
    offset = clock.estimate_offset(td)
    owd = metrics.realtime_owd(td, offset=offset)
    if owd.empty or owd["owd_corrected_ns"].isna().all():
        return None
    # x-axis in UTC: uplink (server) and downlink (mobile) don't share a mono epoch.
    t0u = int(pd.to_numeric(owd["utc_ns"], errors="coerce").dropna().min())
    fig, ax = plt.subplots(figsize=(9, 3.5))
    for direction, color in (("downlink", "tab:green"), ("uplink", "tab:orange")):
        sub = owd[(owd["direction_owd"] == direction) & owd["owd_corrected_ns"].notna()]
        if not sub.empty:
            ax.plot(_rel_s(sub["utc_ns"], t0u), sub["owd_corrected_ns"].astype("float") / NS_PER_MS,
                    ".", ms=3, label=direction, color=color)
    ax.set(xlabel="time (s)", ylabel="one-way delay (ms)",
           title=f"OWD over time (offset-corrected, {offset.method})")
    ax.legend()
    ax.grid(True, alpha=0.3)
    return fig


def plot_jitter(td: TestData):
    j = metrics.jitter(td)
    if j.empty:
        return None
    # x-axis in UTC: a multi-flow plan may have ToServer flows received on the
    # server and FromServer flows received on the mobile; their mono clocks
    # share no epoch, so a mono-based axis splits them by thousands of seconds.
    t0u = int(pd.to_numeric(j["utc_ns"], errors="coerce").dropna().min())
    fig, ax = plt.subplots(figsize=(9, 3.5))
    # Label = flow id + the *direction in which the packets arrived*: received
    # on the mobile = downlink (server sent); received on the server = uplink.
    for (flow_id, node), sub in j.groupby(["flow_id", "node"]):
        direction = "downlink" if node == "mobile" else "uplink"
        ax.plot(_rel_s(sub["utc_ns"], t0u), sub["ipdv_ns"].astype("float") / NS_PER_MS,
                ".", ms=3, label=f"flow {int(flow_id)} {direction}")
    ax.axhline(0, color="k", lw=0.6)
    ax.set(xlabel="time (s)", ylabel="IPDV: actual − expected (ms)",
           title="Inter-arrival jitter over time")
    ax.legend()
    ax.grid(True, alpha=0.3)
    return fig


def plot_radio(td: TestData):
    if td.radio is None or td.radio.empty:
        return None
    t0 = _t0(td)
    r = td.radio.sort_values("mono_ns")
    rsrp = pd.to_numeric(_serving_rsrp(r), errors="coerce")
    if t0 is None or rsrp.dropna().empty:
        return None
    fig, ax = plt.subplots(figsize=(9, 3.5))
    ax.plot(_rel_s(r["mono_ns"], t0), rsrp, "-", color="tab:red", label="serving RSRP")
    ax.set(xlabel="time (s)", ylabel="RSRP (dBm)", title="Serving-cell RSRP over time")
    sinr = pd.to_numeric(r["sinr"], errors="coerce")
    if not sinr.dropna().empty:
        ax2 = ax.twinx()
        ax2.plot(_rel_s(r["mono_ns"], t0), sinr, "-", color="tab:purple", alpha=0.6, label="SINR")
        ax2.set_ylabel("SINR (dB)")
    ax.grid(True, alpha=0.3)
    return fig


def plot_route(td: TestData):
    if td.gps is None or td.gps.empty:
        return None
    g = td.gps.dropna(subset=["latitude", "longitude"]).sort_values("mono_ns")
    if g.empty:
        return None
    fig, ax = plt.subplots(figsize=(6, 6))
    ax.plot(g["longitude"], g["latitude"], "-o", ms=3, color="tab:blue")
    ax.plot(g["longitude"].iloc[0], g["latitude"].iloc[0], "go", ms=8, label="start")
    ax.plot(g["longitude"].iloc[-1], g["latitude"].iloc[-1], "rs", ms=8, label="end")
    ax.set(xlabel="longitude", ylabel="latitude", title=f"GPS route ({len(g)} fixes)")
    ax.ticklabel_format(useOffset=False, style="plain")
    ax.legend()
    ax.grid(True, alpha=0.3)
    return fig


def plot_radio_latency_correlation(td: TestData):
    """Latency vs serving RSRP — does worse radio mean worse latency? RTT samples
    are matched to the nearest radio sample by monotonic time."""
    if td.radio is None or td.radio.empty:
        return None
    rtt = metrics.probe_rtt(td)
    if rtt.empty:
        return None
    r = td.radio.copy()
    r["serving_rsrp"] = pd.to_numeric(_serving_rsrp(r), errors="coerce")
    r = r.dropna(subset=["serving_rsrp", "mono_ns"]).sort_values("mono_ns")
    if r.empty:
        return None
    left = rtt[["mono_ns", "rtt_ns"]].dropna().sort_values("mono_ns").copy()
    left["mono_ns"] = left["mono_ns"].astype("int64")
    r["mono_ns"] = r["mono_ns"].astype("int64")
    merged = pd.merge_asof(left, r[["mono_ns", "serving_rsrp"]], on="mono_ns", direction="nearest")
    fig, ax = plt.subplots(figsize=(6, 4.5))
    ax.plot(merged["serving_rsrp"], merged["rtt_ns"].astype("float") / NS_PER_MS,
            ".", ms=3, color="tab:blue")
    ax.set(xlabel="serving RSRP (dBm)", ylabel="RTT (ms)", title="RTT vs serving RSRP")
    ax.grid(True, alpha=0.3)
    return fig


_PLOTTERS = {
    "rtt": plot_rtt,
    "owd": plot_owd,
    "jitter": plot_jitter,
    "radio": plot_radio,
    "route": plot_route,
    "radio_latency": plot_radio_latency_correlation,
}


def write_all(td: TestData, out_dir: str | Path) -> list[Path]:
    """Generate every applicable plot as a PNG; returns the files written."""
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)
    written: list[Path] = []
    for name, fn in _PLOTTERS.items():
        fig = fn(td)
        if fig is None:
            continue
        path = out / f"{name}.png"
        fig.savefig(path, dpi=110, bbox_inches="tight")
        plt.close(fig)
        written.append(path)
    return written

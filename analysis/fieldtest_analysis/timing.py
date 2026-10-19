"""Per-test delivery-timing summary for the findings page.

For each fast realtime UDP stream (≥ ``MIN_PPS``; 1 ms streams and up) it
combines ``delivery`` (batching, hold time, excess delay, host wake-up,
send outcomes, modem queue limit) and ``spectral`` (periodicities the path
adds). ``summarize`` returns the compact per-stream summary that goes into
the day files, plus the spectra for ``viz/timing/<test_id>.json``.
``day_summary`` condenses a day's streams for the page's callout.
"""

from __future__ import annotations

from collections import defaultdict

import numpy as np
import pandas as pd

from . import delivery, spectral
from .loader import TestData

#: Streams slower than this (packets/s) are not timing streams.
MIN_PPS = 500
#: Spectrum plot resolution (Hz) for the page.
PLOT_STEP_HZ = 2.0


def _sig(a, digits: int = 3) -> list:
    a = np.asarray(a, dtype=float)
    return [None if not np.isfinite(v) else float(f"{v:.{digits}g}") for v in a]


def _pool(freqs: np.ndarray, power: np.ndarray, step: float) -> tuple[np.ndarray, np.ndarray]:
    """Max-pool a spectrum onto a coarser grid (keeps narrow lines visible)."""
    k = max(1, int(round(step / (freqs[1] - freqs[0]))))
    n = len(power) // k * k
    return freqs[:n:k], power[:n].reshape(-1, k).max(axis=1)


def summarize(td: TestData) -> tuple[list[dict], dict]:
    pk = td.packets
    if pk.empty:
        return [], {}
    streams, detail = [], []
    spectra = {(e["flow_id"], e["direction"]): e for e in spectral.analyze(td)}
    for e in delivery.analyze(td):
        sender_node = "mobile" if e["direction"] == "uplink" else "server"
        sent = pk[(pk["node"] == sender_node) & (pk["flow_id"] == e["flow_id"]) & (pk["direction"] == "SENT")]
        if len(sent) < 100:
            continue
        mono = pd.to_numeric(sent["mono_ns"], errors="coerce")
        span = (mono.max() - mono.min()) / 1e9
        pps = len(sent) / span if span > 0 else 0
        if pps < MIN_PPS:
            continue
        size = int(pd.to_numeric(sent["payload_size"], errors="coerce").median())
        d = e["delivery"]
        sp = spectra.get((e["flow_id"], e["direction"]))
        s = {
            "flow_id": e["flow_id"],
            "direction": e["direction"],
            "pps": round(pps),
            "size": size,
            "mbps": round(pps * size * 8 / 1e6, 1),
            "sender": {k: e.get("sender", {}).get(k) for k in
                       ("status", "local_drop_share", "pacing_us", "send_call_us", "outq_bytes", "blocked_share")},
            "network_loss_share": e.get("network_loss_share"),
            "queue_limit": e.get("queue_limit"),
            "delivery": {k: d.get(k) for k in
                         ("clock", "batches", "batch_interval_us", "grid_alignment", "hold_ms",
                          "excess_delay_ms", "excess_delay_peaks_ms", "host_wakeup_us")},
            "lines": sp["arrival_peaks"] if sp else [],
            "delay_peaks": sp["delay_peaks"] if sp else [],
        }
        streams.append(s)
        if sp:
            a, dl = sp["_spectra"]["arrival"], sp["_spectra"]["delay"]
            f, rx = _pool(a["freqs"], a["rx"], PLOT_STEP_HZ)
            tx = _pool(a["freqs"], a["tx"], PLOT_STEP_HZ)[1] if a["tx"] is not None else None
            item = {"flow_id": e["flow_id"], "direction": e["direction"],
                    "arrival": {"f": _sig(f, 5), "rx": _sig(rx), "tx": _sig(tx) if tx is not None else None}}
            if dl:
                item["delay"] = {"f": _sig(dl["freqs"][1:], 4), "psd": _sig(dl["psd"][1:])}
            detail.append(item)
    return streams, ({"streams": detail} if detail else {})


def day_summary(tests: list[dict]) -> dict:
    """Condense a day's timing streams: added spectral lines by fundamental,
    and the modem-queue picture (packets held under back pressure; delay
    ceilings of overloaded streams without it)."""
    lines = defaultdict(lambda: {"uplink": 0, "downlink": 0})
    n = {"uplink": 0, "downlink": 0}
    held, ceilings = [], []
    for t in tests:
        for s in t.get("timing") or []:
            n[s["direction"]] += 1
            seen = {p["fundamental_hz"] for p in s["lines"] if p.get("added_by_path")}
            for hz in seen:
                lines[hz][s["direction"]] += 1
            q = s.get("queue_limit")
            if not q:
                continue
            if q.get("backpressure_share", 0) >= delivery.BACKPRESSURE_SHARE:
                held.append(q["held_packets"])
            elif (s["delivery"].get("excess_delay_ms") or {}).get("p99") is not None:
                ceilings.append(s["delivery"]["excess_delay_ms"]["p99"])
    return {
        "streams": n,
        "added_lines": sorted(
            ({"hz": hz, **c} for hz, c in lines.items() if c["uplink"] + c["downlink"] >= 2),
            key=lambda x: -(x["uplink"] + x["downlink"]),
        )[:8],
        "backpressure": {"streams": len(held), "held_packets_median": float(np.median(held)) if held else None,
                         "held_packets_range": [min(held), max(held)] if held else None},
        "overload_delay_ceilings_ms": sorted(round(c) for c in ceilings),
    }

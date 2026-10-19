"""Link-stall episodes: bursts of short, repeated pauses in packet arrival.

A constant-rate UDP stream (probe / realtime) arrives every few ms. A *stall*
is a gap in arrivals longer than ``STALL_MS`` in one direction (downlink:
the mobile receiving, uplink: the server receiving). Stalls closer together
than ``EPISODE_JOIN_S`` form an *episode*; one with at least
``MIN_STALLS`` stalls is reported, classified by ``directions``:

- ``both``: at least ``MIN_STALLS`` stalls each way, mostly at the same
  instants. The Pixel 9 Pro shows these as ~26 stalls of ~120–140 ms over
  ~4.3 s, every ~5 min, with packets held rather than lost — a device-side
  pause (e.g. dual-SIM tune-away), not queueing.
- ``uplink`` / ``downlink``: gaps in one direction only (e.g. uplink packets
  arriving in ~100 ms bursts under load).

``kind`` separates the two causes of both-direction episodes by stall length:
``link_pause`` (stalls of ``PAUSE_STALL_MS``, the device-side pause above) or
``congestion`` (shorter, ~95–100 ms stalls: an overloaded uplink delivering
in 100 ms bursts, which stalls the echo / downlink too). One-way episodes are
``gap``.

Times are on the server clock: mobile arrival times minus the estimated
offset O = mobile - server.
"""

from __future__ import annotations

import numpy as np
import pandas as pd

from . import schema
from .clock import OffsetEstimate, estimate_offset
from .loader import TestData

STALL_MS = 80.0
#: Median stall length (ms) of a link pause; shorter both-way stalls are congestion.
PAUSE_STALL_MS = (108.0, 200.0)
EPISODE_JOIN_S = 1.0
MIN_STALLS = 3
#: Only streams this dense can show a stall (median inter-arrival, ms).
MAX_STREAM_GAP_MS = 25.0
#: An uplink stall "coincides" with a downlink one if it ends this close.
COINCIDE_MS = 40.0


def _arrival_times_s(td: TestData, node: str, o_ns: float) -> np.ndarray:
    pk = td.packets
    r = pk[(pk["node"] == node) & (pk["direction"] == schema.DIR_RECEIVED) & (pk["protocol"] == "UDP")]
    t = pd.to_numeric(r["utc_ns"], errors="coerce").dropna().astype("float").to_numpy()
    if node == "mobile":
        t = t - o_ns
    return np.sort(t) / 1e9


def _stalls(t: np.ndarray) -> list[tuple[float, float]]:
    """(start, end) of every arrival gap longer than STALL_MS."""
    if t.size < 50:
        return []
    gaps = np.diff(t) * 1000
    if np.median(gaps) > MAX_STREAM_GAP_MS:
        return []
    idx = np.nonzero(gaps > STALL_MS)[0]
    # A gap at the very start/end is the stream starting/stopping, not a stall.
    return [(float(t[i]), float(t[i + 1])) for i in idx]


def detect(td: TestData, offset: OffsetEstimate | None = None) -> list[dict]:
    """Stall episodes in one test, oldest first."""
    if td.packets.empty:
        return []
    if offset is None:
        offset = estimate_offset(td)
    o = float(offset.offset_ns) if offset.available else 0.0
    by_dir = {
        "downlink": _stalls(_arrival_times_s(td, "mobile", o)),
        "uplink": _stalls(_arrival_times_s(td, "server", o)),
    }
    events = sorted((s, e, d) for d, lst in by_dir.items() for s, e in lst)
    episodes: list[list] = []
    for ev in events:
        if episodes and ev[0] - episodes[-1][-1][1] <= EPISODE_JOIN_S:
            episodes[-1].append(ev)
        else:
            episodes.append([ev])

    out = []
    for ep in episodes:
        dl = [(s, e) for s, e, d in ep if d == "downlink"]
        ul = [(s, e) for s, e, d in ep if d == "uplink"]
        if max(len(dl), len(ul)) < MIN_STALLS:
            continue
        durations = [(e - s) * 1000 for s, e, _ in ep]
        # Both directions when most stalls of the smaller side line up with the other.
        both = False
        if len(dl) >= MIN_STALLS and len(ul) >= MIN_STALLS:
            ends = np.array([e for _, e in ul])
            hits = sum(np.min(np.abs(ends - e)) * 1000 <= COINCIDE_MS for _, e in dl)
            both = hits >= 0.5 * min(len(dl), len(ul))
        median = float(np.median(durations))
        kind = "gap"
        if both:
            kind = "link_pause" if PAUSE_STALL_MS[0] <= median <= PAUSE_STALL_MS[1] else "congestion"
        out.append(
            {
                "start": round(min(s for s, _, _ in ep), 3),
                "end": round(max(e for _, e, _ in ep), 3),
                "stalls_downlink": len(dl),
                "stalls_uplink": len(ul),
                "stall_ms_median": round(median, 1),
                "stall_ms_max": round(float(np.max(durations)), 1),
                "directions": "both" if both else ("downlink" if len(dl) >= len(ul) else "uplink"),
                "kind": kind,
            }
        )
    return out

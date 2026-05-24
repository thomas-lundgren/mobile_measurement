"""Clock-offset estimation — the piece that makes cross-node OWD real.

OWD is ``recv_utc - sender_utc`` across two clocks, so it is contaminated by the
offset ``O = mobile_utc - server_utc`` between them. Removing ``O`` is what turns
a raw OWD (which is dominated by the unsynchronised clock offset — ~1 s on
undisciplined phones) into a real network delay.

Offset sources, in priority order:

1. **clock.parquet** (``estimated_offset_ns`` on both nodes) — a disciplined
   offset the Logger measures on-device via a UDP NTP-style burst against the
   server's time responder (HTTP single-timestamp fallback). The minimum-
   uncertainty (min-RTT) sample is used. Preferred when present.
2. **probe-RTT symmetry** — derived from data we already have. Probe RTT is
   measured on the mobile's monotonic clock (offset-free), so the one-way delay
   is ~RTT/2 under path symmetry; comparing that to the raw downlink OWD yields
   ``O``. The absolute OWD then carries a ±RTT/2 uncertainty (the asymmetry we
   can't resolve without a disciplined clock), while OWD *variation* (jitter) is
   exact regardless.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np
import pandas as pd

from . import metrics
from .loader import TestData


@dataclass
class OffsetEstimate:
    """O = mobile_utc - server_utc, with a 1-sigma-ish bound and how we got it."""

    offset_ns: float | None
    uncertainty_ns: float | None
    method: str  # "clock_parquet" | "probe_rtt_symmetry" | "none"

    @property
    def available(self) -> bool:
        return self.offset_ns is not None


def _from_clock_parquet(td: TestData) -> OffsetEstimate | None:
    cl = td.clock
    if cl.empty:
        return None
    have = {}
    for node in ("mobile", "server"):
        sub = cl[(cl["node"] == node) & cl["estimated_offset_ns"].notna()].copy()
        if sub.empty:
            continue
        sub["uncertainty_ns"] = pd.to_numeric(sub["uncertainty_ns"], errors="coerce")
        # NTP best-practice: trust the lowest-RTT (lowest-uncertainty) sample —
        # it carries the least queuing asymmetry — rather than averaging.
        best = sub.sort_values("uncertainty_ns", na_position="last").iloc[0]
        off = float(best["estimated_offset_ns"])
        unc = float(best["uncertainty_ns"]) if pd.notna(best["uncertainty_ns"]) else 0.0
        have[node] = (off, unc)
    # estimated_offset_ns is each node's local clock minus its reference; the
    # mobile-vs-server offset is their difference.
    if "mobile" in have and "server" in have:
        o = have["mobile"][0] - have["server"][0]
        u = have["mobile"][1] + have["server"][1]
        return OffsetEstimate(o, u, "clock_parquet")
    if "mobile" in have:  # server assumed reference-disciplined (offset ~0)
        return OffsetEstimate(have["mobile"][0], have["mobile"][1], "clock_parquet")
    return None


def _from_probe_symmetry(td: TestData) -> OffsetEstimate:
    rtt = metrics.probe_rtt(td)["rtt_ns"]
    owd = metrics.realtime_owd(td)  # naive (no offset applied)
    naive_dl = owd.loc[owd["direction_owd"] == "downlink", "owd_ns"]
    rtt = pd.to_numeric(rtt, errors="coerce").dropna()
    naive_dl = pd.to_numeric(naive_dl, errors="coerce").dropna()
    if rtt.empty or naive_dl.empty:
        return OffsetEstimate(None, None, "none")
    half_rtt = float(np.median(rtt)) / 2.0
    offset = float(np.median(naive_dl)) - half_rtt
    # Absolute OWD can't be pinned tighter than the path asymmetry, ~±RTT/2.
    return OffsetEstimate(offset, half_rtt, "probe_rtt_symmetry")


def estimate_offset(td: TestData) -> OffsetEstimate:
    """Best available estimate of O = mobile_utc - server_utc."""
    disciplined = _from_clock_parquet(td)
    if disciplined is not None:
        return disciplined
    return _from_probe_symmetry(td)

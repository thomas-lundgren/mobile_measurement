"""Core measurements: RTT, one-way delay (with uncertainty), jitter, loss.

All timing is computed from the authoritative monotonic clock where a single
node observes both endpoints (RTT, jitter, inter-arrival); cross-node one-way
delay necessarily uses UTC and therefore carries a clock-uncertainty bound
(invariant 2, spec §11.3).
"""

from __future__ import annotations

import numpy as np
import pandas as pd

from . import schema
from .loader import TestData


def _pct(series: pd.Series, ps=(50, 90, 95, 99)) -> dict:
    s = pd.to_numeric(series, errors="coerce").dropna()
    if s.empty:
        return {f"p{p}": None for p in ps} | {"min": None, "max": None, "mean": None, "count": 0}
    out = {f"p{p}": float(np.percentile(s, p)) for p in ps}
    out |= {"min": float(s.min()), "max": float(s.max()), "mean": float(s.mean()), "count": int(s.size)}
    return out


def probe_rtt(td: TestData) -> pd.DataFrame:
    """Per-echo RTT: match a node's SENT row to its RECEIVED echo on
    (node, flow_id, sequence_number). Returns rows with `rtt_ns`."""
    pk = td.packets
    if pk.empty:
        return pd.DataFrame(columns=["node", "flow_id", "sequence_number", "rtt_ns"])
    sent = pk[pk["direction"] == schema.DIR_SENT]
    recv = pk[pk["direction"] == schema.DIR_RECEIVED]
    keys = ["node", "flow_id", "sequence_number"]
    merged = sent.merge(recv, on=keys, suffixes=("_sent", "_recv"))
    merged["rtt_ns"] = merged["mono_ns_recv"].astype("Int64") - merged["mono_ns_sent"].astype("Int64")
    merged["mono_ns"] = merged["mono_ns_recv"]  # echo-receive time, for plotting
    merged = merged[merged["rtt_ns"] >= 0]
    return merged[keys + ["rtt_ns", "mono_ns"]].reset_index(drop=True)


def realtime_owd(td: TestData, offset=None) -> pd.DataFrame:
    """One-way delay for RECEIVED packets carrying a sender UTC stamp.

    `owd_ns = utc_ns - sender_utc_ns` is the **raw** cross-clock delay (offset-
    contaminated). When an `offset` (clock.OffsetEstimate, O = mobile - server)
    is supplied, `owd_corrected_ns` removes it — downlink subtracts O, uplink
    adds it — and `uncertainty_ns` carries the offset's bound. Direction is
    downlink when the mobile received (server sent), else uplink."""
    pk = td.packets
    # utc_ns (not mono_ns) is the cross-node-comparable time axis: uplink rows
    # come from the server node, downlink from the mobile, and their monotonic
    # clocks share no epoch.
    cols = ["node", "flow_id", "sequence_number", "direction_owd",
            "owd_ns", "owd_corrected_ns", "uncertainty_ns", "mono_ns", "utc_ns"]
    if pk.empty:
        return pd.DataFrame(columns=cols)
    recv = pk[(pk["direction"] == schema.DIR_RECEIVED) & pk["sender_utc_ns"].notna()].copy()
    if recv.empty:
        return pd.DataFrame(columns=cols)
    recv["owd_ns"] = recv["utc_ns"].astype("Int64") - recv["sender_utc_ns"].astype("Int64")
    recv["direction_owd"] = np.where(recv["node"] == "mobile", "downlink", "uplink")

    if offset is not None and getattr(offset, "available", False):
        o = offset.offset_ns
        raw = recv["owd_ns"].astype("float")
        recv["owd_corrected_ns"] = np.where(recv["direction_owd"] == "downlink", raw - o, raw + o)
        recv["uncertainty_ns"] = offset.uncertainty_ns
    else:
        recv["owd_corrected_ns"] = pd.NA
        recv["uncertainty_ns"] = pd.NA
    return recv[cols].reset_index(drop=True)


def jitter(td: TestData) -> pd.DataFrame:
    """Inter-arrival deviation (IPDV) for RECEIVED realtime packets:
    `ipdv_ns = actual_interval_ns - expected_interval_ns`."""
    pk = td.packets
    # utc_ns is needed alongside mono_ns: when both ToServer (recv on server)
    # and FromServer (recv on mobile) rows are present, their mono clocks share
    # no epoch, so the cross-node time axis must use utc_ns (same reasoning as
    # realtime_owd).
    cols = ["node", "flow_id", "actual_interval_ns", "expected_interval_ns",
            "ipdv_ns", "mono_ns", "utc_ns"]
    if pk.empty:
        return pd.DataFrame(columns=cols)
    rt = pk[(pk["direction"] == schema.DIR_RECEIVED) & pk["actual_interval_ns"].notna()].copy()
    if rt.empty:
        return pd.DataFrame(columns=cols)
    rt["ipdv_ns"] = rt["actual_interval_ns"].astype("Int64") - rt["expected_interval_ns"].astype("Int64")
    return rt[cols].reset_index(drop=True)


def loss(td: TestData) -> pd.DataFrame:
    """Per-(node, flow_id) loss. Echo flows (both SENT and RECEIVED present)
    measure unecho'd sequence numbers; one-directional flows measure gaps in
    the received sequence range. A send-only flow can't measure loss locally
    (needs the receiver) and reports NA."""
    pk = td.packets
    rows = []
    if pk.empty:
        return pd.DataFrame(columns=["node", "flow_id", "sent", "received", "lost", "loss_ratio"])
    for (node, flow_id), grp in pk.groupby(["node", "flow_id"]):
        sent = grp[grp["direction"] == schema.DIR_SENT]
        recv = grp[grp["direction"] == schema.DIR_RECEIVED]
        sent_seq = set(sent["sequence_number"].dropna().tolist())
        recv_seq = set(recv["sequence_number"].dropna().tolist())
        if sent_seq and recv_seq:  # echo
            lost = len(sent_seq - recv_seq)
            n_sent, n_recv, ratio = len(sent_seq), len(sent_seq) - lost, lost / len(sent_seq)
        elif recv_seq:  # one-directional, measured at receiver
            lo, hi = min(recv_seq), max(recv_seq)
            expected = hi - lo + 1
            n_recv = len(recv_seq)
            lost = expected - n_recv
            n_sent, ratio = expected, (lost / expected if expected else 0.0)
        else:  # send-only: not locally measurable
            n_sent, n_recv, lost, ratio = len(sent_seq), 0, None, None
        rows.append(
            {
                "node": node,
                "flow_id": int(flow_id) if pd.notna(flow_id) else None,
                "sent": n_sent,
                "received": n_recv,
                "lost": lost,
                "loss_ratio": ratio,
            }
        )
    return pd.DataFrame(rows)


def rtt_stats(td: TestData) -> dict:
    return _pct(probe_rtt(td)["rtt_ns"])


def owd_stats(td: TestData, offset=None) -> dict:
    """Per-direction OWD stats. With an `offset` (clock.OffsetEstimate), reports
    the offset-corrected delay (real network delay) and the raw delay; without
    one, only the raw (offset-contaminated) delay."""
    owd = realtime_owd(td, offset=offset)
    out = {
        "offset_ns": offset.offset_ns if offset is not None else None,
        "offset_method": offset.method if offset is not None else "none",
        "offset_uncertainty_ns": offset.uncertainty_ns if offset is not None else None,
    }
    for direction in ("uplink", "downlink"):
        sub = owd[owd["direction_owd"] == direction]
        out[direction] = {
            "corrected_ns": _pct(sub["owd_corrected_ns"]),
            "raw_ns": _pct(sub["owd_ns"]),
        }
    return out


def jitter_stats(td: TestData) -> dict:
    j = jitter(td)
    ipdv = pd.to_numeric(j["ipdv_ns"], errors="coerce").dropna()
    return {
        "ipdv_abs": _pct(ipdv.abs()),
        "mean_gap_deviation_ns": float(ipdv.abs().mean()) if not ipdv.empty else None,
    }

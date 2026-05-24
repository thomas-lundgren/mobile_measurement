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
    merged = merged[merged["rtt_ns"] >= 0]
    return merged[keys + ["rtt_ns"]].reset_index(drop=True)


def _nearest_uncertainty(clock: pd.DataFrame, node: str, at_utc: pd.Series) -> pd.Series:
    """Clock uncertainty_ns for `node` at each utc in `at_utc` (nearest sample)."""
    if clock.empty:
        return pd.Series([pd.NA] * len(at_utc), index=at_utc.index, dtype="Int64")
    cl = clock[(clock["node"] == node) & clock["uncertainty_ns"].notna()]
    if cl.empty:
        return pd.Series([pd.NA] * len(at_utc), index=at_utc.index, dtype="Int64")
    left = pd.DataFrame({"utc_ns": at_utc.astype("int64")}).reset_index()
    right = cl[["utc_ns", "uncertainty_ns"]].dropna(subset=["utc_ns"]).sort_values("utc_ns")
    right["utc_ns"] = right["utc_ns"].astype("int64")
    merged = pd.merge_asof(
        left.sort_values("utc_ns"), right, on="utc_ns", direction="nearest"
    )
    return merged.set_index("index")["uncertainty_ns"].reindex(at_utc.index).astype("Int64")


def realtime_owd(td: TestData) -> pd.DataFrame:
    """One-way delay for RECEIVED packets carrying a sender UTC stamp.
    `owd_ns = utc_ns - sender_utc_ns`; direction is downlink when the mobile
    received (server sent) and uplink when the server received. Uncertainty is
    the sum of the receiver's and sender's nearest clock-uncertainty bounds
    (NA when that node has no clock.parquet)."""
    pk = td.packets
    cols = ["node", "flow_id", "sequence_number", "direction_owd", "owd_ns", "uncertainty_ns"]
    if pk.empty:
        return pd.DataFrame(columns=cols)
    recv = pk[(pk["direction"] == schema.DIR_RECEIVED) & pk["sender_utc_ns"].notna()].copy()
    if recv.empty:
        return pd.DataFrame(columns=cols)
    recv["owd_ns"] = recv["utc_ns"].astype("Int64") - recv["sender_utc_ns"].astype("Int64")
    recv["direction_owd"] = np.where(recv["node"] == "mobile", "downlink", "uplink")
    # sender node is the opposite of the receiver node
    recv["sender_node"] = np.where(recv["node"] == "mobile", "server", "mobile")

    recv_unc = _nearest_uncertainty(td.clock, "mobile", recv["utc_ns"])
    parts = []
    for sender_node, grp in recv.groupby("sender_node"):
        r_unc = pd.concat(
            [_nearest_uncertainty(td.clock, n, g["utc_ns"]) for n, g in grp.groupby("node")]
        ).reindex(grp.index)
        s_unc = _nearest_uncertainty(td.clock, sender_node, grp["sender_utc_ns"])
        grp = grp.copy()
        grp["uncertainty_ns"] = r_unc.add(s_unc, fill_value=pd.NA)
        parts.append(grp)
    recv = pd.concat(parts).sort_index()
    _ = recv_unc  # receiver-side bound already folded in above
    return recv[cols].reset_index(drop=True)


def jitter(td: TestData) -> pd.DataFrame:
    """Inter-arrival deviation (IPDV) for RECEIVED realtime packets:
    `ipdv_ns = actual_interval_ns - expected_interval_ns`."""
    pk = td.packets
    cols = ["node", "flow_id", "actual_interval_ns", "expected_interval_ns", "ipdv_ns"]
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


def owd_stats(td: TestData) -> dict:
    owd = realtime_owd(td)
    out = {}
    for direction in ("uplink", "downlink"):
        sub = owd[owd["direction_owd"] == direction]
        out[direction] = _pct(sub["owd_ns"])
        unc = pd.to_numeric(sub["uncertainty_ns"], errors="coerce").dropna()
        out[direction]["uncertainty_ns_max"] = float(unc.max()) if not unc.empty else None
    return out


def jitter_stats(td: TestData) -> dict:
    j = jitter(td)
    ipdv = pd.to_numeric(j["ipdv_ns"], errors="coerce").dropna()
    return {
        "ipdv_abs": _pct(ipdv.abs()),
        "mean_gap_deviation_ns": float(ipdv.abs().mean()) if not ipdv.empty else None,
    }

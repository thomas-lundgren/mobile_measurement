"""Per-test summary (spec §12.8) assembled from the metrics + derived events."""

from __future__ import annotations

import pandas as pd

from . import clock, events, metrics
from .loader import TestData


def _radio_stats(td: TestData) -> dict:
    if td.radio is None or td.radio.empty:
        return {}
    r = td.radio

    def mmm(series):
        s = pd.to_numeric(series, errors="coerce").dropna()
        if s.empty:
            return None
        return {"min": float(s.min()), "mean": float(s.mean()), "max": float(s.max())}

    # Effective serving cell spans RATs (NR serving, else LTE anchor).
    serving_pci = r["pci"].where(r["pci"].notna(), r["lte_pci"])
    serving_rsrp = r["rsrp"].where(r["rsrp"].notna(), r["lte_rsrp"])
    pci = pd.to_numeric(serving_pci, errors="coerce").dropna()
    rat = "NR" if r["pci"].notna().any() else ("LTE" if r["lte_pci"].notna().any() else "UNKNOWN")
    return {
        "rat": rat,
        "serving_rsrp_dbm": mmm(serving_rsrp),
        "nr_rsrp_dbm": mmm(r["rsrp"]),
        "lte_rsrp_dbm": mmm(r["lte_rsrp"]),
        "sinr_db": mmm(r["sinr"]),
        "rsrq_db": mmm(r["rsrq"]),
        "samples": int(len(r)),
        "unique_serving_pci": int(pci.nunique()),
        "pci_changes": int((pci != pci.shift()).sum() - 1) if not pci.empty else 0,
    }


def _gps_endpoints(td: TestData) -> dict:
    if td.gps is None or td.gps.empty:
        return {}
    g = td.gps.dropna(subset=["latitude", "longitude"]).sort_values("mono_ns")
    if g.empty:
        return {}
    first, last = g.iloc[0], g.iloc[-1]
    return {
        "start": {"lat": float(first["latitude"]), "lon": float(first["longitude"])},
        "end": {"lat": float(last["latitude"]), "lon": float(last["longitude"])},
        "fixes": int(len(g)),
    }


def build_summary(td: TestData) -> dict:
    ev = events.derive_events(td)
    ev_counts = ev["type"].value_counts().to_dict() if not ev.empty else {}
    loss_df = metrics.loss(td)
    offset = clock.estimate_offset(td)
    return {
        "test_id": td.test_id,
        "nodes": td.nodes,
        "rtt_ns": metrics.rtt_stats(td),
        "owd_ns": metrics.owd_stats(td, offset=offset),
        "jitter": metrics.jitter_stats(td),
        "loss_per_flow": loss_df.to_dict(orient="records"),
        "radio": _radio_stats(td),
        "gps": _gps_endpoints(td),
        "derived_events": {
            "counts": {k: int(v) for k, v in ev_counts.items()},
            "handover_count": int(ev_counts.get("HANDOVER", 0)),
        },
    }

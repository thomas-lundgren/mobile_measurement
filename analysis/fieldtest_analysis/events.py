"""Events derived in post-processing from ``radio.parquet`` (spec §12.7).

These are NOT logged live: HANDOVER, BAND_CHANGE, COVERAGE_GAP, and
NEIGHBOR_COUNT_CHANGE are reconstructed from consecutive radio samples.
"""

from __future__ import annotations

import pandas as pd

from .loader import TestData

# RSRP at/below this (dBm) counts as a coverage gap.
COVERAGE_GAP_RSRP_DBM = -110


def _neighbor_count(val) -> int:
    if val is None:
        return 0
    try:
        return len(val)
    except TypeError:
        return 0


def derive_events(td: TestData, *, coverage_gap_rsrp=COVERAGE_GAP_RSRP_DBM) -> pd.DataFrame:
    cols = ["type", "mono_ns", "utc_ns", "detail"]
    if td.radio is None or td.radio.empty:
        return pd.DataFrame(columns=cols)
    r = td.radio.sort_values("mono_ns").reset_index(drop=True)
    # Effective serving cell spans RATs: NR serving fields when present, else the
    # LTE anchor (a Pixel indoors is often LTE-only, with no NR serving cell).
    r["serving_pci"] = r["pci"].where(r["pci"].notna(), r["lte_pci"])
    r["serving_freq"] = r["nrarfcn"].where(r["nrarfcn"].notna(), r["earfcn"])
    r["serving_rsrp"] = r["rsrp"].where(r["rsrp"].notna(), r["lte_rsrp"])
    out: list[dict] = []

    prev = None
    in_gap = False
    gap_start = None
    for _, row in r.iterrows():
        if prev is not None:
            # HANDOVER: serving PCI or NR NCI changed between non-null samples
            for keyfield in ("serving_pci", "nci"):
                a, b = prev[keyfield], row[keyfield]
                if pd.notna(a) and pd.notna(b) and a != b:
                    out.append(
                        {
                            "type": "HANDOVER",
                            "mono_ns": row["mono_ns"],
                            "utc_ns": row["utc_ns"],
                            "detail": {"field": keyfield, "from": _scalar(a), "to": _scalar(b)},
                        }
                    )
                    break
            # BAND_CHANGE: serving carrier frequency changed (NRARFCN or EARFCN)
            a, b = prev["serving_freq"], row["serving_freq"]
            if pd.notna(a) and pd.notna(b) and a != b:
                out.append(
                    {
                        "type": "BAND_CHANGE",
                        "mono_ns": row["mono_ns"],
                        "utc_ns": row["utc_ns"],
                        "detail": {"from": _scalar(a), "to": _scalar(b)},
                    }
                )
            # NEIGHBOR_COUNT_CHANGE
            na, nb = _neighbor_count(prev["neighbor_cells"]), _neighbor_count(row["neighbor_cells"])
            if na != nb:
                out.append(
                    {
                        "type": "NEIGHBOR_COUNT_CHANGE",
                        "mono_ns": row["mono_ns"],
                        "utc_ns": row["utc_ns"],
                        "detail": {"from": na, "to": nb},
                    }
                )

        # COVERAGE_GAP spans on serving RSRP (NR or LTE anchor)
        rsrp = row["serving_rsrp"]
        low = pd.notna(rsrp) and float(rsrp) <= coverage_gap_rsrp
        if low and not in_gap:
            in_gap, gap_start = True, row
        elif not low and in_gap:
            out.append(_gap_event(gap_start, row))
            in_gap = False
        prev = row
    if in_gap:
        out.append(_gap_event(gap_start, r.iloc[-1]))

    return pd.DataFrame(out, columns=cols)


def _gap_event(start, end) -> dict:
    return {
        "type": "COVERAGE_GAP",
        "mono_ns": start["mono_ns"],
        "utc_ns": start["utc_ns"],
        "detail": {
            "duration_ns": int(end["mono_ns"]) - int(start["mono_ns"]),
            "min_rsrp": _scalar(start["serving_rsrp"]),
        },
    }


def _scalar(v):
    try:
        return v.item()
    except AttributeError:
        return v

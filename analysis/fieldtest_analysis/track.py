"""Radio samples placed on the map: every ``radio.parquet`` row under a data
root, joined to the nearest GPS fix of the same test.

    python -m fieldtest_analysis.track <data_root> [--db mp.db] -o track.json

``data_root`` holds ``ses_*/tst_*/`` test directories (the server's upload
root). Rows with no GPS fix within ``MAX_GAP_NS`` are dropped. ``--db`` adds
each test's plan name from the server database. Output is written atomically
so a reader never sees a half-written file.
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
import time
from pathlib import Path

import pandas as pd

from . import schema
from .loader import _read_one

#: Max distance in time between a radio sample and the GPS fix it is placed at.
MAX_GAP_NS = 5_000_000_000

# (first, last, band) — E-UTRA bands in use in Europe (36.101 §5.7.3).
_LTE_BANDS = [
    (0, 599, "B1"),
    (1200, 1949, "B3"),
    (2750, 3449, "B7"),
    (3450, 3799, "B8"),
    (6150, 6449, "B20"),
    (9210, 9659, "B28"),
    (9920, 10359, "B32"),
    (37750, 38249, "B38"),
]
# NR-ARFCN ranges (38.101-1 §5.4.2.3); n1/n3/n28 overlap LTE refarming bands.
_NR_BANDS = [
    (151600, 160600, "n28"),
    (342000, 357000, "n3"),
    (422000, 434000, "n1"),
    (620000, 653333, "n78"),
]


def _band(value, table) -> str | None:
    if pd.isna(value):
        return None
    v = int(value)
    for first, last, name in table:
        if first <= v <= last:
            return name
    return None


def lte_band(earfcn) -> str | None:
    return _band(earfcn, _LTE_BANDS)


def nr_band(nrarfcn) -> str | None:
    return _band(nrarfcn, _NR_BANDS)


def _plan_names(db: str | None) -> dict[str, str]:
    if not db:
        return {}
    with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as c:
        return dict(c.execute("SELECT test_id, plan_name FROM tests"))


def _value(v):
    if pd.isna(v):
        return None
    return v.item() if hasattr(v, "item") else v


def _round(v):
    return None if pd.isna(v) else round(float(v), 1)


def test_points(test_dir: Path, plan: str | None = None) -> list[dict]:
    # Only radio + GPS: packet files are large and not needed for the map.
    radio = _read_one(test_dir / "radio.parquet", schema.RADIO)
    gps = _read_one(test_dir / "gps.parquet", schema.GPS)
    if radio is None or radio.empty or gps is None or gps.empty:
        return []
    radio = radio.dropna(subset=["utc_ns"]).sort_values("utc_ns")
    gps = (
        gps.dropna(subset=["utc_ns", "latitude", "longitude"])
        .sort_values("utc_ns")[["utc_ns", "latitude", "longitude", "accuracy_m", "speed_mps"]]
        .rename(columns={"utc_ns": "gps_utc_ns"})
    )
    if radio.empty or gps.empty:
        return []
    radio = radio.assign(utc_ns=radio["utc_ns"].astype("int64"))
    gps = gps.assign(gps_utc_ns=gps["gps_utc_ns"].astype("int64"))
    joined = pd.merge_asof(
        radio,
        gps,
        left_on="utc_ns",
        right_on="gps_utc_ns",
        direction="nearest",
        tolerance=MAX_GAP_NS,
    ).dropna(subset=["latitude"])
    test_id = test_dir.name
    points = []
    for r in joined.itertuples(index=False):
        points.append(
            {
                "t": int(r.utc_ns) // 1_000_000,  # ms, for JS Date
                "lat": round(float(r.latitude), 6),
                "lon": round(float(r.longitude), 6),
                "acc": _round(r.accuracy_m),
                "speed": _round(r.speed_mps),
                "mode": _value(r.network_type),
                "lte_band": lte_band(r.earfcn),
                "earfcn": _value(r.earfcn),
                "lte_pci": _value(r.lte_pci),
                "lte_rsrp": _value(r.lte_rsrp),
                "nr_band": nr_band(r.nrarfcn),
                "nr_pci": _value(r.pci),
                "nr_rsrp": _value(r.rsrp),
                "nr_rsrq": _value(r.rsrq),
                "test_id": test_id,
                "plan": plan,
            }
        )
    return points


def build_track(root: str | Path, db: str | None = None) -> dict:
    plans = _plan_names(db)
    points = []
    for test_dir in sorted(Path(root).glob("ses_*/tst_*")):
        points.extend(test_points(test_dir, plans.get(test_dir.name)))
    points.sort(key=lambda p: p["t"])
    return {"generated_utc_ms": int(time.time() * 1000), "points": points}


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="fieldtest-track")
    ap.add_argument("root", help="data root holding ses_*/tst_* test directories")
    ap.add_argument("--db", help="server SQLite database, for plan names")
    ap.add_argument("-o", "--output", required=True, help="write track JSON here")
    args = ap.parse_args(argv)

    track = build_track(args.root, args.db)
    out = Path(args.output)
    out.parent.mkdir(parents=True, exist_ok=True)
    tmp = out.with_suffix(out.suffix + ".tmp")
    tmp.write_text(json.dumps(track, separators=(",", ":")))
    os.replace(tmp, out)
    print(f"wrote {out} ({len(track['points'])} points)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

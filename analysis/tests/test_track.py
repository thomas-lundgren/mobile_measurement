"""Radio samples joined to GPS fixes for the map."""

from __future__ import annotations

import json

import pandas as pd

from fieldtest_analysis.track import build_track, channel_label, lte_band, lte_dl_mhz, main, nr_band, nr_mhz

S = 1_000_000_000


def _write_test(root, radio: list[dict], gps: list[dict]):
    d = root / "ses_A" / "tst_A"
    d.mkdir(parents=True)
    pd.DataFrame(radio).to_parquet(d / "radio.parquet")
    pd.DataFrame(gps).to_parquet(d / "gps.parquet")


def test_bands():
    assert lte_band(1750) == "B3"
    assert lte_band(3600) == "B8"
    assert lte_band(6400) == "B20"
    assert lte_band(None) is None
    assert nr_band(644640) == "n78"
    assert nr_band(423850) == "n1"
    assert nr_band(1) is None


def test_radio_rows_take_nearest_fix_within_gap(tmp_path):
    t0 = 1_792_000_000 * S
    _write_test(
        tmp_path,
        radio=[
            {"mono_ns": 1, "utc_ns": t0, "earfcn": 1750, "lte_rsrp": -100, "network_type": "NR_NSA",
             "pci": 351, "nrarfcn": 423850, "rsrp": -104},
            {"mono_ns": 2, "utc_ns": t0 + 3 * S, "earfcn": 3600, "lte_rsrp": -90, "network_type": "LTE_CA"},
            {"mono_ns": 3, "utc_ns": t0 + 60 * S, "earfcn": 3600, "network_type": "LTE_CA"},  # no fix near
        ],
        gps=[
            {"mono_ns": 1, "utc_ns": t0 + S, "latitude": 55.7, "longitude": 13.18, "accuracy_m": 5.0},
            {"mono_ns": 2, "utc_ns": t0 + 4 * S, "latitude": 55.8, "longitude": 13.20, "accuracy_m": 9.0},
        ],
    )
    pts = build_track(tmp_path)["points"]
    assert len(pts) == 2
    assert (pts[0]["lat"], pts[0]["mode"], pts[0]["lte_band"], pts[0]["nr_band"]) == (55.7, "NR_NSA", "B3", "n1")
    assert pts[0]["nr_rsrp"] == -104
    assert (pts[1]["lat"], pts[1]["mode"], pts[1]["lte_band"], pts[1]["nr_rsrp"]) == (55.8, "LTE_CA", "B8", None)
    assert pts[0]["test_id"] == "tst_A"


def test_cli_writes_json(tmp_path):
    t0 = 1_792_000_000 * S
    _write_test(
        tmp_path,
        radio=[{"mono_ns": 1, "utc_ns": t0, "earfcn": 6400}],
        gps=[{"mono_ns": 1, "utc_ns": t0, "latitude": 55.7, "longitude": 13.18}],
    )
    out = tmp_path / "viz" / "track.json"
    assert main([str(tmp_path), "-o", str(out)]) == 0
    data = json.loads(out.read_text())
    assert data["points"][0]["lte_band"] == "B20"
    assert data["points"][0]["mode"] is None


def test_frequencies():
    assert lte_dl_mhz(3600) == 940.0  # B8: 925 + 0.1 * (3600 - 3450)
    assert lte_dl_mhz(1750) == 1860.0  # B3
    assert lte_dl_mhz(6400) == 816.0  # B20
    assert nr_mhz(644640) == 3669.6  # n78
    assert nr_mhz(423850) == 2119.25  # n1
    assert channel_label("LTE", 3600) == "B8 · 940 MHz"
    assert channel_label("NR", 644640) == "n78 · 3669.6 MHz"
    assert channel_label("LTE", 99999) == "ARFCN 99999"
    assert channel_label("NR", None) is None

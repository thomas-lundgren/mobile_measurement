"""Derived radio events: deterministic synthetic + real-capture smoke."""

from __future__ import annotations

from fieldtest_analysis import events
from fieldtest_analysis.loader import load_test
from tests.conftest import make_radio, synthetic_test


def test_derive_events_exact():
    radio = make_radio([
        dict(mono_ns=0, utc_ns=0, pci=100, nci=1, nrarfcn=600, rsrp=-90, neighbor_cells=[1, 2]),
        dict(mono_ns=1_000_000_000, utc_ns=0, pci=100, nci=1, nrarfcn=600, rsrp=-90, neighbor_cells=[1, 2]),
        dict(mono_ns=2_000_000_000, utc_ns=0, pci=200, nci=2, nrarfcn=600, rsrp=-90, neighbor_cells=[1]),  # handover + neighbor change
        dict(mono_ns=3_000_000_000, utc_ns=0, pci=200, nci=2, nrarfcn=700, rsrp=-115, neighbor_cells=[1]),  # band change + gap start
        dict(mono_ns=4_000_000_000, utc_ns=0, pci=200, nci=2, nrarfcn=700, rsrp=-90, neighbor_cells=[1]),   # gap end
    ])
    td = synthetic_test(radio=radio)
    ev = events.derive_events(td)
    counts = ev["type"].value_counts().to_dict()
    assert counts.get("HANDOVER") == 1
    assert counts.get("BAND_CHANGE") == 1
    assert counts.get("NEIGHBOR_COUNT_CHANGE") == 1
    assert counts.get("COVERAGE_GAP") == 1
    gap = ev[ev["type"] == "COVERAGE_GAP"].iloc[0]
    assert gap["detail"]["duration_ns"] == 1_000_000_000


def test_no_radio_no_events():
    assert events.derive_events(synthetic_test()).empty


def test_real_radio_events_run(udp_test_dir):
    td = load_test(udp_test_dir)
    ev = events.derive_events(td)
    # well-formed (may legitimately be empty on a stationary indoor capture)
    assert list(ev.columns) == ["type", "mono_ns", "utc_ns", "detail"]

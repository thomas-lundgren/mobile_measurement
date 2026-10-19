"""Day-level findings: pause period and regime-change classification."""

from __future__ import annotations

from fieldtest_analysis.findings import day_findings, pause_period


def test_pause_period_prefers_true_period_over_its_fraction():
    p = 304.5
    starts = [0, 2 * p, 3 * p, 7 * p, 9 * p, 11 * p + 1, 13 * p - 1]
    found = pause_period(starts)
    assert abs(found["period_s"] - p) < 1.0
    assert found["fits"] == found["intervals"]


def test_pause_period_needs_two_intervals():
    assert pause_period([0, 300]) is None


def _day(dl_before, dl_after, down_before, down_after, pci_change=False):
    n = 60  # 12 s of 200 ms bins, change at bin 30
    t = [1000 + 0.2 * i for i in range(n)]
    col = lambda a, b: [a] * 30 + [b] * 30  # noqa: E731
    pci = [101] * 20 + [202] * 40 if pci_change else [101] * n
    return {
        "day": "2026-10-19",
        "t": t,
        "dl_med": col(dl_before, dl_after),
        "ul_med": [20.0] * n,
        "down_mbps": col(down_before, down_after),
        "up_mbps": [None] * n,
        "mode": ["LTE_CA"] * n,
        "earfcn": [3600] * n,
        "nrarfcn": [None] * n,
        "lte_pci": pci,
        "nr_pci": [None] * n,
        "channels": {"lte": {"3600": "B8 · 940 MHz"}, "nr": {}},
        "stall_episodes": [],
        "tests": [{"test_id": "tst_A", "plan": "hol-bulk-down", "start": t[0], "end": t[-1], "stats": None}],
    }


def test_capacity_rise_is_found_and_radio_labelled():
    f = day_findings(_day(120.0, 30.0, 50.0, 90.0))
    (r,) = f["regime_changes"]
    assert r["direction"] == "downlink" and r["kind"] == "radio capacity change"
    assert f["tests"][0]["radio"] == "4G CA · B8"


def test_change_after_handover_is_attributed_to_it():
    f = day_findings(_day(120.0, 30.0, 50.0, 90.0, pci_change=True))
    assert f["regime_changes"][0]["kind"] == "after handover"
    assert "101 → 202" in f["regime_changes"][0]["explanation"]


def test_no_change_without_throughput_gain():
    assert day_findings(_day(120.0, 30.0, 50.0, 50.0))["regime_changes"] == []

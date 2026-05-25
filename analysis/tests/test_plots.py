"""Plots: each plotter renders or tolerantly returns None; write_all emits PNGs."""

from __future__ import annotations

from fieldtest_analysis import plots
from fieldtest_analysis.loader import load_test


def test_write_all_udp_capture(udp_test_dir, tmp_path):
    td = load_test(udp_test_dir)
    written = plots.write_all(td, tmp_path)
    names = {p.stem for p in written}
    # probe + realtime-downlink + radio + gps → the full set
    assert names == {"rtt", "owd", "jitter", "radio", "route", "radio_latency"}
    for p in written:
        assert p.suffix == ".png" and p.stat().st_size > 0


def test_two_node_has_owd_no_route(twonode_test_dir, tmp_path):
    td = load_test(twonode_test_dir)
    # no GPS in this capture → no route plot
    assert plots.plot_route(td) is None
    names = {p.stem for p in plots.write_all(td, tmp_path)}
    assert "route" not in names
    assert {"rtt", "owd", "jitter", "radio"}.issubset(names)


def test_plotters_tolerate_empty():
    from fieldtest_analysis.loader import TestData
    from pathlib import Path

    empty = TestData(path=Path("/x"))
    assert plots.plot_rtt(empty) is None
    assert plots.plot_owd(empty) is None
    assert plots.plot_radio(empty) is None
    assert plots.write_all(empty, Path("/tmp/mp-empty-plots")) == []

"""Binned time series for the charts."""

from __future__ import annotations

import json
import shutil

from fieldtest_analysis import series
from fieldtest_analysis.series import COLUMNS, build


def test_series_columns_align(twonode_test_dir):
    s = series.test_series(twonode_test_dir)
    assert s is not None
    n = len(s["t"])
    assert n > 0
    assert all(len(s[c]) == n for c in COLUMNS)
    assert s["t"] == sorted(s["t"])
    assert any(v is not None for v in s["rtt_med"]) or any(v is not None for v in s["ul_med"])


def test_build_writes_day_files_and_cache(twonode_test_dir, tmp_path):
    root = tmp_path / "data"
    shutil.copytree(twonode_test_dir, root / "ses_X" / "tst_X")
    out = tmp_path / "viz"
    days = build(root, out)
    assert len(days) == 1
    assert (out / "tests" / "tst_X.json").exists()
    day = json.loads((out / "series" / f"{days[0]}.json").read_text())
    assert day["tests"][0]["test_id"] == "tst_X"
    assert day["t"][-1] is not None and day["rtt_med"][-1] is None  # gap row after the test
    assert json.loads((out / "series" / "days.json").read_text())["days"] == days


def test_one_way_delay_never_exceeds_rtt_in_a_bin(twonode_test_dir):
    """Delays are binned at send time, so a probe's legs share its RTT bin."""
    s = series.test_series(twonode_test_dir)
    pairs = [(r, u) for r, u in zip(s["rtt_med"], s["ul_med"]) if r is not None and u is not None]
    assert pairs
    assert all(u <= r + 2.0 for r, u in pairs)


def test_uploads_during_counts_other_tests_files_in_the_window():
    import pandas as pd

    from fieldtest_analysis.series import uploads_during

    files = pd.DataFrame(
        {"test_id": ["tst_prev", "tst_prev", "tst_prev", "tst_this"], "t": [99.0, 105.0, 121.5, 110.0],
         "bytes": [5e6, 1e6, 2e5, 9e6]}  # fmt: skip
    )
    # test measured 100..120 s: the 105 s file and the one landing 1.5 s after
    # the end count; the file before the start and the test's own do not
    assert uploads_during(files, "tst_this", 100.0, 120.0) == 1.2
    assert uploads_during(files.iloc[:0], "tst_this", 100.0, 120.0) == 0.0

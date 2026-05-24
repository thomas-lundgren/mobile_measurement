"""Summary assembly + CLI on a real capture."""

from __future__ import annotations

import json

from fieldtest_analysis import build_summary, load_test
from fieldtest_analysis.cli import main


def test_summary_shape_and_json_serializable(udp_test_dir):
    td = load_test(udp_test_dir)
    s = build_summary(td)
    for key in ("test_id", "nodes", "rtt_ns", "owd_ns", "jitter", "loss_per_flow", "radio", "gps", "derived_events"):
        assert key in s
    assert s["nodes"] == ["mobile"]
    assert s["rtt_ns"]["count"] > 0
    assert s["radio"]["samples"] > 0
    # must be JSON-serializable as-is (CLI relies on this)
    json.dumps(s, default=str)


def test_cli_writes_output(bidir_test_dir, tmp_path, capsys):
    out = tmp_path / "summary.json"
    rc = main([str(bidir_test_dir), "-o", str(out)])
    assert rc == 0
    data = json.loads(out.read_text())
    assert sorted(r["flow_id"] for r in data["loss_per_flow"]) == [1, 2, 3, 4]

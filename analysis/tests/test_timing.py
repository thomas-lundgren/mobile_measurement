"""Per-test timing summary for the findings page."""

from __future__ import annotations

from test_delivery import _uplink

from fieldtest_analysis import timing


def test_fast_stream_is_summarized_with_spectrum():
    streams, detail = timing.summarize(_uplink(grant_ms=4))
    [s] = streams
    assert s["direction"] == "uplink" and abs(s["pps"] - 1000) <= 2 and s["size"] == 200
    assert s["delivery"]["batches"]["mean_size"] > 3.5
    assert any(p["hz"] == 250.0 and p["added_by_path"] for p in s["lines"])  # 4 ms grants
    [d] = detail["streams"]
    assert len(d["arrival"]["f"]) == len(d["arrival"]["rx"]) > 1000


def test_day_summary_counts_added_lines_and_backpressure():
    s = {"direction": "uplink", "lines": [{"hz": 400.0, "fundamental_hz": 400.0, "added_by_path": True}],
         "delivery": {"excess_delay_ms": {"p99": 950.0}},
         "queue_limit": {"backpressure_share": 0.5, "held_packets": 2400}}
    s2 = dict(s, queue_limit={"backpressure_share": 0.0, "held_packets": 1800})
    out = timing.day_summary([{"timing": [s]}, {"timing": [s2]}])
    assert out["added_lines"] == [{"hz": 400.0, "uplink": 2, "downlink": 0}]
    assert out["backpressure"]["streams"] == 1 and out["backpressure"]["held_packets_median"] == 2400
    assert out["overload_delay_ceilings_ms"] == [950]

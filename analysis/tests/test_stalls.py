"""Stall-episode detection on synthetic streams."""

from __future__ import annotations

from conftest import make_packets, synthetic_test

from fieldtest_analysis import stalls

MS = 1_000_000


def _stream(node: str, times_ms: list[float]) -> list[dict]:
    return [
        {"node": node, "flow_id": 1, "sequence_number": i, "direction": "RECEIVED", "protocol": "UDP",
         "mono_ns": int(t * MS), "utc_ns": int(t * MS), "payload_size": 200}
        for i, t in enumerate(times_ms)
    ]


def _with_stalls(stall_at_ms: list[float], length_ms: float = 120, end_ms: float = 10_000) -> list[float]:
    """A 10 ms stream; each stall holds packets, which then arrive together."""
    out = []
    for t in range(0, int(end_ms), 10):
        held = next((s + length_ms for s in stall_at_ms if s <= t < s + length_ms), None)
        out.append(float(held if held is not None else t))
    return sorted(out)


def _td(rows):
    return synthetic_test(packets=make_packets(rows))


def test_clean_stream_has_no_episode():
    t = _with_stalls([])
    assert stalls.detect(_td(_stream("mobile", t) + _stream("server", t))) == []


def test_repeated_stalls_in_both_directions_form_one_episode():
    starts = [2000 + 150 * k for k in range(10)]
    t = _with_stalls(starts)
    eps = stalls.detect(_td(_stream("mobile", t) + _stream("server", t)))
    assert len(eps) == 1
    e = eps[0]
    assert e["directions"] == "both"
    assert e["stalls_downlink"] == 10 and e["stalls_uplink"] == 10
    assert 100 <= e["stall_ms_median"] <= 140


def test_isolated_stall_is_not_an_episode():
    t = _with_stalls([3000, 7000])
    assert stalls.detect(_td(_stream("mobile", t))) == []


def test_one_sided_stalls_are_not_both():
    t_dl = _with_stalls([2000 + 150 * k for k in range(10)])
    t_ul = _with_stalls([2000])  # one coinciding stall only
    eps = stalls.detect(_td(_stream("mobile", t_dl) + _stream("server", t_ul)))
    assert [e["directions"] for e in eps] == ["downlink"]


def test_both_way_stall_length_tells_pause_from_congestion():
    pause = _with_stalls([2000 + 150 * k for k in range(10)], length_ms=120)
    [e] = stalls.detect(_td(_stream("mobile", pause) + _stream("server", pause)))
    assert e["directions"] == "both" and e["kind"] == "link_pause"
    burst = _with_stalls([2000 + 150 * k for k in range(10)], length_ms=95)
    [e] = stalls.detect(_td(_stream("mobile", burst) + _stream("server", burst)))
    assert e["directions"] == "both" and e["kind"] == "congestion"

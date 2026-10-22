"""The DSCP page's summary: rows per burst stream, sweeps, and what the path
did with each marking."""

from __future__ import annotations

from fieldtest_analysis import dscp


def _test(tid, start, transport, sent, arrived, loss, activity="video_chunk"):
    return {
        "test_id": tid, "plan": f"dscp-video-{sent}", "start": start, "end": start + 60,
        "transport": [transport] * 3, "lte_rsrp": [-95, -96, -97], "wifi_rssi": [-56] * 3, "nr_rsrp": [],
        "app": {"bursts": [{
            "activity": activity, "direction": "downlink", "dscp_sent": sent, "dscp_recv": arrived,
            "dscp_kept": None if sent is None else (arrived.get(sent, 0) / max(1, sum(arrived.values()))),
            "loss": loss, "planned": 20000, "arrived": 20000 - int(20000 * loss),
            "extra_delay_ms": {"p50": 3.0, "p99": 50.0}, "ipdv_ms": {"p50": 0.6, "p99": 6.0},
        }]},
    }  # fmt: skip


def test_rows_sweeps_and_outcomes():
    tests = [
        _test("t1", 1000, "CELLULAR", None, {0: 20000}, 0.0),
        _test("t2", 1070, "CELLULAR", 46, {0: 17036, 40: 1690}, 0.064),
        _test("t3", 1140, "CELLULAR", 34, {32: 18500}, 0.0),
        _test("t4", 1210, "CELLULAR", 46, {0: 17030, 40: 1689}, 0.064),
        # a Wi-Fi sweep later: kept
        _test("t5", 9000, "WIFI", 46, {46: 19350}, 0.033),
        {"test_id": "plain", "start": 500, "end": 560, "app": None},
    ]
    s = dscp.summary(tests)
    assert len(s["rows"]) == 5
    assert [(w["path"], w["tests"]) for w in s["sweeps"]] == [("mobile data", 4), ("Wi-Fi", 1)]
    by = {(m["path"], m["dscp_sent"]): m for m in s["by_marking"]}
    ef = by[("mobile data", 46)]
    assert (ef["tests"], ef["outcome"], ef["loss"]) == (2, "bleached", 0.064)
    assert ef["arrived_as"] == {0: 34066, 40: 3379}
    assert by[("mobile data", 34)]["outcome"] == "re-marked to 32"
    assert by[("Wi-Fi", 46)]["outcome"] == "kept"
    assert by[("mobile data", None)]["outcome"] == "arrived unmarked"
    assert by[("Wi-Fi", 46)]["workload"] == "video (8 Mbit/s bursts)"


def test_mixed_when_no_value_dominates():
    s = dscp.summary([_test("t", 0, "CELLULAR", 46, {0: 600, 40: 400}, 0.0)])
    assert s["by_marking"][0]["outcome"] == "mixed"


def test_bursts_are_not_realtime_streams():
    """A burst's datagrams share their object id as sequence number and carry
    sender times: the realtime analyses (delay, delivery timing, spectra)
    must not take them for a stream."""
    import pandas as pd
    from conftest import make_packets, synthetic_test

    from fieldtest_analysis import delivery, metrics

    rows = []
    for i in range(200):  # one burst, one object id
        rows.append({"node": "mobile", "flow_id": 4, "sequence_number": 1 << 20, "direction": "RECEIVED",
                     "protocol": "UDP", "utc_ns": 1_000_000 * i + 5_000_000, "mono_ns": 0,
                     "sender_utc_ns": 1_000_000 * i, "app_activity": "video_chunk", "app_packet_index": i,
                     "payload_size": 1200})  # fmt: skip
    td = synthetic_test(packets=make_packets(rows))
    assert delivery.analyze(td) == []
    assert metrics.realtime_owd(td).empty
    assert isinstance(td.packets, pd.DataFrame)

"""Metrics: deterministic synthetic assertions + real-capture smoke checks."""

from __future__ import annotations

import pandas as pd

from fieldtest_analysis import metrics
from fieldtest_analysis.loader import load_test
from tests.conftest import make_clock, make_packets, synthetic_test


def test_probe_rtt_exact():
    # probe echo: 5 sent, seq 2 not echoed
    sent = [dict(node="mobile", flow_id=1, sequence_number=s, direction="SENT", mono_ns=100 + s * 100, utc_ns=0) for s in range(5)]
    recv = [dict(node="mobile", flow_id=1, sequence_number=s, direction="RECEIVED", mono_ns=150 + s * 100 + s * 10, utc_ns=0) for s in (0, 1, 3, 4)]
    td = synthetic_test(packets=make_packets(sent + recv))
    rtt = metrics.probe_rtt(td)
    assert len(rtt) == 4
    # seq0: 150-100=50; seq1: 260-200=60; seq3: 480-400=80; seq4: 590-500=90
    assert sorted(rtt["rtt_ns"].tolist()) == [50, 60, 80, 90]


def test_loss_echo_and_oneway():
    sent = [dict(node="mobile", flow_id=1, sequence_number=s, direction="SENT", mono_ns=s, utc_ns=0) for s in range(5)]
    recv = [dict(node="mobile", flow_id=1, sequence_number=s, direction="RECEIVED", mono_ns=s, utc_ns=0) for s in (0, 1, 3, 4)]
    # one-directional downlink flow 2: seq 0..9 but 3 missing
    dl = [dict(node="mobile", flow_id=2, sequence_number=s, direction="RECEIVED", mono_ns=s, utc_ns=0) for s in range(10) if s not in (4, 7)]
    td = synthetic_test(packets=make_packets(sent + recv + dl))
    loss = metrics.loss(td).set_index("flow_id")
    assert loss.loc[1, "lost"] == 1 and abs(loss.loc[1, "loss_ratio"] - 0.2) < 1e-9
    # flow2: range 0..9 = 10 expected, 8 received → 2 lost
    assert loss.loc[2, "lost"] == 2 and abs(loss.loc[2, "loss_ratio"] - 0.2) < 1e-9


def test_owd_raw_without_offset():
    recv = [dict(node="mobile", flow_id=2, sequence_number=0, direction="RECEIVED",
                 mono_ns=0, utc_ns=1100, sender_utc_ns=1000)]
    td = synthetic_test(packets=make_packets(recv))
    owd = metrics.realtime_owd(td)
    assert owd["direction_owd"].tolist() == ["downlink"]
    assert owd["owd_ns"].tolist() == [100]  # raw cross-clock delay
    assert owd["owd_corrected_ns"].isna().all()  # no offset → no correction


def test_owd_corrected_with_offset():
    from fieldtest_analysis.clock import OffsetEstimate

    recv = [dict(node="mobile", flow_id=2, sequence_number=s, direction="RECEIVED",
                 mono_ns=s, utc_ns=1_000_000_200 + s, sender_utc_ns=100 + s)
            for s in range(3)]  # raw downlink owd = 1_000_000_100 (offset + 100ns)
    td = synthetic_test(packets=make_packets(recv))
    # O = mobile - server = 1e9; corrected downlink = raw - O = 100ns real delay
    off = OffsetEstimate(offset_ns=1_000_000_000, uncertainty_ns=2500, method="probe_rtt_symmetry")
    owd = metrics.realtime_owd(td, offset=off)
    assert owd["owd_corrected_ns"].tolist() == [100.0, 100.0, 100.0]
    assert owd["uncertainty_ns"].tolist() == [2500, 2500, 2500]


def test_jitter_ipdv():
    recv = [
        dict(node="mobile", flow_id=2, sequence_number=s, direction="RECEIVED",
             mono_ns=s, utc_ns=s, actual_interval_ns=a, expected_interval_ns=10000)
        for s, a in enumerate([10000, 10500, 9500, 12000])
    ]
    td = synthetic_test(packets=make_packets(recv))
    j = metrics.jitter(td)
    assert j["ipdv_ns"].tolist() == [0, 500, -500, 2000]


# ---- real-capture smoke checks ----

def test_two_node_capture_has_uplink_owd(twonode_test_dir):
    """The server node's RECEIVED rows (uplink flow) populate uplink OWD, which a
    mobile-only capture cannot. Both nodes present, both directions resolved."""
    from fieldtest_analysis import clock

    td = load_test(twonode_test_dir)
    assert td.nodes == ["mobile", "server"]
    # flow-scoped server segments concatenated without collision
    assert "server" in td.packets["node"].unique()
    est = clock.estimate_offset(td)
    owd = metrics.realtime_owd(td, offset=est)
    up = owd[owd["direction_owd"] == "uplink"]
    dn = owd[owd["direction_owd"] == "downlink"]
    assert len(up) > 0, "server RECEIVED rows give uplink OWD"
    assert len(dn) > 0, "mobile RECEIVED rows give downlink OWD"
    # corrected, both directions land at network scale
    stats = metrics.owd_stats(td, offset=est)
    assert abs(stats["uplink"]["corrected_ns"]["p50"]) < 100_000_000
    assert abs(stats["downlink"]["corrected_ns"]["p50"]) < 100_000_000


def test_real_udp_capture_metrics(udp_test_dir):
    td = load_test(udp_test_dir)
    rtt = metrics.probe_rtt(td)
    assert len(rtt) > 0 and (rtt["rtt_ns"] >= 0).all()  # probe echoes present
    owd = metrics.realtime_owd(td)
    assert (owd["direction_owd"] == "downlink").any()   # realtime FromServer arrived
    loss = metrics.loss(td)
    ratios = loss["loss_ratio"].dropna()
    assert ((ratios >= 0) & (ratios <= 1)).all()

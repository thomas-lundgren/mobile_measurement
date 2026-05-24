"""Clock-offset estimation: recovers an injected offset and corrects real OWD."""

from __future__ import annotations

from fieldtest_analysis import clock, metrics
from fieldtest_analysis.loader import load_test
from tests.conftest import make_clock, make_packets, synthetic_test


def test_offset_from_clock_parquet_difference():
    cl = make_clock([
        dict(node="mobile", mono_ns=0, utc_ns=0, estimated_offset_ns=1_500, uncertainty_ns=10),
        dict(node="server", mono_ns=0, utc_ns=0, estimated_offset_ns=500, uncertainty_ns=20),
    ])
    est = clock.estimate_offset(synthetic_test(clock=cl))
    assert est.method == "clock_parquet"
    assert est.offset_ns == 1000  # 1500 - 500
    assert est.uncertainty_ns == 30


def test_offset_probe_symmetry_recovers_injected_offset():
    # probe echo: RTT = 10_000 ns on the mobile mono clock (one-way ~5_000)
    sent = [dict(node="mobile", flow_id=1, sequence_number=s, direction="SENT",
                 mono_ns=s * 1000, utc_ns=0) for s in range(50)]
    recv = [dict(node="mobile", flow_id=1, sequence_number=s, direction="RECEIVED",
                 mono_ns=s * 1000 + 10_000, utc_ns=0) for s in range(50)]
    # realtime downlink: true one-way 5_000 ns, plus injected clock offset O=1e9
    O = 1_000_000_000
    dl = [dict(node="mobile", flow_id=2, sequence_number=s, direction="RECEIVED",
               mono_ns=s, utc_ns=O + 5_000 + s, sender_utc_ns=s)  # raw = O + 5_000
          for s in range(50)]
    td = synthetic_test(packets=make_packets(sent + recv + dl))

    est = clock.estimate_offset(td)
    assert est.method == "probe_rtt_symmetry"
    # O recovered as median(raw_dl) - RTT/2 = (O+5000) - 5000 = O
    assert abs(est.offset_ns - O) < 1.0
    assert est.uncertainty_ns == 5_000  # ±RTT/2

    owd = metrics.realtime_owd(td, offset=est)
    corrected = owd["owd_corrected_ns"].dropna()
    # corrected downlink ≈ true one-way (5_000), not the 1e9 raw
    assert corrected.abs().max() < 20_000


def test_clocked_capture_uses_disciplined_offset(clocked_test_dir):
    td = load_test(clocked_test_dir)
    assert not td.clock.empty  # device wrote clock.parquet
    # this capture used the UDP time responder (tight bound)
    assert (td.clock["source"] == "server_udp").all()
    est = clock.estimate_offset(td)
    # disciplined offset is preferred over the probe-RTT symmetry fallback
    assert est.method == "clock_parquet"
    assert 1e9 < est.offset_ns < 2e9  # ~1.19 s phone-ahead measured on-device
    # the chosen sample is the minimum-uncertainty (min-RTT) one, and UDP keeps
    # that well under the ~8 ms the HTTP fallback would give
    min_unc = td.clock["uncertainty_ns"].astype("int64").min()
    assert est.uncertainty_ns == min_unc
    assert est.uncertainty_ns < 5_000_000  # < 5 ms
    stats = metrics.owd_stats(td, offset=est)
    corr_p50 = stats["downlink"]["corrected_ns"]["p50"]
    assert abs(corr_p50) < 100_000_000  # network-scale, not the ~1.19 s raw


def test_real_capture_owd_becomes_network_scale(udp_test_dir):
    td = load_test(udp_test_dir)
    est = clock.estimate_offset(td)
    assert est.method == "probe_rtt_symmetry"
    assert est.offset_ns is not None
    stats = metrics.owd_stats(td, offset=est)
    raw_p50 = stats["downlink"]["raw_ns"]["p50"]
    corr_p50 = stats["downlink"]["corrected_ns"]["p50"]
    # raw is offset-dominated (~1s); corrected is network-scale (sub-100ms)
    assert raw_p50 > 100_000_000          # > 100 ms (really ~1.15 s)
    assert abs(corr_p50) < 100_000_000    # < 100 ms

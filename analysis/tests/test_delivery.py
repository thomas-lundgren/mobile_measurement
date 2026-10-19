"""Delivery timing / back pressure on synthetic 1 ms streams."""

from __future__ import annotations

from conftest import make_packets, synthetic_test

from fieldtest_analysis import delivery

MS = 1_000_000


def _uplink(n: int = 2000, grant_ms: int = 4, eagain_every: int = 0, harq_every: int = 0):
    """Phone sends every 1 ms; the server receives each packet at the next
    `grant_ms` boundary (+10 ms path), back to back 20 µs apart. Every
    `eagain_every`-th send fails locally; every `harq_every`-th delivered
    packet arrives 8 ms late."""
    rows, k = [], 0
    for i in range(n):
        t = i * MS
        status = "EAGAIN" if eagain_every and i % eagain_every == 0 else "OK"
        rows.append({"node": "mobile", "flow_id": 2, "sequence_number": i, "direction": "SENT", "protocol": "UDP",
                     "mono_ns": t, "utc_ns": t, "payload_size": 200, "send_status": status,
                     "send_call_ns": 20_000, "sock_outq_bytes": 0})
        if status != "OK":
            continue
        grant = ((i // grant_ms) + 1) * grant_ms * MS
        rx = grant + 10 * MS + (i % grant_ms) * 20_000
        if harq_every and k % harq_every == 0:
            rx += 8 * MS
        k += 1
        rows.append({"node": "server", "flow_id": 2, "sequence_number": i, "direction": "RECEIVED", "protocol": "UDP",
                     "mono_ns": rx + 50_000, "utc_ns": rx + 50_000, "kernel_rx_utc_ns": rx,
                     "sender_mono_ns": t, "sender_utc_ns": t, "payload_size": 200})
    return synthetic_test(packets=make_packets(rows))


def test_grant_batches_and_hold_time():
    [e] = delivery.analyze(_uplink(grant_ms=4))
    assert e["direction"] == "uplink"
    d = e["delivery"]
    assert d["clock"] == "kernel"
    assert d["batches"]["mean_size"] > 3.5
    assert 3900 < d["batch_interval_us"]["p50"] < 4100
    assert d["grid_alignment"]["1000us"] > 0.9
    assert 2.9 < d["hold_ms"]["p50"] < 3.1
    assert 45 < d["host_wakeup_us"]["p50"] < 55


def test_local_drops_are_not_network_loss():
    [e] = delivery.analyze(_uplink(eagain_every=10))
    assert e["sender"]["status"] == {"OK": 1800, "EAGAIN": 200}
    assert e["sender"]["local_drop_share"] == 0.1
    assert e["network_loss_share"] == 0.0


def test_harq_like_step_shows_as_excess_delay_peak():
    [e] = delivery.analyze(_uplink(grant_ms=1, harq_every=20))
    peaks = [p["at"] for p in e["delivery"]["excess_delay_peaks_ms"]]
    assert any(7.5 <= p <= 9.5 for p in peaks), peaks


def _overloaded(drain_every: int, wait_ms: int, n: int = 4000):
    """Phone offers 1 packet/ms; the uplink delivers only every
    `drain_every`-th one, each after `wait_ms` (the rest are dropped)."""
    rows = []
    for i in range(n):
        t = i * MS
        rows.append({"node": "mobile", "flow_id": 2, "sequence_number": i, "direction": "SENT", "protocol": "UDP",
                     "mono_ns": t, "utc_ns": t, "payload_size": 1000, "send_status": "OK"})
        if i % drain_every:
            continue
        rx = t + 10 * MS + (wait_ms * MS if i > 200 else 0)
        rows.append({"node": "server", "flow_id": 2, "sequence_number": i, "direction": "RECEIVED", "protocol": "UDP",
                     "mono_ns": rx, "utc_ns": rx, "kernel_rx_utc_ns": rx,
                     "sender_mono_ns": t, "sender_utc_ns": t, "payload_size": 1000})
    return synthetic_test(packets=make_packets(rows))


def test_queue_limit_from_drain_rate_and_wait():
    [e] = delivery.analyze(_overloaded(drain_every=4, wait_ms=300))
    q = e["queue_limit"]
    assert q["loss_share"] == 0.75
    assert abs(q["drained_mbps"] - 2.0) < 0.05 and abs(q["wait_ms"] - 300) < 1
    assert abs(q["queue_kb"] - 75) < 2  # 2 Mbit/s × 0.3 s


def test_no_queue_limit_when_not_overloaded():
    [e] = delivery.analyze(_uplink())
    assert e["queue_limit"] is None


def test_backpressure_counts_as_overload_without_loss():
    td = _uplink()
    pk = td.packets
    sent = pk["direction"] == "SENT"
    pk.loc[sent, "sock_outq_bytes"] = 300_000  # OS queue full on every send
    [e] = delivery.analyze(td)
    q = e["queue_limit"]
    assert q is not None and q["backpressure_share"] == 1.0 and q["loss_share"] < 0.01

"""App traffic: objects, episodes and the realtime impact on synthetic logs."""

from __future__ import annotations

from conftest import make_packets, synthetic_test

from fieldtest_analysis import app

MS = 1_000_000


def _test():
    """A page (HTML 0–100 ms, two objects to 400 ms) and a photo upload
    (1.0–1.5 s) beside a 1 ms uplink stream whose packets wait 20 ms extra
    (and one in ten is lost) while app objects are in flight, 1 ms otherwise."""
    rows = []

    def obj(oid, act, size, t0, t1, upload=False):
        tag = {"flow_id": 9, "sequence_number": oid, "protocol": "TCP", "node": "mobile",
               "app_activity": act, "app_object_bytes": size}
        rows.append(dict(tag, direction="SENT", utc_ns=t0, mono_ns=t0, payload_size=24))
        if upload:
            rows.append(dict(tag, direction="SENT", utc_ns=t0 + MS, mono_ns=t0 + MS, payload_size=size))
            rows.append(dict(tag, direction="RECEIVED", utc_ns=t1, mono_ns=t1, payload_size=24))
        else:
            half = (t0 + t1) // 2
            rows.append(dict(tag, direction="RECEIVED", utc_ns=half, mono_ns=half, payload_size=size // 2))
            rows.append(dict(tag, direction="RECEIVED", utc_ns=t1, mono_ns=t1, payload_size=size - size // 2))

    ep = 1 << 20
    obj(ep | 0, "web_page", 50_000, 0, 100 * MS)
    obj(ep | 1, "web_object", 20_000, 100 * MS, 300 * MS)
    obj(ep | 2, "web_object", 30_000, 120 * MS, 400 * MS)
    obj(2 * ep, "photo_upload", 2_000_000, 1000 * MS, 1500 * MS, upload=True)

    for i in range(3000):
        t = i * MS
        busy = t <= 400 * MS or 1000 * MS <= t <= 1500 * MS
        rows.append({"node": "mobile", "flow_id": 1, "sequence_number": i, "direction": "SENT", "protocol": "UDP",
                     "utc_ns": t, "mono_ns": t, "payload_size": 200, "send_status": "OK"})
        if busy and i % 10 == 0:
            continue
        rx = t + (21 if busy else 1) * MS
        rows.append({"node": "server", "flow_id": 1, "sequence_number": i, "direction": "RECEIVED", "protocol": "UDP",
                     "utc_ns": rx, "mono_ns": rx, "sender_utc_ns": t, "sender_mono_ns": t, "payload_size": 200})
    return synthetic_test(packets=make_packets(rows))


def test_objects_and_page_load():
    r = app.analyze(_test())
    assert r["objects"] == 4
    assert r["mbytes_up"] == 2.0
    acts = {a["activity"]: a for a in r["activities"]}
    assert acts["web_object"]["objects"] == 2 and acts["web_object"]["complete_share"] == 1.0
    assert acts["web_page"]["wait_ms"]["p50"] == 50.0  # first half of the HTML
    assert acts["photo_upload"]["done_ms"]["p50"] == 500.0
    eps = {e["kind"]: e for e in r["episodes"]}
    assert eps["web_page"]["n"] == 1 and eps["web_page"]["ms"]["p50"] == 400.0  # page load time


def test_realtime_impact_active_vs_idle():
    [i] = app.analyze(_test())["impact"]
    assert i["direction"] == "uplink"
    assert i["active"]["excess_ms"]["p50"] == 20.0
    assert i["idle"]["excess_ms"]["p50"] == 0.0
    assert 0.05 < i["active"]["loss"] < 0.15
    assert i["idle"]["loss"] == 0.0


def test_no_app_traffic():
    rows = [{"node": "mobile", "flow_id": 1, "sequence_number": 0, "direction": "SENT", "protocol": "UDP",
             "utc_ns": 0, "mono_ns": 0, "payload_size": 200}]
    assert app.analyze(synthetic_test(packets=make_packets(rows))) is None

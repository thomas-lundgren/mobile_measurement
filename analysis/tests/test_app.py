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


def test_v2_llm_stream_and_tool_call():
    """An LLM call (20 kB up, 800 ms to the first token, a stream with one
    1.5 s stall), a parallel MCP call and an upload that only gets an ack."""
    rows = []

    def tag(oid, act, down, up):
        return {"flow_id": 9, "sequence_number": oid, "protocol": "TCP", "node": "mobile",
                "app_activity": act, "app_object_bytes": down, "app_up_bytes": up}

    def at(t, d, n, **k):
        rows.append(dict(k, direction=d, utc_ns=t, mono_ns=t, payload_size=n))

    llm = tag(1 << 20, "llm_call", 6000, 20_000)
    at(0, "SENT", 44, **llm)
    at(10 * MS, "SENT", 20_000, **llm)
    reads = [810 + 20 * i for i in range(20)] + [2600 + 20 * i for i in range(23)]
    for i, t in enumerate(reads):
        at(t * MS, "RECEIVED", 6000 // 43 + (6000 % 43 if i == 0 else 0), **llm)
    mcp = tag(2 << 20, "mcp_call", 4000, 1200)
    at(3500 * MS, "SENT", 44, **mcp)
    at(3501 * MS, "SENT", 1200, **mcp)
    at(3900 * MS, "RECEIVED", 4000, **mcp)
    up = tag(3 << 20, "photo_upload", 0, 300_000)
    at(5000 * MS, "SENT", 44, **up)
    at(5100 * MS, "SENT", 300_000, **up)
    at(5150 * MS, "RECEIVED", 44, **up)
    r = app.analyze(synthetic_test(packets=make_packets(rows)))
    acts = {a["activity"]: a for a in r["activities"]}
    assert r["mbytes_up"] == round((20_000 + 1200 + 300_000) / 1e6, 2)
    assert r["mbytes_down"] == 0.01
    a = acts["llm_call"]
    assert a["complete_share"] == 1.0
    assert a["think_ms"]["p50"] == 800.0  # after the context went up
    assert a["gap_ms"]["max"] == 1410.0 and a["stalls_1s"] == 1
    assert acts["mcp_call"]["think_ms"]["p50"] == 399.0
    p = acts["photo_upload"]
    assert p["complete_share"] == 1.0 and p["done_ms"]["p50"] == 150.0 and p["gap_ms"] is None

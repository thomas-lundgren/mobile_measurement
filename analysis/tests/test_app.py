"""App traffic: objects, episodes and the realtime impact on synthetic logs."""

from __future__ import annotations

import pandas as pd
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


def test_sends_after_the_receiver_stopped_are_not_loss():
    """The test ends on the receiver first; the sender's last seconds never
    had a chance to arrive."""
    td = _test()
    pk = td.packets
    tail = pk[(pk["node"] == "mobile") & (pk["flow_id"] == 1) & (pk["direction"] == "SENT")].tail(500).copy()
    tail["sequence_number"] += 3000
    tail["utc_ns"] += 5000 * MS
    tail["mono_ns"] += 5000 * MS
    td.packets = pd.concat([pk, tail], ignore_index=True)
    [i] = app.impact(td, app.objects(td))
    assert i["idle"]["loss"] == 0.0


def _events():
    """app_events of two agents. Agent 0 (an AI agent): task 1 = LLM turn,
    two tools, LLM turn, all ok, 0 → 5 s; task 2 = an LLM turn cut by the
    test's end. Agent 1: page views, task 1 (HTML + 2 objects) done in
    400 ms, task 2 still running at the end."""
    rows = []

    def ev(agent, task, step, kind, act, t_plan, t_start, t_first, t_done, outcome="ok", **plan):
        rows.append({
            "flow_id": 4, "object_id": len(rows), "agent": agent, "task": task, "step": step,
            "step_kind": kind, "activity": act, "episode": 0, "parallel": 1,
            "plan_up_bytes": plan.get("up", 0), "plan_down_bytes": plan.get("down", 1000),
            "plan_delay_ms": plan.get("delay", 0), "plan_rate_bps": plan.get("rate", 0), "plan_chunk_bytes": 0,
            "planned_mono_ns": t_plan * MS, "start_mono_ns": None if t_start is None else t_start * MS,
            "first_byte_mono_ns": None if t_first is None else t_first * MS,
            "done_mono_ns": None if t_done is None else t_done * MS,
            "bytes_up": 44, "bytes_down": plan.get("down", 1000) if outcome == "ok" else 10,
            "outcome": outcome, "new_connection": step == 1,
        })  # fmt: skip

    # LLM: 900 ms think, observed first byte 1000 ms; 6000 B at 48 kbit/s = 1000 ms planned, 1200 observed
    ev(0, 1, 1, "llm_turn", "llm_call", 0, 10, 1010, 2210, delay=900, down=6000, rate=48_000)
    ev(0, 1, 2, "tools", "mcp_call", 2210, 2220, 2400, 2500, delay=150)
    ev(0, 1, 2, "tools", "web_fetch", 2210, 2300, 2500, 2700, delay=100)
    ev(0, 1, 3, "llm_turn", "llm_call", 2700, 2700, 3700, 5000, delay=900, down=6000, rate=48_000)
    ev(0, 2, 1, "llm_turn", "llm_call", 9000, 9000, None, None, outcome="stopped", delay=900)
    ev(1, 1, 1, "page_html", "web_page", 0, 0, 50, 100)
    ev(1, 1, 2, "page_objects", "web_object", 100, 100, 150, 300)
    ev(1, 1, 2, "page_objects", "web_object", 100, 120, 200, 400)
    ev(1, 2, 1, "page_html", "web_page", 9000, 9000, 9050, 9100)
    td = synthetic_test()
    td.app_events = pd.DataFrame(rows)
    return td


def test_tasks_finished_time_and_steps():
    t = {x["kind"]: x for x in app.tasks(_events())}
    agent = t["agent task"]
    assert (agent["started"], agent["finished"]) == (2, 1)
    assert agent["ms"]["p50"] == 5000  # planned 0 → last byte 5000 ms
    assert agent["steps"]["p50"] == 3
    assert agent["objects_not_ok"] == 0.2  # 1 of agent 0's 5 objects
    page = t["page view"]
    assert (page["started"], page["finished"]) == (2, 1)  # the last one was still open
    assert page["ms"]["p50"] == 400


def test_intent_planned_against_observed():
    i = {x["activity"]: x for x in app.intent(_events())}
    llm = i["llm_call"]
    assert llm["outcomes"] == {"ok": 2, "stopped": 1}
    assert llm["think_plan_ms"]["p50"] == 900
    assert llm["first_byte_ms"]["p50"] == 1000
    assert llm["beyond_think_ms"]["p50"] == 100
    assert llm["stream_plan_ms"]["p50"] == 1000 and llm["stream_ms"]["p50"] == 1250  # 1200, 1300
    fetch = i["web_fetch"]
    assert fetch["conn_wait_ms"]["p50"] == 90  # planned 2210, started 2300


def test_no_app_events_no_tasks():
    assert app.tasks(synthetic_test()) is None and app.intent(synthetic_test()) is None


def test_open_loop_lateness_and_dropped_tasks():
    """Open loop: objects carry a due time. Task 1 ran (its objects 30 and
    50 ms late), task 2 arrived with the backlog full and was dropped."""
    rows = []
    for task, start, outcome in ((1, 30, "ok"), (1, 50, "ok"), (2, None, "skipped")):
        due = 1000 * task
        rows.append({
            "flow_id": 4, "object_id": len(rows), "agent": 0, "task": task, "step": 1, "step_kind": "llm_turn",
            "activity": "llm_call", "plan_up_bytes": 0, "plan_down_bytes": 100, "plan_delay_ms": 0, "plan_rate_bps": 0,
            "planned_mono_ns": due * MS, "due_mono_ns": due * MS,
            "start_mono_ns": None if start is None else (due + start) * MS,
            "first_byte_mono_ns": None if start is None else (due + start + 5) * MS,
            "done_mono_ns": None if start is None else (due + start + 10) * MS,
            "bytes_down": 100 if start else 0, "outcome": outcome, "new_connection": False,
        })  # fmt: skip
    td = synthetic_test()
    td.app_events = pd.DataFrame(rows)
    (t,) = app.tasks(td)
    assert (t["loop"], t["started"], t["dropped"]) == ("open", 2, 1)
    (i,) = app.intent(td)
    assert i["late_ms"]["p50"] == 40 and i["conn_wait_ms"] is None


def test_graph_wait_counts_from_the_last_dependency():
    """A fetch planned with its task at 0 ms but waiting for a search that
    ended at 500 ms, started at 510 ms: it waited 10 ms, not 510."""
    base = {"flow_id": 4, "agent": 0, "task": 1, "activity": "web_fetch", "plan_up_bytes": 0, "plan_down_bytes": 10,
            "plan_delay_ms": 0, "plan_rate_bps": 0, "planned_mono_ns": 0, "outcome": "ok", "bytes_down": 10}
    rows = [
        dict(base, object_id=1, step=1, step_kind="search", activity="web_search", after_objects=[],
             start_mono_ns=0, first_byte_mono_ns=400 * MS, done_mono_ns=500 * MS),
        dict(base, object_id=2, step=2, step_kind="fetch", after_objects=[1],
             start_mono_ns=510 * MS, first_byte_mono_ns=600 * MS, done_mono_ns=700 * MS),
    ]
    td = synthetic_test()
    td.app_events = pd.DataFrame(rows)
    i = {x["activity"]: x for x in app.intent(td)}
    assert i["web_fetch"]["conn_wait_ms"]["p50"] == 10

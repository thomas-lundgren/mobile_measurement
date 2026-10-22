"""App-like background traffic (``app_traffic`` flows): how the simulated
users' objects fared, and what their load did to the realtime streams.

Every object's rows share its ``object_id`` (``sequence_number``) and carry
``app_activity`` / ``app_object_bytes`` / ``app_up_bytes``
(contracts/packets/app_request.md). On the mobile node an object is the
request (SENT), the bytes going up (SENT: request body, LLM context, upload),
then RECEIVED: the response, or the server's acknowledgement when there is
none (``app_object_bytes`` 0). v1 rows (no ``app_up_bytes``) are either a
download or an upload. So per object:

- ``wait_ms``: request → first response byte (or acknowledgement);
- ``think_ms``: last byte up → first response byte: the server's think time
  (an LLM's time to first token, a tool's latency) plus the network;
- ``done_ms``: request → last byte (or acknowledgement);
- ``gap_ms``: the longest pause between response reads after the first — a
  stalled token stream or download;
- ``complete``: all response bytes arrived (or the upload was acknowledged).

Objects of one *episode* (``object_id >> 20``: a page view with its objects, a
mail sync, a video segment burst) together give e.g. the **page load time**.

**Tasks and intent** (tests whose phone wrote ``mobile/app_events_*.parquet``):
the scheduler's own record of every object — agent → task → step, what the
model asked for, and when it was planned, started, got its first byte and
ended, with its outcome. ``tasks`` gives per kind of task (an AI agent's task,
a page view, a mail sync, ...) how many finished and how long they took;
``intent`` gives per activity what was planned against what happened.

**Impact** on the realtime streams of the same test: their excess one-way
delay (delay minus the stream's minimum, so the clock offset cancels) and loss
while any app object was in flight versus while the apps were idle. Packets
are placed on the mobile clock: by send time for uplink, by arrival for
downlink.
"""

from __future__ import annotations

import numpy as np
import pandas as pd

from . import schema, stats
from .loader import realtime_received

REQUEST_BYTES = 24  # v1 request and acknowledgement
#: An app burst affects packets around it too; treat this much after the last
#: byte as still "active" (queues drain).
ACTIVE_TAIL_NS = 200_000_000



def objects(td) -> pd.DataFrame:
    """One row per app object the mobile node moved as a TCP exchange (bulk
    included). UDP bursts are datagrams, not request → response: their
    objects are in ``app_events``."""
    pk = td.packets
    if pk is None or pk.empty or "app_activity" not in pk:
        return pd.DataFrame()
    m = pk[(pk["node"] == "mobile") & pk["app_activity"].notna() & (pk["protocol"] != schema.PROTO_UDP)].copy()
    if m.empty:
        return pd.DataFrame()
    m["t"] = pd.to_numeric(m["utc_ns"], errors="coerce")
    m["size"] = pd.to_numeric(m["payload_size"], errors="coerce").fillna(0)
    m = m.sort_values("t", kind="stable")
    out = []
    has_up = "app_up_bytes" in m
    for (flow, oid), g in m.groupby(["flow_id", "sequence_number"], sort=False):
        req = g.iloc[0]
        rest = g.iloc[1:]
        total = int(pd.to_numeric(req.get("app_object_bytes"), errors="coerce") or 0)
        sent = rest[rest["direction"] == schema.DIR_SENT]
        got = rest[rest["direction"] == schema.DIR_RECEIVED]
        up_col = pd.to_numeric(req.get("app_up_bytes"), errors="coerce") if has_up else np.nan
        if pd.notna(up_col):  # v2: response size and upload size both known
            up, down = int(up_col), total
            upload = down == 0
        else:  # v1: either a download or an upload of `total` bytes
            upload = int(sent["size"].sum()) > 0
            up, down = (total, 0) if upload else (0, total)
        t_up_end = sent["t"].iloc[-1] if len(sent) else req["t"]
        if upload:
            end = got["t"].iloc[0] if len(got) else None
            first = end
            complete = len(got) > 0
            gap = None
        else:
            end = got["t"].iloc[-1] if len(got) else None
            first = got["t"].iloc[0] if len(got) else None
            complete = int(got["size"].sum()) >= down > 0
            gap = float(np.diff(got["t"].to_numpy(dtype="float")).max()) / 1e6 if len(got) >= 2 else None
        out.append({
            "flow_id": int(flow), "object_id": int(oid), "episode": int(oid) >> 20,
            "activity": req["app_activity"], "upload": upload, "bytes": down if down else up,
            "up_bytes": up, "down_bytes": down,
            "t_req": req["t"], "t_up_end": t_up_end, "t_first": first, "t_end": end,
            "complete": bool(complete), "gap_ms": gap,
        })  # fmt: skip
    df = pd.DataFrame(out)
    df["wait_ms"] = (df["t_first"].astype("float") - df["t_req"]) / 1e6
    df["think_ms"] = (df["t_first"].astype("float") - df["t_up_end"].astype("float")) / 1e6
    df["done_ms"] = (df["t_end"].astype("float") - df["t_req"]) / 1e6
    return df


def _episodes(obj: pd.DataFrame) -> pd.DataFrame:
    """Episode = objects asked for together: kind = the first object's activity."""
    g = obj.sort_values("t_req").groupby(["flow_id", "episode"], sort=False)
    ep = g.agg(kind=("activity", "first"), t0=("t_req", "min"), t1=("t_end", "max"),
               n=("object_id", "size"), bytes=("bytes", "sum"), complete=("complete", "all"))  # fmt: skip
    ep["ms"] = (ep["t1"].astype("float") - ep["t0"]) / 1e6
    return ep.reset_index()


def _active_mask(t: np.ndarray, obj: pd.DataFrame) -> np.ndarray:
    """True where some object was in flight (plus ACTIVE_TAIL_NS)."""
    if obj.empty or len(t) == 0:
        return np.zeros(len(t), dtype=bool)
    ends = obj["t_end"].astype("float").fillna(obj["t_req"].max() + 1e9)
    iv = sorted(zip(obj["t_req"].astype("float"), ends + ACTIVE_TAIL_NS))
    merged = []
    for a, b in iv:
        if merged and a <= merged[-1][1]:
            merged[-1][1] = max(merged[-1][1], b)
        else:
            merged.append([a, b])
    starts = np.array([a for a, _ in merged])
    stops = np.array([b for _, b in merged])
    i = np.searchsorted(starts, t, side="right") - 1
    return (i >= 0) & (t <= stops[np.clip(i, 0, None)])


def impact(td, obj: pd.DataFrame) -> list[dict]:
    """Realtime streams' excess delay and loss: apps active vs idle."""
    pk = td.packets
    out = []
    rt = realtime_received(pk)
    for (node, flow), g in rt.groupby(["node", "flow_id"]):
        direction = "uplink" if node == "server" else "downlink"
        send = pd.to_numeric(g["sender_utc_ns"], errors="coerce").astype("float")
        recv = pd.to_numeric(g["utc_ns"], errors="coerce").astype("float")
        owd = (recv - send).to_numpy()
        if len(owd) < 100:
            continue
        excess = (owd - np.nanmin(owd)) / 1e6
        place = (send if direction == "uplink" else recv).to_numpy()
        active = _active_mask(place, obj)
        # loss: sequence numbers the sender logged that never arrived
        sender_node = "mobile" if direction == "uplink" else "server"
        sent = pk[(pk["node"] == sender_node) & (pk["flow_id"] == flow) & (pk["direction"] == schema.DIR_SENT)]
        if "send_status" in sent:
            sent = sent[sent["send_status"].isna() | (sent["send_status"] == "OK")]  # failed sends never left
        got = set(pd.to_numeric(g["sequence_number"]).astype("int64"))
        s_t = pd.to_numeric(sent["utc_ns"], errors="coerce").astype("float").to_numpy()
        s_seq = pd.to_numeric(sent["sequence_number"]).astype("int64").to_numpy()
        # the server's send clock is placed on the mobile clock via the delay minimum
        if sender_node == "server":
            s_t = s_t + np.nanmin(owd)
        # packets sent after the last one that arrived (the test ending on the
        # receiver first) never had a chance: not loss
        keep = s_t <= np.nanmax(send.to_numpy() if direction == "uplink" else send.to_numpy() + np.nanmin(owd))
        s_t, s_seq = s_t[keep], s_seq[keep]
        s_active = _active_mask(s_t, obj)
        lost = ~np.isin(s_seq, list(got))
        row = {"flow_id": int(flow), "direction": direction}
        for name, sel, ssel in (("active", active, s_active), ("idle", ~active, ~s_active)):
            e = excess[sel]
            row[name] = {
                "share": round(float(sel.mean()), 3),
                "excess_ms": None if len(e) == 0 else {
                    "p50": round(float(np.percentile(e, 50)), 1),
                    "p99": round(float(np.percentile(e, 99)), 1)},
                "loss": None if ssel.sum() == 0 else round(float(lost[ssel].mean()), 4),
            }  # fmt: skip
        out.append(row)
    return out


#: What a task is, by the kind of its first step (enums.yaml app_step_kinds).
TASK_ACTIVITIES = {
    "chat_turn": "chat turn",
    "visual_query": "camera session",
    "image_gen": "image",
    "video_query": "video question",
}
TASK_KINDS = {
    "llm_turn": "agent task",
    "page_html": "page view",
    "mail_sync": "mail sync",
    "video_start": "video session",
    "feed": "feed scroll",
    "script_action": "script action",
}


def _ms(a, b) -> pd.Series:
    """b − a in ms, NaN where either is missing."""
    return (pd.to_numeric(b, errors="coerce") - pd.to_numeric(a, errors="coerce")) / 1e6


def _app_events(td) -> pd.DataFrame | None:
    """The test's app_events with every canonical column (older logs lack
    the newer ones: read as null), or None."""
    ev = td.app_events
    if ev is None or ev.empty:
        return None
    return ev.reindex(columns=list(dict.fromkeys([*ev.columns, *schema.APP_EVENTS])))


def tasks(td) -> list[dict] | None:
    """Per kind of task: how many started and finished, and for the finished
    ones the task time (planned → last byte) and steps. A task finished when
    every object was ``ok`` and the agent went on to another task (the last
    task of each agent was still running when the test ended). Open loop
    (objects carry a due time): ``started`` counts every task that arrived,
    ``dropped`` those that arrived with the backlog full and never ran."""
    ev = _app_events(td)
    if ev is None:
        return None
    ev["ok"] = ev["outcome"] == "ok"
    key = ["flow_id", "agent", "task"]
    g = ev.groupby(key, sort=True)
    t = pd.DataFrame({
        "first_kind": g.apply(lambda x: x.sort_values("step")["step_kind"].iloc[0], include_groups=False),
        "first_activity": g.apply(lambda x: x.sort_values("step")["activity"].iloc[0], include_groups=False),
        "steps": g["step"].max(),
        "objects": g.size(),
        "all_ok": g["ok"].all(),
        "t0": g["planned_mono_ns"].min(),
        "t1": g["done_mono_ns"].max(),
    }).reset_index()
    last = t.groupby(["flow_id", "agent"])["task"].transform("max")
    t["finished"] = t["all_ok"] & (t["task"] < last)
    # an LLM turn's task is named by what it is for (agent, chat, ...)
    by_activity = t["first_activity"].map(TASK_ACTIVITIES)
    t["kind"] = by_activity.fillna(t["first_kind"].map(TASK_KINDS)).fillna(t["first_kind"])
    # open loop: a task that arrived with the backlog full never ran
    never = g.apply(lambda x: bool((x["outcome"] == "skipped").all() and x["start_mono_ns"].isna().all()), include_groups=False)
    t["dropped"] = never.values & bool(ev["due_mono_ns"].notna().any())
    t["ms"] = _ms(t["t0"], t["t1"])
    # task numbers are per agent: join the objects to their task's kind
    ev = ev.merge(t[key + ["kind"]], on=key, how="left")
    loop = "open" if ev["due_mono_ns"].notna().any() else "closed"
    out = []
    for kind, k in t.groupby("kind"):
        f = k[k["finished"]]
        out.append({
            "kind": kind, "loop": loop, "started": len(k), "finished": len(f), "dropped": int(k["dropped"].sum()),
            "ms": stats.pcts(f["ms"], (50, 90), with_max=True, nd=0),
            "steps": stats.pcts(f["steps"], (50, 90), nd=0),
            "objects_not_ok": round(float((~ev[ev["kind"] == kind]["ok"]).mean()), 3),
        })  # fmt: skip
    return sorted(out, key=lambda x: -x["started"])


def intent(td) -> list[dict] | None:
    """Per activity and engine, planned against observed: how long objects waited for a
    connection (ready → start; closed loop: ready is when the burst was
    planned, in a task graph when the last object it waited for ended), how late they started (due →
    start; open loop), the server's planned think time against the observed
    wait for the first byte (the rest is network and upload), and for paced
    streams their planned length against the observed one."""
    ev = _app_events(td)
    if ev is None:
        return None
    # when each object could start: planned, or in a task graph when the last
    # object it waited for ended
    done = dict(zip(ev["object_id"], pd.to_numeric(ev["done_mono_ns"], errors="coerce")))

    def ready(r):
        deps = r["after_objects"]
        # null (None, NaN or pd.NA, by how the column was read): no graph
        if not isinstance(deps, (list, tuple, np.ndarray)) or len(deps) == 0:
            return r["planned_mono_ns"]
        return max(done.get(d, np.nan) for d in deps)

    ev = ev.assign(ready_mono_ns=ev.apply(ready, axis=1), engine=ev["engine"].fillna("exchange"))
    out = []
    for (act, engine), a in ev.groupby(["activity", "engine"]):
        started = a[a["start_mono_ns"].notna()]
        ok = a[a["outcome"] == "ok"]
        first = _ms(ok["start_mono_ns"], ok["first_byte_mono_ns"])
        think = pd.to_numeric(ok["plan_delay_ms"], errors="coerce")
        paced = ok[(pd.to_numeric(ok["plan_rate_bps"]) > 0) & (pd.to_numeric(ok["plan_down_bytes"]) > 0)]
        plan_stream = pd.to_numeric(paced["plan_down_bytes"]) * 8 / pd.to_numeric(paced["plan_rate_bps"]) * 1e3
        obs_stream = _ms(paced["first_byte_mono_ns"], paced["done_mono_ns"])
        out.append({
            "activity": act, "engine": engine, "objects": len(a),
            "outcomes": {k: int(v) for k, v in a["outcome"].value_counts().items()},
            "conn_wait_ms": stats.pcts(_ms(started["ready_mono_ns"], started["start_mono_ns"]), (50, 90), nd=0)
            if started["due_mono_ns"].isna().all() else None,
            "late_ms": stats.pcts(_ms(started["due_mono_ns"], started["start_mono_ns"]), (50, 90), nd=0),
            "think_plan_ms": stats.pcts(think, (50, 90), nd=0),
            "first_byte_ms": stats.pcts(first, (50, 90), nd=0),
            "beyond_think_ms": stats.pcts(first - think, (50, 90), nd=0),
            "stream_plan_ms": stats.pcts(plan_stream, (50,), nd=0),
            "stream_ms": stats.pcts(obs_stream, (50,), nd=0),
            "mbytes_plan": round(float(pd.to_numeric(a["plan_down_bytes"]).sum()) / 1e6, 2),
            "mbytes_moved": round(float(pd.to_numeric(a["bytes_down"]).sum()) / 1e6, 2),
            "new_conn_share": None if started.empty else round(float(started["new_connection"].fillna(False).astype(bool).mean()), 3),
        })  # fmt: skip
    return sorted(out, key=lambda x: -x["objects"])


def bursts(td) -> list[dict] | None:
    """UDP bursts (engine udp_burst) per activity and direction: datagrams
    that arrived of those planned, the received datagrams' extra one-way
    delay (above the burst stream's minimum, so the clock offset cancels) and
    delay variation between consecutive datagrams of a burst (IPDV), and the
    DSCP: what the sender marked and the share that arrived with it."""
    ev = _app_events(td)
    pk = td.packets
    if ev is None or pk is None or pk.empty or "app_packet_index" not in pk:
        return None
    ev = ev[ev["engine"] == "udp_burst"]
    if ev.empty:
        return None
    pk = pk[(pk["protocol"] == schema.PROTO_UDP) & pk["app_packet_index"].notna()]
    out = []
    for (act, direction), sender, receiver in (
        (key, *nodes)
        for key, nodes in {
            (a, d): (("server", "mobile") if d == "downlink" else ("mobile", "server"))
            for a in sorted(ev["activity"].unique())
            for d in ("downlink", "uplink")
        }.items()
    ):
        objs = ev[(ev["activity"] == act) & ((ev["plan_down_bytes"] > 0) == (direction == "downlink"))]
        if objs.empty:
            continue
        rows = pk[pk["sequence_number"].isin(objs["object_id"])]
        rx = rows[(rows["node"] == receiver) & (rows["direction"] == schema.DIR_RECEIVED)]
        tx = rows[(rows["node"] == sender) & (rows["direction"] == schema.DIR_SENT)]
        rec = {
            "activity": act, "direction": direction, "bursts": len(objs),
            "planned": int(pd.to_numeric(objs["plan_packets"]).sum()),
            "arrived": int(pd.to_numeric(objs["packets"]).fillna(0).sum()),
            "extra_delay_ms": None, "ipdv_ms": None, "dscp_sent": None, "dscp_kept": None,
        }  # fmt: skip
        rec["loss"] = round(1 - rec["arrived"] / rec["planned"], 4) if rec["planned"] else None
        if not rx.empty and rx["sender_utc_ns"].notna().any():
            r = rx.assign(
                owd=pd.to_numeric(rx["utc_ns"], errors="coerce") - pd.to_numeric(rx["sender_utc_ns"], errors="coerce"),
                sent=pd.to_numeric(rx["sender_utc_ns"], errors="coerce"),
                at=pd.to_numeric(rx["utc_ns"], errors="coerce"),
            ).sort_values(["sequence_number", "app_packet_index"])
            rec["extra_delay_ms"] = stats.pcts((r["owd"] - r["owd"].min()) / 1e6, (50, 99))
            g = r.groupby("sequence_number")
            ipdv = ((g["at"].diff() - g["sent"].diff()).abs() / 1e6).dropna()
            rec["ipdv_ms"] = stats.pcts(ipdv, (50, 99), nd=2)
        marks = pd.to_numeric(tx.get("dscp_sent"), errors="coerce").dropna() if not tx.empty else pd.Series(dtype=float)
        # what arrived, marked or not (unmarked traffic is the baseline)
        got = pd.to_numeric(rx.get("dscp_recv"), errors="coerce").dropna() if not rx.empty else pd.Series(dtype=float)
        rec["dscp_recv"] = {int(k): int(v) for k, v in got.value_counts().items()}
        if not marks.empty:
            mark = int(marks.mode().iloc[0])
            rec["dscp_sent"] = mark
            rec["dscp_kept"] = round(float((got == mark).mean()), 3) if not got.empty else None
        out.append(rec)
    return out or None


def _burst_intervals(td) -> pd.DataFrame:
    """When each UDP burst was in flight (app_events, UTC): the app was busy
    then too, though no TCP object was."""
    ev = _app_events(td)
    if ev is None:
        return pd.DataFrame(columns=["t_req", "t_end"])
    b = ev[(ev["engine"] == "udp_burst") & ev["start_utc_ns"].notna()]
    return pd.DataFrame({"t_req": pd.to_numeric(b["start_utc_ns"]), "t_end": pd.to_numeric(b["done_utc_ns"])})


def analyze(td) -> dict | None:
    """Summary for one test, or None if it ran no app_traffic."""
    obj = objects(td)
    burst, burst_stats = _burst_intervals(td), bursts(td)
    if obj.empty and burst.empty and burst_stats is None:
        return None
    parts = [x for x in (obj[["t_req", "t_end"]] if not obj.empty else None, burst) if x is not None and not x.empty]
    busy = pd.concat(parts) if parts else pd.DataFrame(columns=["t_req", "t_end"])
    if obj.empty:
        return {
            "objects": 0, "mbytes_down": 0.0, "mbytes_up": 0.0, "span_s": None, "activities": [], "episodes": [],
            "impact": impact(td, busy), "tasks": tasks(td), "intent": intent(td), "bursts": burst_stats,
            "params_version": (td.header or {}).get("app_params_version"),
        }  # fmt: skip
    ep = _episodes(obj)
    acts = []
    for act, g in obj.groupby("activity"):
        done = g[g["complete"]]
        acts.append({
            "activity": act, "objects": len(g), "complete_share": round(float(g["complete"].mean()), 3),
            "mbytes": round(float(g["down_bytes"].sum()) / 1e6, 2),
            "mbytes_up": round(float(g["up_bytes"].sum()) / 1e6, 2),
            "wait_ms": stats.pcts(done["wait_ms"], (50, 90), with_max=True), "done_ms": stats.pcts(done["done_ms"], (50, 90), with_max=True),
            "think_ms": stats.pcts(done["think_ms"], (50, 90), with_max=True) if (done["up_bytes"] > 0).any() else None,
            "gap_ms": stats.pcts(done["gap_ms"], (50, 90), with_max=True),
            "stalls_1s": int((g["gap_ms"] > 1000).sum()),
        })  # fmt: skip
    episodes = []
    for kind, g in ep.groupby("kind"):
        done = g[g["complete"]]
        episodes.append({"kind": kind, "n": len(g), "complete_share": round(float(g["complete"].mean()), 3),
                         "ms": stats.pcts(done["ms"], (50, 90), with_max=True)})  # fmt: skip
    span = (obj["t_end"].astype("float").max() - obj["t_req"].min()) / 1e9
    return {
        "objects": len(obj),
        "mbytes_down": round(float(obj["down_bytes"].sum()) / 1e6, 2),
        "mbytes_up": round(float(obj["up_bytes"].sum()) / 1e6, 2),
        "span_s": round(float(span), 1) if np.isfinite(span) else None,
        "activities": sorted(acts, key=lambda a: -a["objects"]),
        "episodes": episodes,
        "impact": impact(td, busy),
        "tasks": tasks(td),
        "intent": intent(td),
        "bursts": burst_stats,
        # the model parameter set the phone ran with (contracts/app_traffic/parameters.yaml)
        "params_version": (td.header or {}).get("app_params_version"),
    }

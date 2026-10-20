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

**Impact** on the realtime streams of the same test: their excess one-way
delay (delay minus the stream's minimum, so the clock offset cancels) and loss
while any app object was in flight versus while the apps were idle. Packets
are placed on the mobile clock: by send time for uplink, by arrival for
downlink.
"""

from __future__ import annotations

import numpy as np
import pandas as pd

from . import schema

REQUEST_BYTES = 24  # v1 request and acknowledgement
#: An app burst affects packets around it too; treat this much after the last
#: byte as still "active" (queues drain).
ACTIVE_TAIL_NS = 200_000_000


def _pcts(v: pd.Series) -> dict | None:
    v = pd.to_numeric(v, errors="coerce").dropna()
    if v.empty:
        return None
    return {"p50": round(float(v.quantile(0.5)), 1), "p90": round(float(v.quantile(0.9)), 1),
            "max": round(float(v.max()), 1)}  # fmt: skip


def objects(td) -> pd.DataFrame:
    """One row per app object seen by the mobile node."""
    pk = td.packets
    if pk is None or pk.empty or "app_activity" not in pk:
        return pd.DataFrame()
    m = pk[(pk["node"] == "mobile") & pk["app_activity"].notna()].copy()
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
    rt = pk[pk["sender_utc_ns"].notna() & (pk["direction"] == schema.DIR_RECEIVED)]
    if "app_activity" in pk:
        rt = rt[rt["app_activity"].isna()]
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


def analyze(td) -> dict | None:
    """Summary for one test, or None if it ran no app_traffic."""
    obj = objects(td)
    if obj.empty:
        return None
    ep = _episodes(obj)
    acts = []
    for act, g in obj.groupby("activity"):
        done = g[g["complete"]]
        acts.append({
            "activity": act, "objects": len(g), "complete_share": round(float(g["complete"].mean()), 3),
            "mbytes": round(float(g["down_bytes"].sum()) / 1e6, 2),
            "mbytes_up": round(float(g["up_bytes"].sum()) / 1e6, 2),
            "wait_ms": _pcts(done["wait_ms"]), "done_ms": _pcts(done["done_ms"]),
            "think_ms": _pcts(done["think_ms"]) if (done["up_bytes"] > 0).any() else None,
            "gap_ms": _pcts(done["gap_ms"]),
            "stalls_1s": int((g["gap_ms"] > 1000).sum()),
        })  # fmt: skip
    episodes = []
    for kind, g in ep.groupby("kind"):
        done = g[g["complete"]]
        episodes.append({"kind": kind, "n": len(g), "complete_share": round(float(g["complete"].mean()), 3),
                         "ms": _pcts(done["ms"])})  # fmt: skip
    span = (obj["t_end"].astype("float").max() - obj["t_req"].min()) / 1e9
    return {
        "objects": len(obj),
        "mbytes_down": round(float(obj["down_bytes"].sum()) / 1e6, 2),
        "mbytes_up": round(float(obj["up_bytes"].sum()) / 1e6, 2),
        "span_s": round(float(span), 1) if np.isfinite(span) else None,
        "activities": sorted(acts, key=lambda a: -a["objects"]),
        "episodes": episodes,
        "impact": impact(td, obj),
    }

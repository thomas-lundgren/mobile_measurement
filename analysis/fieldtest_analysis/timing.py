"""Per-test delivery-timing summary for the findings page.

For each fast realtime UDP stream (≥ ``MIN_PPS``; 1 ms streams and up) it
combines ``delivery`` (batching, hold time, excess delay, host wake-up,
send outcomes, modem queue limit) and ``spectral`` (periodicities the path
adds). ``summarize`` returns the compact per-stream summary that goes into
the day files, plus the spectra for ``viz/timing/<test_id>.json``.
``day_summary`` condenses a day's streams for the page's callout.
"""

from __future__ import annotations

from collections import defaultdict

import numpy as np
import pandas as pd

from . import bench, delivery, spectral
from .loader import TestData

#: Streams slower than this (packets/s) are not timing streams.
MIN_PPS = 500
#: Spectrum plot resolution (Hz) for the page.
PLOT_STEP_HZ = 2.0
#: Heatmaps (day spectrogram, per-stream waterfall): resolution and range.
HEAT_STEP_HZ = 5.0
HEAT_DB = (-10.0, 40.0)
#: Frequency of the first heatmap column (the arrival grid starts here).
HEAT_F0_HZ = spectral.ARRIVAL_F_STEP * 4
#: Waterfall window and step (s).
WATERFALL_WIN_S = 2.0
WATERFALL_STEP_S = 1.0
#: Buffer-fill timeline bin (s).
BUFFER_BIN_S = 0.05


def _sig(a, digits: int = 3) -> list:
    a = np.asarray(a, dtype=float)
    return [None if not np.isfinite(v) else float(f"{v:.{digits}g}") for v in a]


def _pool(freqs: np.ndarray, power: np.ndarray, step: float) -> tuple[np.ndarray, np.ndarray]:
    """Max-pool a spectrum onto a coarser grid (keeps narrow lines visible)."""
    k = max(1, int(round(step / (freqs[1] - freqs[0]))))
    n = len(power) // k * k
    return freqs[:n:k], power[:n].reshape(-1, k).max(axis=1)


def _db_codes(power: np.ndarray, ref: float) -> list[int]:
    """Power relative to `ref` in dB, clipped to HEAT_DB, as 0–250 codes
    (0.2 dB steps) — compact for heatmaps."""
    lo, hi = HEAT_DB
    db = 10 * np.log10(np.maximum(np.asarray(power, dtype=float), 1e-12) / max(ref, 1e-12))
    return [int(v) for v in np.round((np.clip(db, lo, hi) - lo) * 5)]


def _heat_row(freqs: np.ndarray, rx: np.ndarray, tx: np.ndarray | None) -> dict:
    """One heatmap row: received and sent spectra pooled to HEAT_STEP_HZ, in dB
    over the received spectrum's median (its noise floor)."""
    f, prx = _pool(freqs, rx, HEAT_STEP_HZ)
    ref = float(np.median(rx))
    row = {"rx": _db_codes(prx, ref)}
    if tx is not None:
        row["tx"] = _db_codes(_pool(freqs, tx, HEAT_STEP_HZ)[1], ref)
    return row


def waterfall(times_s: np.ndarray) -> dict | None:
    """Short-time arrival spectra (WATERFALL_WIN_S windows every
    WATERFALL_STEP_S): when a rhythm switches on or off within a test."""
    t = np.sort(np.asarray(times_s, dtype=float))
    if len(t) < 200 or t[-1] - t[0] < WATERFALL_WIN_S:
        return None
    freqs = np.arange(spectral.ARRIVAL_F_STEP * 4, spectral.ARRIVAL_F_MAX + spectral.ARRIVAL_F_STEP, spectral.ARRIVAL_F_STEP)
    rows, starts = [], []
    s0 = t[0]
    while s0 + WATERFALL_WIN_S <= t[-1] + 1e-9:
        w = t[(t >= s0) & (t < s0 + WATERFALL_WIN_S)]
        if len(w) >= 50:
            p = spectral.point_spectrum(w, freqs)
            rows.append(_heat_row(freqs, p, None)["rx"])
            starts.append(round(s0 - t[0], 2))
        s0 += WATERFALL_STEP_S
    return {"t": starts, "win_s": WATERFALL_WIN_S, "rows": rows} if rows else None


def buffer_timeline(sent: pd.DataFrame, recv: pd.DataFrame, qdisc: pd.DataFrame | None = None) -> dict | None:
    """Uplink buffer fill over the test, on BUFFER_BIN_S bins: packets in the
    modem (sent OK, not yet delivered, not yet given up), the phone's OS queue
    below the socket, time blocked in send(), and sent vs delivered rate.
    With bench data (phone on USB): the modem link's qdisc backlog and the
    requeues — the modem driver refusing packets, i.e. flow control."""
    if "send_status" in sent:
        ok = sent[sent["send_status"].isna() | (sent["send_status"] == "OK")]
    else:
        ok = sent
    ts = ok.set_index(ok["sequence_number"].astype("int64"))["utc_ns"].astype("int64")
    kernel = pd.to_numeric(recv["kernel_rx_utc_ns"], errors="coerce") if "kernel_rx_utc_ns" in recv else None
    rxt = (kernel.fillna(pd.to_numeric(recv["utc_ns"])) if kernel is not None else pd.to_numeric(recv["utc_ns"])).astype("int64")
    tr = pd.Series(rxt.to_numpy(), index=recv["sequence_number"].astype("int64").to_numpy())
    tr = tr[~tr.index.duplicated()]
    owd = (tr - ts.reindex(tr.index)).dropna()
    if len(owd) < 50:
        return None
    base = np.percentile(owd, 0.5)
    leave = (tr - base).reindex(ts.index)  # when each packet left the phone's queues
    lost = leave.isna()
    max_wait = float((leave - ts).max())
    t0, t1 = int(ts.min()), int(ts.max())
    bin_ns = int(BUFFER_BIN_S * 1e9)
    edges = np.arange(t0, t1 + bin_ns, bin_ns)
    sent_c = np.searchsorted(np.sort(ts.to_numpy()), edges[1:], side="right")
    done_c = np.searchsorted(np.sort(leave.dropna().to_numpy()), edges[1:], side="right")
    gone_c = np.searchsorted(np.sort((ts[lost] + max_wait).to_numpy()), edges[1:], side="right")
    modem = np.maximum(sent_c - done_c - gone_c, 0)
    size = float(pd.to_numeric(sent["payload_size"], errors="coerce").median())
    idx_sent = np.clip((ts.to_numpy() - t0) // bin_ns, 0, len(edges) - 2)
    idx_done = np.clip((leave.dropna().to_numpy() - t0) // bin_ns, 0, len(edges) - 2).astype(np.int64)
    n = len(edges) - 1
    sent_mbps = np.bincount(idx_sent, minlength=n) * size * 8 / BUFFER_BIN_S / 1e6
    done_mbps = np.bincount(idx_done, minlength=n) * size * 8 / BUFFER_BIN_S / 1e6
    all_t = pd.to_numeric(sent["utc_ns"], errors="coerce").astype("int64").to_numpy()
    idx_all = np.clip((all_t - t0) // bin_ns, 0, n - 1)
    outq = pd.to_numeric(sent["sock_outq_bytes"], errors="coerce").to_numpy() if "sock_outq_bytes" in sent else None
    call = pd.to_numeric(sent["send_call_ns"], errors="coerce").to_numpy() if "send_call_ns" in sent else None
    out = {
        "bin_s": BUFFER_BIN_S,
        "t": [round(i * BUFFER_BIN_S, 2) for i in range(n)],
        "modem_packets": [int(v) for v in modem],
        "sent_mbps": [round(float(v), 2) for v in sent_mbps],
        "delivered_mbps": [round(float(v), 2) for v in done_mbps],
    }
    if outq is not None and np.isfinite(outq).any():
        q = pd.Series(outq).groupby(idx_all).max().reindex(range(n))
        out["os_queue_kb"] = [None if pd.isna(v) else round(float(v) / 1e3, 1) for v in q]
    if call is not None and np.isfinite(call).any():
        b = pd.Series(np.where(call > 500_000, call, 0.0)).groupby(idx_all).sum().reindex(range(n), fill_value=0)
        out["blocked_share"] = [round(min(1.0, float(v) / 1e9 / BUFFER_BIN_S), 2) for v in b]
    if qdisc is not None and not qdisc.empty:
        # the modem link carrying the test: the device that sent the most packets
        sent_by_dev = qdisc.groupby("dev")["sent_pkts"].agg(lambda x: x.max() - x.min())
        dev = sent_by_dev.idxmax()
        q = qdisc[qdisc["dev"] == dev].sort_values("utc_ns")
        qi = np.clip((q["utc_ns"].astype("int64").to_numpy() - t0) // bin_ns, -1, n)
        inside = (qi >= 0) & (qi < n)
        if inside.any():
            backlog = pd.Series(q["backlog_bytes"].to_numpy()[inside] / 1e3).groupby(qi[inside]).max()
            out["qdisc_dev"] = dev
            out["qdisc_backlog_kb"] = [None if pd.isna(v) else round(float(v), 1) for v in backlog.reindex(range(n))]
            req = q["requeues"].astype("int64")
            delta = req.diff().clip(lower=0).fillna(0).to_numpy()[inside]
            out["requeues"] = [int(v) for v in pd.Series(delta).groupby(qi[inside]).sum().reindex(range(n), fill_value=0)]
    return out


def _load(sent: pd.DataFrame, recv: pd.DataFrame, e: dict, planned: dict | None = None) -> dict:
    """Planned / offered / sent / delivered rate and the queue's size, for
    every stream (saturation plots): wait = excess delay, held = OK send rate
    × p90 wait. ``planned_mbps`` is the plan's rate × packet size: under back
    pressure send() blocks, so the attempts (``offered_mbps``) fall below it
    and understate the load."""
    mono = pd.to_numeric(sent["mono_ns"], errors="coerce")
    span = float(mono.max() - mono.min()) / 1e9
    size = float(pd.to_numeric(sent["payload_size"], errors="coerce").median())
    status = sent["send_status"] if "send_status" in sent else pd.Series(dtype=object)
    n_ok = int((status.isna() | (status == "OK")).sum()) if len(status) else len(sent)
    ex = e["delivery"].get("excess_delay_ms") or {}
    p90 = ex.get("p90")
    held = n_ok / span * p90 / 1e3 if span > 0 and p90 is not None else None
    snd = e.get("sender") or {}
    q = e.get("queue_limit") or {}
    planned_mbps = None
    if planned and planned.get("rate_pps") and planned.get("packet_size_bytes"):
        planned_mbps = round(planned["rate_pps"] * planned["packet_size_bytes"] * 8 / 1e6, 2)
    return {
        "planned_mbps": planned_mbps,
        "offered_mbps": round(len(sent) * size * 8 / span / 1e6, 2) if span > 0 else None,
        "sent_mbps": round(n_ok * size * 8 / span / 1e6, 2) if span > 0 else None,
        "delivered_mbps": round(len(recv) * size * 8 / span / 1e6, 2) if span > 0 else None,
        "wait_p50_ms": ex.get("p50"),
        "wait_p90_ms": p90,
        "held_packets": None if held is None else round(held),
        "held_kb": None if held is None else round(held * size / 1e3, 1),
        "backpressure_share": q.get("backpressure_share", 0.0),
        "blocked_share": snd.get("blocked_share"),
        "os_queue_max_kb": None if not snd.get("outq_bytes") else round(snd["outq_bytes"]["max"] / 1e3, 1),
    }


def summarize(td: TestData, plan_flows: dict[int, dict] | None = None, bench_dir=None) -> tuple[list[dict], dict]:
    pk = td.packets
    if pk.empty:
        return [], {}
    streams, detail = [], []
    spectra = {(e["flow_id"], e["direction"]): e for e in spectral.analyze(td)}
    # Bench data (USB collector) over the test, on the phone's clock.
    mob = pd.to_numeric(pk.loc[pk["node"] == "mobile", "utc_ns"], errors="coerce").dropna()
    qdisc = radiolog = None
    if bench_dir is not None and len(mob):
        qdisc, radiolog = bench.load_window(bench_dir, int(mob.min()) - 5_000_000_000, int(mob.max()) + 5_000_000_000)
    for e in delivery.analyze(td):
        sender_node = "mobile" if e["direction"] == "uplink" else "server"
        sent = pk[(pk["node"] == sender_node) & (pk["flow_id"] == e["flow_id"]) & (pk["direction"] == "SENT")]
        if len(sent) < 100:
            continue
        mono = pd.to_numeric(sent["mono_ns"], errors="coerce")
        span = (mono.max() - mono.min()) / 1e9
        pps = len(sent) / span if span > 0 else 0
        if pps < MIN_PPS:
            continue
        size = int(pd.to_numeric(sent["payload_size"], errors="coerce").median())
        d = e["delivery"]
        sp = spectra.get((e["flow_id"], e["direction"]))
        s = {
            "flow_id": e["flow_id"],
            "direction": e["direction"],
            "pps": round(pps),
            "size": size,
            "mbps": round(pps * size * 8 / 1e6, 1),
            "sender": {k: e.get("sender", {}).get(k) for k in
                       ("status", "local_drop_share", "pacing_us", "send_call_us", "outq_bytes", "blocked_share")},
            "network_loss_share": e.get("network_loss_share"),
            "queue_limit": e.get("queue_limit"),
            "delivery": {k: d.get(k) for k in
                         ("clock", "batches", "batch_interval_us", "grid_alignment", "hold_ms",
                          "excess_delay_ms", "excess_delay_peaks_ms", "host_wakeup_us")},
            "lines": sp["arrival_peaks"] if sp else [],
            "delay_peaks": sp["delay_peaks"] if sp else [],
        }
        recv = pk[(pk["node"] == ("server" if e["direction"] == "uplink" else "mobile"))
                  & (pk["flow_id"] == e["flow_id"]) & (pk["direction"] == "RECEIVED")]
        s["load"] = _load(sent, recv, e, (plan_flows or {}).get(e["flow_id"]))
        streams.append(s)
        if sp:
            a, dl = sp["_spectra"]["arrival"], sp["_spectra"]["delay"]
            f, rx = _pool(a["freqs"], a["rx"], PLOT_STEP_HZ)
            tx = _pool(a["freqs"], a["tx"], PLOT_STEP_HZ)[1] if a["tx"] is not None else None
            item = {"flow_id": e["flow_id"], "direction": e["direction"],
                    "arrival": {"f": _sig(f, 5), "rx": _sig(rx), "tx": _sig(tx) if tx is not None else None}}
            if dl:
                item["delay"] = {"f": _sig(dl["freqs"][1:], 4), "psd": _sig(dl["psd"][1:])}
            item["heat"] = _heat_row(a["freqs"], a["rx"], a["tx"])
            k = pd.to_numeric(recv["kernel_rx_utc_ns"], errors="coerce")
            rx_t = (k if k.notna().mean() > 0.9 else pd.to_numeric(recv["utc_ns"], errors="coerce")).dropna()
            item["waterfall"] = waterfall(rx_t.to_numpy(dtype=float) / 1e9)
            if e["direction"] == "uplink":
                item["buffer"] = buffer_timeline(sent, recv, qdisc)
            detail.append(item)
    out = {"streams": detail, "heat_step_hz": HEAT_STEP_HZ, "heat_f0_hz": HEAT_F0_HZ, "heat_db": list(HEAT_DB)}
    if radiolog is not None and not radiolog.empty and len(mob):
        t0 = int(mob.min())
        out["radio_events"] = [
            {"t": round((int(r.utc_ns) - t0) / 1e9, 2), "kind": r.kind, "summary": r.summary}
            for r in radiolog.sort_values("utc_ns").itertuples() if r.kind in ("channel_config", "lce", "data_call")
        ]
    return streams, (out if detail else {})


def day_summary(tests: list[dict]) -> dict:
    """Condense a day's timing streams: added spectral lines by fundamental,
    and the modem-queue picture (packets held under back pressure; delay
    ceilings of overloaded streams without it)."""
    lines = defaultdict(lambda: {"uplink": 0, "downlink": 0})
    n = {"uplink": 0, "downlink": 0}
    held, ceilings = [], []
    for t in tests:
        for s in t.get("timing") or []:
            n[s["direction"]] += 1
            seen = {p["fundamental_hz"] for p in s["lines"] if p.get("added_by_path")}
            for hz in seen:
                lines[hz][s["direction"]] += 1
            q = s.get("queue_limit")
            if not q:
                continue
            if q.get("backpressure_share", 0) >= delivery.BACKPRESSURE_SHARE:
                held.append(q["held_packets"])
            elif (s["delivery"].get("excess_delay_ms") or {}).get("p99") is not None:
                ceilings.append(s["delivery"]["excess_delay_ms"]["p99"])
    return {
        "streams": n,
        "added_lines": sorted(
            ({"hz": hz, **c} for hz, c in lines.items() if c["uplink"] + c["downlink"] >= 2),
            key=lambda x: -(x["uplink"] + x["downlink"]),
        )[:8],
        "backpressure": {"streams": len(held), "held_packets_median": float(np.median(held)) if held else None,
                         "held_packets_range": [min(held), max(held)] if held else None},
        "overload_delay_ceilings_ms": sorted(round(c) for c in ceilings),
    }


def day_spectrogram(day_tests: list[dict], detail_dir) -> dict:
    """Rows for the day's spectrogram heatmaps: one per timing stream, in
    time order, with the stream's received/sent heat rows from its detail
    file."""
    import json
    from pathlib import Path

    rows = []
    for t in day_tests:
        if not t.get("timing"):
            continue
        p = Path(detail_dir) / f"{t['test_id']}.json"
        if not p.exists():
            continue
        det = {(d["flow_id"], d["direction"]): d for d in json.loads(p.read_text()).get("streams", [])}
        for s in t["timing"]:
            d = det.get((s["flow_id"], s["direction"]))
            if not d or "heat" not in d:
                continue
            rows.append({
                "test_id": t["test_id"], "plan": t.get("plan"), "start": t["start"], "radio": t.get("radio"),
                "flow_id": s["flow_id"], "direction": s["direction"], "pps": s["pps"], "size": s["size"],
                **d["heat"],
            })
    return {"f0_hz": HEAT_F0_HZ, "f_step_hz": HEAT_STEP_HZ, "db": list(HEAT_DB), "rows": rows}

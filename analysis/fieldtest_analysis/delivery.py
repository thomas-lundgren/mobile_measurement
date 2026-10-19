"""Delivery timing and back pressure of realtime UDP streams.

Built for 1 ms streams (1000 pps), where the radio's scheduling shows through:

- **Sender** (SENT rows, one per attempt): pacing accuracy, send outcomes
  (``OK`` / ``EAGAIN`` = socket buffer full, back pressure / ``ENOBUFS`` =
  qdisc drop / ``ERROR``), syscall time and the bytes queued below the socket
  (``sock_outq_bytes``). Sequence numbers that never left the host are local
  drops; ``OK`` sequence numbers the receiver never saw are network loss.
- **Delivery** at the receiver, on the kernel receive time
  (``kernel_rx_utc_ns``; app time if absent). Packets sent 1 ms apart that
  arrive within ``BATCH_GAP_US`` of each other were delivered together: one
  uplink grant (seen at the server) or one modem-to-host transfer (seen on
  the phone). Reported: batch sizes, the spacing between batches, and how
  well that spacing sits on a slot grid (0.5 ms: NR 30 kHz, 1 ms: LTE / NR
  15 kHz) as a Rayleigh mean resultant length R — near 1 means aligned, near
  ``1/sqrt(n)`` means no grid.
- **Hold time**: within a batch, how long its first packet waited for the
  last one (send-time spread) — the grant / delivery period as felt by data.
- **Excess delay**: one-way delay above the stream's floor. Steps of ~8 ms
  repeated across packets suggest HARQ retransmissions (LTE FDD: 8 ms);
  larger, smooth spreads are queueing.
- **Host wake-up**: app receive time minus kernel receive time.

Clock offset cancels: the excess delay is relative to the floor of the
same stream, and every spacing is measured on one node's clock.

Caveat: the server is on Wi-Fi. Its own link batches packets too (A-MPDU),
which blurs uplink batches at the server and pre-batches the downlink
before it reaches the radio. Wire the server for clean grant timing.
"""

from __future__ import annotations

import argparse
import json
import sys

import numpy as np
import pandas as pd

from .loader import TestData, load_test

#: Arrivals closer than this were delivered together (one batch).
BATCH_GAP_US = 100.0
#: Slot grids tested for batch spacing, in µs.
GRIDS_US = (500.0, 1000.0)
#: Excess-delay histogram bin and range for step (HARQ-like) detection, ms.
EXCESS_BIN_MS = 1.0
EXCESS_MAX_MS = 40.0
#: A histogram peak must hold at least this share of packets to be listed.
PEAK_MIN_SHARE = 0.01
#: Loss above this marks a stream as overloaded for the queue-limit estimate…
OVERLOAD_LOSS = 0.05
#: …as does the OS queue below the socket being this full (back pressure:
#: the modem flow-controls the host instead of dropping) for this share of sends.
BACKPRESSURE_OUTQ_BYTES = 100_000
BACKPRESSURE_SHARE = 0.05


def _pct(s: pd.Series | np.ndarray, q: float) -> float | None:
    a = np.asarray(s, dtype=float)
    a = a[~np.isnan(a)]
    return float(np.percentile(a, q)) if len(a) else None


def _r(x: float | None, nd: int = 1) -> float | None:
    return None if x is None else round(x, nd)


def grid_alignment(intervals_us: np.ndarray, period_us: float) -> float | None:
    """Mean resultant length of interval phases on a `period_us` grid."""
    if len(intervals_us) < 10:
        return None
    phase = 2 * np.pi * (np.asarray(intervals_us) % period_us) / period_us
    return float(np.hypot(np.cos(phase).mean(), np.sin(phase).mean()))


def histogram_peaks(values: np.ndarray, bin_w: float, vmax: float) -> list[dict]:
    """Local maxima of a histogram that hold at least PEAK_MIN_SHARE."""
    v = np.asarray(values, dtype=float)
    v = v[(v >= 0) & (v < vmax)]
    if not len(v):
        return []
    counts, edges = np.histogram(v, bins=np.arange(0, vmax + bin_w, bin_w))
    share = counts / len(values)
    peaks = []
    for i, c in enumerate(counts):
        left = counts[i - 1] if i else -1
        right = counts[i + 1] if i + 1 < len(counts) else -1
        if c > left and c >= right and share[i] >= PEAK_MIN_SHARE:
            peaks.append({"at": round(float(edges[i] + bin_w / 2), 2), "share": round(float(share[i]), 3)})
    return peaks


def _sender_stats(sent: pd.DataFrame) -> dict:
    sent = sent.sort_values("sequence_number")
    out: dict = {"attempts": int(len(sent))}
    status = sent["send_status"] if "send_status" in sent else pd.Series(dtype=object)
    if status.notna().any():
        out["status"] = {str(k): int(v) for k, v in status.value_counts().items()}
        out["local_drop_share"] = round(float((status.notna() & (status != "OK")).mean()), 4)
    iv = np.diff(sent["mono_ns"].astype("int64").to_numpy()) / 1e3
    if len(iv):
        out["pacing_us"] = {"p1": _r(_pct(iv, 1)), "p50": _r(_pct(iv, 50)), "p99": _r(_pct(iv, 99))}
    if "send_call_ns" in sent and sent["send_call_ns"].notna().any():
        c = pd.to_numeric(sent["send_call_ns"], errors="coerce") / 1e3
        out["send_call_us"] = {"p50": _r(_pct(c, 50)), "p99": _r(_pct(c, 99)), "max": _r(float(c.max()))}
        span_s = (sent["mono_ns"].astype("int64").max() - sent["mono_ns"].astype("int64").min()) / 1e9
        # share of the test the sender spent blocked in send() (calls > 0.5 ms)
        out["blocked_share"] = round(float(c[c > 500].sum() / 1e6 / span_s), 3) if span_s > 0 else None
    if "sock_outq_bytes" in sent and sent["sock_outq_bytes"].notna().any():
        q = pd.to_numeric(sent["sock_outq_bytes"], errors="coerce")
        out["outq_bytes"] = {"p50": _pct(q, 50), "p99": _pct(q, 99), "max": float(q.max())}
    return out


def _delivery_stats(recv: pd.DataFrame) -> dict:
    """Batches, batch spacing, hold time, excess delay, wake-up latency."""
    kernel = pd.to_numeric(recv.get("kernel_rx_utc_ns"), errors="coerce") if "kernel_rx_utc_ns" in recv else None
    use_kernel = kernel is not None and kernel.notna().mean() > 0.9
    t_rx = (kernel if use_kernel else pd.to_numeric(recv["utc_ns"], errors="coerce")).astype("float64")
    df = pd.DataFrame(
        {"t": t_rx.to_numpy(), "sent": pd.to_numeric(recv["sender_utc_ns"], errors="coerce").astype("float64").to_numpy()}
    ).dropna().sort_values("t")
    out: dict = {"clock": "kernel" if use_kernel else "app", "received": int(len(df))}
    if len(df) < 20:
        return out

    gaps_us = np.diff(df["t"].to_numpy()) / 1e3
    new_batch = np.concatenate([[True], gaps_us >= BATCH_GAP_US])
    batch_id = np.cumsum(new_batch)
    sizes = np.bincount(batch_id)[1:]
    out["batches"] = {
        "count": int(len(sizes)),
        "mean_size": round(float(sizes.mean()), 2),
        "p90_size": _pct(sizes, 90),
        "max_size": int(sizes.max()),
        "share_batched": round(float(sizes[sizes > 1].sum() / sizes.sum()), 3),
    }
    starts = df["t"].to_numpy()[new_batch]
    between = np.diff(starts) / 1e3  # µs between batch starts
    if len(between):
        out["batch_interval_us"] = {"p10": _r(_pct(between, 10)), "p50": _r(_pct(between, 50)), "p90": _r(_pct(between, 90))}
        out["batch_interval_peaks_ms"] = histogram_peaks(between / 1e3, 0.25, 20.0)
        out["grid_alignment"] = {
            f"{int(p)}us": _r(grid_alignment(between, p), 2) for p in GRIDS_US
        }
        out["grid_alignment"]["random_level"] = _r(float(1 / np.sqrt(len(between))), 2)

    g = df.assign(batch=batch_id).groupby("batch")["sent"]
    spread_ms = ((g.max() - g.min()) / 1e6)[g.size() > 1]
    if len(spread_ms):
        out["hold_ms"] = {"p50": _r(_pct(spread_ms, 50), 2), "p90": _r(_pct(spread_ms, 90), 2)}

    owd = (df["t"] - df["sent"]).to_numpy() / 1e6  # ms, includes the clock offset
    excess = owd - np.percentile(owd, 0.5)
    out["excess_delay_ms"] = {"p50": _r(_pct(excess, 50), 2), "p90": _r(_pct(excess, 90), 2), "p99": _r(_pct(excess, 99), 2)}
    out["excess_delay_peaks_ms"] = histogram_peaks(excess, EXCESS_BIN_MS, EXCESS_MAX_MS)

    if use_kernel:
        wake = (pd.to_numeric(recv["utc_ns"], errors="coerce") - kernel).dropna() / 1e3
        out["host_wakeup_us"] = {"p50": _r(_pct(wake, 50)), "p99": _r(_pct(wake, 99)), "max": _r(float(wake.max()))}
    return out


def queue_limit(sent: pd.DataFrame, recv: pd.DataFrame) -> dict | None:
    """For an overloaded stream: where does the sender-side queue (in the
    uplink: the modem) stop growing? Over the second half of the test
    (steady state):

    - ``queue_*``: delivered backlog ≈ drain rate × median wait;
    - ``held_packets``: what the queue holds including packets it will
      drop, ≈ accepted send rate × p90 wait (the age packets reach).

    Across tests, a constant wait means a time limit (e.g. a discard
    timer); a constant byte backlog, a byte limit; a constant
    ``held_packets``, a packet-count limit. ``backpressure_share`` is the
    share of sends that found the OS queue full."""
    ts = sent.set_index("sequence_number")["utc_ns"].astype("int64")
    kernel = pd.to_numeric(recv["kernel_rx_utc_ns"], errors="coerce") if "kernel_rx_utc_ns" in recv else None
    rx = (kernel.fillna(pd.to_numeric(recv["utc_ns"])) if kernel is not None else pd.to_numeric(recv["utc_ns"])).astype("int64")
    tr = pd.Series(rx.to_numpy(), index=recv["sequence_number"].astype("int64").to_numpy())
    owd = (tr - ts.reindex(tr.index)).dropna()
    if len(owd) < 50:
        return None
    lo, hi = tr.index.min(), tr.index.max()
    mid = (lo + hi) / 2
    sent_half = ts[(ts.index > mid) & (ts.index <= hi)]
    got_half = owd[owd.index > mid]
    if len(sent_half) < 50:
        return None
    loss = 1 - len(got_half) / len(sent_half)
    outq = pd.to_numeric(sent["sock_outq_bytes"], errors="coerce") if "sock_outq_bytes" in sent else pd.Series(dtype=float)
    backpressure = float((outq > BACKPRESSURE_OUTQ_BYTES).mean()) if outq.notna().any() else 0.0
    if loss < OVERLOAD_LOSS and backpressure < BACKPRESSURE_SHARE:
        return None
    dur_s = float(sent_half.max() - sent_half.min()) / 1e9
    size = float(sent["payload_size"].median())
    drain_bps = len(got_half) * size * 8 / dur_s
    waits = (got_half - np.percentile(owd, 0.5)) / 1e9
    wait_s = float(waits.median())
    queue_bytes = drain_bps / 8 * wait_s
    return {
        "offered_mbps": round(len(sent_half) * size * 8 / dur_s / 1e6, 2),
        "drained_mbps": round(drain_bps / 1e6, 2),
        "loss_share": round(loss, 3),
        "wait_ms": round(wait_s * 1e3, 1),
        "queue_kb": round(queue_bytes / 1e3, 1),
        "queue_packets": round(queue_bytes / size),
        "held_packets": round(len(sent_half) / dur_s * float(waits.quantile(0.9))),
        "backpressure_share": round(backpressure, 3),
    }


def analyze(td: TestData) -> list[dict]:
    """One entry per realtime stream (a flow with embedded sender times)."""
    pk = td.packets
    if pk.empty:
        return []
    rx = pk[(pk["direction"] == "RECEIVED") & pk["sender_utc_ns"].notna()]
    out = []
    for (node, flow_id), recv in rx.groupby(["node", "flow_id"]):
        sender_node = "mobile" if node == "server" else "server"
        sent = pk[(pk["node"] == sender_node) & (pk["flow_id"] == flow_id) & (pk["direction"] == "SENT")]
        entry: dict = {
            "flow_id": int(flow_id),
            "direction": "uplink" if node == "server" else "downlink",
            "receiver": node,
        }
        if not sent.empty:
            entry["sender"] = _sender_stats(sent)
            ok = sent if "send_status" not in sent else sent[sent["send_status"].isna() | (sent["send_status"] == "OK")]
            got = set(recv["sequence_number"].astype("int64"))
            # Only while the receiver was listening: the sender starts before
            # and stops after it (downlink waits for the keepalive).
            lo, hi = min(got), max(got)
            sent_ok = {q for q in ok["sequence_number"].astype("int64") if lo <= q <= hi}
            entry["network_loss_share"] = round(len(sent_ok - got) / len(sent_ok), 4) if sent_ok else None
            entry["queue_limit"] = queue_limit(ok, recv)
        entry["delivery"] = _delivery_stats(recv)
        out.append(entry)
    return sorted(out, key=lambda e: e["flow_id"])


def _fmt(e: dict) -> str:
    lines = [f"flow {e['flow_id']} {e['direction']} (received at {e['receiver']})"]
    s = e.get("sender")
    if s:
        lines.append(f"  sender: {s['attempts']} attempts, status {s.get('status')}, pacing µs {s.get('pacing_us')}")
        lines.append(f"          send call µs {s.get('send_call_us')}, queued below socket (bytes) {s.get('outq_bytes')}")
        lines.append(f"  network loss: {e.get('network_loss_share')}")
        if e.get("queue_limit"):
            lines.append(f"  overloaded, steady state: {e['queue_limit']}")
    d = e["delivery"]
    lines.append(f"  delivery ({d['clock']} clock): {d['received']} packets, batches {d.get('batches')}")
    lines.append(f"          batch spacing µs {d.get('batch_interval_us')}, peaks (ms) {d.get('batch_interval_peaks_ms')}")
    lines.append(f"          grid alignment R {d.get('grid_alignment')}, hold ms {d.get('hold_ms')}")
    lines.append(f"          excess delay ms {d.get('excess_delay_ms')}, peaks (ms) {d.get('excess_delay_peaks_ms')}")
    if "host_wakeup_us" in d:
        lines.append(f"          host wake-up µs {d['host_wakeup_us']}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("test_dirs", nargs="+")
    ap.add_argument("--json", action="store_true", help="print JSON instead of text")
    args = ap.parse_args(argv)
    result = {}
    for d in args.test_dirs:
        td = load_test(d)
        result[td.test_id or d] = analyze(td)
    if args.json:
        json.dump(result, sys.stdout, indent=1)
        print()
    else:
        for tid, streams in result.items():
            print(f"== {tid}")
            for e in streams:
                print(_fmt(e))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

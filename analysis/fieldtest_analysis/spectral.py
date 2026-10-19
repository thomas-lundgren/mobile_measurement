"""Frequency analysis of realtime UDP streams: which periodicities the radio
imposes on delivery.

Two spectra per stream (receiver's kernel timestamps when present):

- **Arrival spectrum** — the periodogram of the arrival *times* as a point
  process, ``P(f) = |Σ exp(-2πi f t_k)|² / N``: about 1 for random
  arrivals, ``N`` at most for perfectly periodic ones. Delivery per 1 ms
  subframe (LTE / NR 15 kHz) puts lines at 1000 Hz and harmonics; 0.5 ms
  slots (NR 30 kHz) at 2000 Hz; a grant every 5 ms at 200 Hz and harmonics.
  The 1 ms sender is periodic itself, so the same spectrum of the *send*
  times is computed and each peak carries ``ratio`` = arrival / send power.
  A ratio well above 1 is structure the network added (re-quantised to its
  own grid); well below 1, structure it smeared out.
- **Delay spectrum** — Welch power spectrum of the excess one-way delay,
  sampled at the send times on a uniform 1 ms grid (0–500 Hz). A delay
  sawtooth from a grant / scheduling cycle, HARQ (8 ms → 125 Hz) or the
  10 ms radio frame (100 Hz) shows as peaks; ``share`` is the part of the
  delay variance within the peak.

Peaks are local maxima well above the spectrum's median (``*_MIN_RATIO``).
Clock offset does not matter (spacing only, and delay relative to its own
floor); a few-ppm rate difference between the server clock and the radio's
moves a 1000 Hz line by a few mHz, below the resolution here.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np
import pandas as pd

from .loader import TestData, load_test

#: Arrival-spectrum grid (Hz) and the minimum peak height over the median.
ARRIVAL_F_MAX = 2500.0
ARRIVAL_F_STEP = 0.5
ARRIVAL_MIN_RATIO = 20.0
#: Delay spectrum: uniform resampling step and Welch segment length.
DELAY_STEP_S = 0.001
WELCH_SEG = 2048
DELAY_MIN_RATIO = 8.0
MAX_PEAKS = 8


#: Arrival times are binned this finely before the FFT (attenuates 2.5 kHz by <3 %).
BIN_S = 50e-6


def point_spectrum(times_s: np.ndarray, freqs: np.ndarray) -> np.ndarray:
    """|Σ exp(-2πi f t)|² / N at each f (times in seconds), via an FFT of the
    arrival counts in BIN_S bins. Each output value is the strongest FFT bin
    within ±step/2 of f, so a line that drifts slightly still counts once."""
    t = np.asarray(times_s, dtype=float)
    t = t - t.min()
    n_bins = int(t.max() / BIN_S) + 1
    counts = np.bincount((t / BIN_S).astype(np.int64), minlength=n_bins).astype(float)
    spec = np.abs(np.fft.rfft(counts)) ** 2 / len(t)
    f_fft = np.fft.rfftfreq(n_bins, BIN_S)
    step = freqs[1] - freqs[0] if len(freqs) > 1 else 1.0
    lo = np.searchsorted(f_fft, freqs - step / 2)
    hi = np.minimum(np.maximum(np.searchsorted(f_fft, freqs + step / 2), lo + 1), len(spec))
    lo = np.minimum(lo, len(spec) - 1)
    # max over each [lo, hi) window: reduceat over the window starts, then
    # trim each result to its own window by masking the rest out
    out = np.empty(len(freqs))
    width = int((hi - lo).max())
    padded = np.concatenate([spec, np.zeros(width)])
    idx = lo[:, None] + np.arange(width)[None, :]
    vals = padded[idx]
    vals[np.arange(width)[None, :] >= (hi - lo)[:, None]] = 0.0
    out[:] = vals.max(axis=1)
    return out


def welch_psd(x: np.ndarray, fs: float, seg: int = WELCH_SEG) -> tuple[np.ndarray, np.ndarray]:
    """One-sided Welch PSD (Hann window, 50 % overlap) of an evenly sampled series."""
    x = np.asarray(x, dtype=float) - np.mean(x)
    seg = min(seg, len(x))
    win = np.hanning(seg)
    scale = fs * (win**2).sum()
    step = seg // 2
    acc, n = np.zeros(seg // 2 + 1), 0
    for s in range(0, len(x) - seg + 1, step):
        part = x[s : s + seg]
        acc += np.abs(np.fft.rfft((part - part.mean()) * win)) ** 2
        n += 1
    psd = acc / max(n, 1) / scale
    psd[1:-1] *= 2
    return np.fft.rfftfreq(seg, 1 / fs), psd


#: Half-width (bins) of the running median used as each spectrum's baseline.
BASELINE_HALF = 40


def _baseline(power: np.ndarray) -> np.ndarray:
    """Running median: spectra have a sloped floor, so a peak is judged
    against its neighbourhood, not the global median."""
    padded = np.pad(power, BASELINE_HALF, mode="edge")
    win = np.lib.stride_tricks.sliding_window_view(padded, 2 * BASELINE_HALF + 1)
    return np.median(win, axis=1)


def find_peaks(freqs: np.ndarray, power: np.ndarray, min_ratio: float, skip_below_hz: float = 0.0) -> list[int]:
    """Indices of local maxima at least `min_ratio` × their local baseline,
    strongest (relative to baseline) first."""
    base = np.maximum(_baseline(power), 1e-30)
    rel = power / base
    idx = [
        i
        for i in range(1, len(power) - 1)
        if freqs[i] >= skip_below_hz and power[i] >= power[i - 1] and power[i] > power[i + 1] and rel[i] >= min_ratio
    ]
    return sorted(idx, key=lambda i: -rel[i])[:MAX_PEAKS]


def fundamentals(peaks_hz: list[float], tol: float = 0.01) -> dict[float, list[float]]:
    """Group peak frequencies into harmonic series: {fundamental: [members]}.
    A peak within `tol` (relative) of an integer multiple of a lower peak
    belongs to that peak's series."""
    series: dict[float, list[float]] = {}
    for f in sorted(peaks_hz):
        base = next((b for b in series if round(f / b) >= 2 and abs(f / b - round(f / b)) < tol * round(f / b)), None)
        series.setdefault(base if base is not None else f, []).append(f)
    return series


def _arrival(rx_t: np.ndarray, tx_t: np.ndarray | None) -> dict:
    freqs = np.arange(ARRIVAL_F_STEP * 4, ARRIVAL_F_MAX + ARRIVAL_F_STEP, ARRIVAL_F_STEP)
    p_rx = point_spectrum(rx_t, freqs)
    p_tx = point_spectrum(tx_t, freqs) if tx_t is not None and len(tx_t) > 10 else None
    peaks = []
    for i in find_peaks(freqs, p_rx, ARRIVAL_MIN_RATIO, skip_below_hz=5.0):
        pk = {"hz": round(float(freqs[i]), 1), "period_ms": round(float(1e3 / freqs[i]), 3), "power": round(float(p_rx[i]), 1)}
        if p_tx is not None:
            # the sender's line may sit a bin away (different clock)
            near = p_tx[max(0, i - 2) : i + 3].max()
            pk["send_power"] = round(float(near), 1)
            # A sender line below the noise floor (~1) means the peak is new.
            pk["ratio"] = round(float(p_rx[i] / max(near, 1.0)), 1)
            pk["added_by_path"] = bool(near < ARRIVAL_MIN_RATIO)
        peaks.append(pk)
    groups = fundamentals([p["hz"] for p in peaks])
    for p in peaks:
        p["fundamental_hz"] = next(b for b, m in groups.items() if p["hz"] in m)
    return {"freqs": freqs, "rx": p_rx, "tx": p_tx, "peaks": peaks}


def _delay(send_t: np.ndarray, excess_ms: np.ndarray) -> dict | None:
    order = np.argsort(send_t)
    t, d = send_t[order], excess_ms[order]
    if len(t) < 2 * WELCH_SEG // 4 or t[-1] - t[0] < 2.0:
        return None
    grid = np.arange(t[0], t[-1], DELAY_STEP_S)
    series = np.interp(grid, t, d)  # lost packets: interpolated
    freqs, psd = welch_psd(series, 1 / DELAY_STEP_S)
    df = freqs[1] - freqs[0]
    total = psd[1:].sum() * df
    peaks = []
    for i in find_peaks(freqs, psd, DELAY_MIN_RATIO, skip_below_hz=2.0):
        share = psd[max(1, i - 1) : i + 2].sum() * df / total if total > 0 else 0.0
        peaks.append({"hz": round(float(freqs[i]), 1), "period_ms": round(float(1e3 / freqs[i]), 2), "share": round(float(share), 3)})
    return {"freqs": freqs, "psd": psd, "peaks": peaks}


def analyze(td: TestData) -> list[dict]:
    """Per realtime stream: arrival-spectrum and delay-spectrum peaks, plus
    the raw spectra (under ``_spectra``, for plotting)."""
    pk = td.packets
    if pk.empty:
        return []
    rx_all = pk[(pk["direction"] == "RECEIVED") & pk["sender_utc_ns"].notna()]
    out = []
    for (node, flow_id), recv in rx_all.groupby(["node", "flow_id"]):
        kernel = pd.to_numeric(recv["kernel_rx_utc_ns"], errors="coerce") if "kernel_rx_utc_ns" in recv else None
        use_kernel = kernel is not None and kernel.notna().mean() > 0.9
        t_rx = (kernel if use_kernel else pd.to_numeric(recv["utc_ns"], errors="coerce")).to_numpy(dtype=float)
        t_tx = pd.to_numeric(recv["sender_utc_ns"], errors="coerce").to_numpy(dtype=float)
        ok = ~(np.isnan(t_rx) | np.isnan(t_tx))
        t_rx, t_tx = t_rx[ok], t_tx[ok]
        if len(t_rx) < 200:
            continue
        sender_node = "mobile" if node == "server" else "server"
        sent = pk[(pk["node"] == sender_node) & (pk["flow_id"] == flow_id) & (pk["direction"] == "SENT")]
        if "send_status" in sent:
            sent = sent[sent["send_status"].isna() | (sent["send_status"] == "OK")]
        # sender spectrum over the same window as the arrivals
        tx_times = pd.to_numeric(sent["utc_ns"], errors="coerce").dropna().to_numpy(dtype=float)
        tx_times = tx_times[(tx_times >= t_tx.min()) & (tx_times <= t_tx.max())]
        arr = _arrival(t_rx / 1e9, tx_times / 1e9 if len(tx_times) else None)
        owd = (t_rx - t_tx) / 1e6
        dl = _delay(t_tx / 1e9, owd - np.percentile(owd, 0.5))
        out.append(
            {
                "flow_id": int(flow_id),
                "direction": "uplink" if node == "server" else "downlink",
                "clock": "kernel" if use_kernel else "app",
                "packets": int(len(t_rx)),
                "arrival_peaks": arr["peaks"],
                "delay_peaks": dl["peaks"] if dl else [],
                "_spectra": {"arrival": arr, "delay": dl},
            }
        )
    return sorted(out, key=lambda e: e["flow_id"])


def plot(streams: list[dict], title: str, path: Path) -> None:
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    fig, axes = plt.subplots(len(streams), 2, figsize=(13, 3.6 * len(streams)), squeeze=False)
    for row, e in zip(axes, streams):
        a, d = e["_spectra"]["arrival"], e["_spectra"]["delay"]
        ax = row[0]
        if a["tx"] is not None:
            ax.semilogy(a["freqs"], a["tx"], color="#bbbbbb", lw=0.7, label="sent")
        ax.semilogy(a["freqs"], a["rx"], color="#1f6fb4", lw=0.7, label="received (kernel)")
        for p in a["peaks"][:5]:
            ax.annotate(f"{p['hz']:g} Hz", (p["hz"], p["power"]), fontsize=7, xytext=(3, 3), textcoords="offset points")
        ax.set_title(f"flow {e['flow_id']} {e['direction']}: arrival spectrum", fontsize=9)
        ax.set_xlabel("Hz")
        ax.legend(fontsize=7)
        ax = row[1]
        if d:
            ax.semilogy(d["freqs"][1:], d["psd"][1:], color="#c0392b", lw=0.8)
            for p in d["peaks"][:5]:
                ax.annotate(f"{p['period_ms']:g} ms", (p["hz"], d["psd"][np.searchsorted(d["freqs"], p["hz"])]),
                            fontsize=7, xytext=(3, 3), textcoords="offset points")
        ax.set_title(f"flow {e['flow_id']} {e['direction']}: excess-delay spectrum", fontsize=9)
        ax.set_xlabel("Hz")
    fig.suptitle(title, fontsize=10)
    fig.tight_layout()
    fig.savefig(path, dpi=110)
    plt.close(fig)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("test_dirs", nargs="+")
    ap.add_argument("--plot-dir", help="write <test_id>_spectra.png here")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)
    result = {}
    for d in args.test_dirs:
        td = load_test(d)
        tid = td.test_id or d
        streams = analyze(td)
        if args.plot_dir and streams:
            plot(streams, tid, Path(args.plot_dir) / f"{tid}_spectra.png")
        result[tid] = [{k: v for k, v in e.items() if k != "_spectra"} for e in streams]
    if args.json:
        json.dump(result, sys.stdout, indent=1)
        print()
    else:
        for tid, streams in result.items():
            print(f"== {tid}")
            for e in streams:
                print(f"flow {e['flow_id']} {e['direction']} ({e['clock']}, {e['packets']} pkts)")
                print("  arrival peaks (Hz, power, ×sent, new):",
                      [(p["hz"], p["power"], p.get("ratio"), p.get("added_by_path")) for p in e["arrival_peaks"]])
                print("  delay peaks:  ", [(p["period_ms"], p["share"]) for p in e["delay_peaks"]])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

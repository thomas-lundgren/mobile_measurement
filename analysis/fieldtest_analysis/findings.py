"""Findings for one day, computed from that day's chart series (series.py):

- ``tests``: per-test overview (radio, delay percentiles, loss, throughput)
  with flags for link pauses, one-way gaps and a server-side bottleneck.
- ``regime_changes``: moments where the delay in a bulk direction drops by
  at least half while that direction's throughput rises, each classified.
- ``link_pauses``: both-direction stall episodes, with the recurrence
  period that best fits their spacing.
- ``gap_episodes``: one-way stall episodes.

Classifications are heuristics from the 200 ms bins and say so in their
text; they point at where to look, they do not prove a cause.
"""

from __future__ import annotations

import numpy as np
import pandas as pd

#: 3 s of 200 ms bins on each side of a candidate change.
REGIME_WINDOW = 15
REGIME_MIN_DROP_MS = 20.0
REGIME_MIN_TPUT_GAIN = 1.2
#: A cell change this long before a regime change counts as its cause.
HANDOVER_LOOKBACK_S = 8.0
#: Downlink bins at or above this rate with little queueing suggest the
#: bottleneck is on the server side, not the radio.
SERVER_SIDE_MBPS = 200.0
SERVER_SIDE_DELAY_MS = 15.0
#: Range searched for the link-pause period.
PAUSE_PERIOD_RANGE_S = (120.0, 900.0)


def _frame(day: dict, test: dict) -> pd.DataFrame:
    t = np.asarray(day["t"], dtype=float)
    idx = np.nonzero((t >= test["start"]) & (t <= test["end"]))[0]
    cols = ["t", "ul_med", "dl_med", "down_mbps", "up_mbps", "mode", "earfcn", "nrarfcn",
            "lte_pci", "nr_pci", "transport", "wifi_freq_mhz", "wifi_bw_mhz", "wifi_standard"]  # fmt: skip
    df = pd.DataFrame({c: [day[c][i] for i in idx] if c in day else [None] * len(idx) for c in cols})
    return df.astype(object).where(df.notna(), None)  # all-null columns would read as NaN


def _radio_label(day: dict, df: pd.DataFrame) -> str | None:
    """Most common radio over the test, e.g. "5G NSA · B8 + n78"."""
    names = {"NR_SA": "5G SA", "NR_NSA": "5G NSA", "NR_ADVANCED": "5G+", "LTE_CA": "4G CA",
             "LTE_ADVANCED_PRO": "4G+", "LTE": "4G"}  # fmt: skip
    labels = []
    for r in df.itertuples(index=False):
        if r.transport == "WIFI":
            f = r.wifi_freq_mhz
            band = None if f is None else ("2.4 GHz" if f < 2500 else "5 GHz" if f < 5925 else "6 GHz")
            labels.append(" · ".join(x for x in ["Wi-Fi", band] if x))
            continue
        if r.mode is None and r.earfcn is None:
            continue
        # Channel numbers may come back as floats (3600.0); the table keys are "3600".
        lte = day.get("channels", {}).get("lte", {}).get(str(int(r.earfcn))) if r.earfcn is not None else None
        bands = [lte.split(" · ")[0]] if lte else []
        if r.mode and r.mode.startswith("NR"):
            nr = day.get("channels", {}).get("nr", {}).get(str(int(r.nrarfcn))) if r.nrarfcn is not None else None
            bands.append(nr.split(" · ")[0] if nr else "n?")
        name = names.get(r.mode, "mode not recorded")
        labels.append(f"{name} · {' + '.join(bands)}" if bands else name)
    if not labels:
        return None
    counts = pd.Series(labels).value_counts()
    top = counts.index[0]
    return top if len(counts) == 1 else f"{top} ({round(100 * counts.iloc[0] / len(labels))} %)"


def _cell_changes(df: pd.DataFrame) -> list[tuple[float, str]]:
    out = []
    for col, rat in (("lte_pci", "LTE"), ("nr_pci", "5G")):
        prev = None
        for t, v in zip(df["t"], df[col]):
            if v is None:
                continue
            if prev is not None and v != prev:
                out.append((t, f"{rat} PCI {int(prev)} → {int(v)}"))
            prev = v
    return out


def _day_ceiling_mbps(day: dict) -> float | None:
    s = pd.to_numeric(pd.Series(day["down_mbps"]), errors="coerce")
    peak = s.rolling(5).mean().max()
    return None if pd.isna(peak) else float(peak)


def regime_changes(day: dict) -> list[dict]:
    W = REGIME_WINDOW
    ceiling = _day_ceiling_mbps(day)
    gaps = [e for e in day.get("stall_episodes", []) if e["directions"] != "both"]
    out = []
    for test in day["tests"]:
        df = _frame(day, test)
        if len(df) < 2 * W + 5:
            continue
        changes = _cell_changes(df)
        for direction, dcol, tcol in (("downlink", "dl_med", "down_mbps"), ("uplink", "ul_med", "up_mbps")):
            tp = pd.to_numeric(df[tcol], errors="coerce")
            dl = pd.to_numeric(df[dcol], errors="coerce")
            if tp.isna().all() or tp.max() < 5:
                continue
            best = None
            for k in range(W, len(df) - W):
                db, da = dl.iloc[k - W : k].median(), dl.iloc[k : k + W].median()
                tb, ta = tp.iloc[k - W : k].median(), tp.iloc[k : k + W].median()
                if any(pd.isna(v) for v in (db, da, tb, ta)) or tb <= 1:
                    continue
                if da <= 0.5 * db and db - da >= REGIME_MIN_DROP_MS and ta >= REGIME_MIN_TPUT_GAIN * tb:
                    score = db / max(da, 1.0)
                    if best is None or score > best[0]:
                        best = (score, k, db, da, tb, ta)
            if not best:
                continue
            _, k, db, da, tb, ta = best
            t = float(df["t"].iloc[k])
            dip = tp.iloc[max(0, k - 8) : k + 1].min() < 0.3 * tb
            ho = [c for c in changes if t - HANDOVER_LOOKBACK_S <= c[0] <= t]
            gap = [e for e in gaps if e["directions"] == direction and e["start"] <= t + 1 and e["end"] >= t - 2]
            if ho:
                kind, why = "after handover", f"{ho[-1][1]} {t - ho[-1][0]:.1f} s earlier; queue drains on the new cell."
            elif gap:
                article = "an" if direction == "uplink" else "a"
                kind, why = "stall recovery", f"{article} {direction} gap episode ends here; the backlog drains once packets flow again."
            elif direction == "downlink" and ceiling and ta >= 0.85 * ceiling and da <= SERVER_SIDE_DELAY_MS:
                kind, why = (
                    "bottleneck moved to the server side",
                    f"throughput reaches the day's ceiling (~{ceiling:.0f} Mbit/s) with no queueing: the radio became faster than "
                    "the server's link, so radio head-of-line blocking is no longer measured.",
                )
            elif dip:
                kind, why = (
                    "likely radio reconfiguration",
                    "a short interruption on the same cell, then more capacity (e.g. a carrier-aggregation change, which is not recorded).",
                )
            else:
                kind, why = "radio capacity change", "same cell, no interruption; capacity rose and the queue shrank."
            out.append(
                {
                    "time": t, "test_id": test["test_id"], "plan": test["plan"], "direction": direction,
                    "delay_before_ms": round(db), "delay_after_ms": round(da),
                    "tput_before_mbps": round(tb), "tput_after_mbps": round(ta),
                    "interruption_before": bool(dip), "kind": kind, "explanation": why,
                }  # fmt: skip
            )
    return sorted(out, key=lambda r: r["time"])


#: An interval "fits" a period when it is this close to a whole multiple.
PAUSE_FIT_S = 5.0


def pause_period(starts: list[float]) -> dict | None:
    """Period P that best explains the spacing of link pauses as whole
    multiples of P (pauses can fall outside tests, so one gap may span
    several periods). Scored by how many intervals fit within PAUSE_FIT_S;
    among equal scores the longest period wins, since P/2, P/3 … fit any
    multiple of P just as well."""
    gaps = np.diff(sorted(starts))
    gaps = gaps[gaps > 30]
    if len(gaps) < 2:
        return None
    lo, hi = PAUSE_PERIOD_RANGE_S
    step = 0.1
    cands = []  # (period, fits, rms of fitting intervals, residuals)
    for p in np.arange(lo, hi, step):
        n = np.maximum(np.round(gaps / p), 1)
        resid = np.abs(gaps - n * p)
        ok = resid[resid <= PAUSE_FIT_S]
        cands.append((p, len(ok), float(np.sqrt(np.mean(ok**2))) if len(ok) else np.inf, resid))
    top = max(c[1] for c in cands)
    best_p = [c for c in cands if c[1] == top]
    # Periods with the top score form runs; take the run with the longest
    # period, then the most accurate period inside it.
    run = [best_p[-1]]
    for c in reversed(best_p[:-1]):
        if run[-1][0] - c[0] > 1.5 * step:
            break
        run.append(c)
    p, fits, _, resid = min(run, key=lambda c: c[2])
    ok = resid[resid <= PAUSE_FIT_S]
    return {
        "period_s": round(float(p), 1),
        "fits": fits,
        "intervals": len(gaps),
        "rms_error_s": round(float(np.sqrt(np.mean(ok**2))), 1) if len(ok) else None,
    }


def day_findings(day: dict) -> dict:
    episodes = day.get("stall_episodes", [])
    pauses = [e for e in episodes if e["directions"] == "both"]
    tests = []
    for test in day["tests"]:
        df = _frame(day, test)
        down = pd.to_numeric(df["down_mbps"], errors="coerce")
        dlm = pd.to_numeric(df["dl_med"], errors="coerce")
        loaded = down[down > 0]
        server_side = (
            round(float(((down >= SERVER_SIDE_MBPS) & (dlm < SERVER_SIDE_DELAY_MS)).sum() / len(loaded)), 2)
            if len(loaded)
            else None
        )
        in_test = [e for e in episodes if e["test_id"] == test["test_id"]]
        tests.append(
            dict(
                test,
                radio=_radio_label(day, df),
                stats=test.get("stats"),
                link_pauses=sum(e["directions"] == "both" for e in in_test),
                gap_episodes=sum(e["directions"] != "both" for e in in_test),
                server_side_share=server_side,
            )
        )
    return {
        "day": day["day"],
        "tests": tests,
        "regime_changes": regime_changes(day),
        "link_pauses": pauses,
        "pause_period": pause_period([e["start"] for e in pauses]),
        "gap_episodes": [e for e in episodes if e["directions"] != "both"],
    }

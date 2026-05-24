# fieldtest-analysis (Phase 4)

Python analysis toolchain for the 5G field-test platform. Reads an aggregated
test tree (spec §12.1) and produces RTT / one-way-delay / jitter / loss metrics,
derived radio events, and a per-test summary (§12.8).

## Install & run

```bash
cd analysis
python3 -m venv .venv && .venv/bin/pip install -e ".[dev,plots]"

# summary for one test directory
.venv/bin/python -m fieldtest_analysis <test_dir> [-o summary.json]

.venv/bin/python -m pytest -q          # tests (synthetic + real-capture fixtures)
```

A `<test_dir>` is the aggregated tree: `mobile/` and/or `server/` subfolders
(each with segmented `packets_*.parquet`, plus optional `events`/`clock`), and
the mobile-only `gps.parquet` / `radio.parquet` at the root.

## What it computes

| Metric | Source | Notes |
|---|---|---|
| **RTT** | probe echo: a node's `SENT`↔`RECEIVED` on `(flow_id, seq)` | monotonic clock — authoritative |
| **OWD** (up/downlink) | `utc_ns - sender_utc_ns` at the receiver, **offset-corrected** | cross-clock; offset removed via `clock.parquet` or probe-RTT symmetry (see below) |
| **Jitter (IPDV)** | `actual_interval_ns - expected_interval_ns` | monotonic intervals |
| **Loss** | sequence gaps (echo or one-directional) | send-only flows report NA (need the receiver) |
| **Events** | `radio.parquet` deltas | HANDOVER, BAND_CHANGE, COVERAGE_GAP, NEIGHBOR_COUNT_CHANGE |

The serving cell is RAT-agnostic: NR fields when present, else the LTE anchor —
a Pixel indoors is frequently LTE-only with no NR serving cell.

## Design notes / known caveats

- **OWD is offset-corrected** (`clock.py`). Raw cross-clock OWD is dominated by
  the phone↔server UTC offset (~1.15 s on the current undisciplined captures —
  *not* network delay). The offset `O = mobile_utc - server_utc` is estimated
  and removed, so the summary reports both `raw_ns` and `corrected_ns` per
  direction, plus the `offset_ns` / `offset_method` / `offset_uncertainty_ns`
  used. Sources, preferred first:
    1. **`clock.parquet`** `estimated_offset_ns` (a disciplined offset measured
       on-device via NTP/GPS/chrony). The Logger does not emit this yet.
    2. **probe-RTT symmetry** — probe RTT is offset-free (single monotonic
       clock), so one-way ≈ RTT/2 under path symmetry; comparing that to raw
       downlink OWD yields `O`. Absolute OWD then carries a ±RTT/2 bound (the
       asymmetry we can't resolve without a disciplined clock); OWD *variation*
       (jitter) is exact regardless.
  On the committed `test_udp` fixture this turns a raw downlink p50 of ~1153 ms
  into a corrected p50 of ~2.8 ms (≈ half the 5.6 ms probe RTT). RTT and jitter
  use the monotonic clock and need no correction.
- **Mobile-only today.** MVP uploads are mobile-only, so uplink OWD (server is
  the receiver) has no rows until the server node's `packets.parquet` is
  aggregated under the test. The metric is implemented and will populate then.
- **Tolerant by construction** (invariant 4): missing files/columns are filled
  with NA, unknown columns are preserved, segmented packet files are
  concatenated.

## Layout

```
fieldtest_analysis/
  schema.py    canonical columns per table (mirrors contracts/parquet/*)
  loader.py    tolerant test-tree loader -> TestData
  metrics.py   rtt / owd (raw + corrected) / jitter / loss
  clock.py     clock-offset estimation (clock.parquet | probe-RTT symmetry)
  events.py    derived radio events
  summary.py   §12.8 summary assembly
  cli.py       fieldtest-analyze
tests/         synthetic unit tests + real-capture fixtures
```

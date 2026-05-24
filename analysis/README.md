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
| **OWD** (up/downlink) | `utc_ns - sender_utc_ns` at the receiver | cross-clock; carries an uncertainty bound from both nodes' `clock.parquet` |
| **Jitter (IPDV)** | `actual_interval_ns - expected_interval_ns` | monotonic intervals |
| **Loss** | sequence gaps (echo or one-directional) | send-only flows report NA (need the receiver) |
| **Events** | `radio.parquet` deltas | HANDOVER, BAND_CHANGE, COVERAGE_GAP, NEIGHBOR_COUNT_CHANGE |

The serving cell is RAT-agnostic: NR fields when present, else the LTE anchor —
a Pixel indoors is frequently LTE-only with no NR serving cell.

## Design notes / known caveats

- **OWD needs clock discipline.** On the current captures the raw downlink OWD
  is ~1.15 s — that is the **UTC offset between the phone and the host**, not
  network delay (the two clocks are unsynchronised). This is exactly why the
  spec computes OWD only in post-processing *with* uncertainty bounds. Today the
  Logger does not emit `clock.parquet`, so the bound reads `null`. Follow-up:
  have the Logger write clock-calibration snapshots (RustCore already has the
  writer) so OWD can be offset-corrected and bounded. RTT and jitter, which use
  the monotonic clock, are unaffected and correct.
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
  metrics.py   rtt / owd / jitter / loss
  events.py    derived radio events
  summary.py   §12.8 summary assembly
  cli.py       fieldtest-analyze
tests/         synthetic unit tests + real-capture fixtures
```

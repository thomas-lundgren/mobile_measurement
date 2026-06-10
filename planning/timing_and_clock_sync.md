# Timing and clock synchronisation

Timing is the most fragile signal in this platform. Most metrics this system
exists to produce — one-way delay, jitter, inter-arrival, handover timing,
GPS-correlated radio events — collapse to "wrong" if a clock drifts more
than the effect we are trying to measure. This document is the single
reference for how time is *sampled*, *kept accurate*, *recorded*, and *used*
in analysis, on both nodes and end-to-end.

It complements:

- the architecture spec §3.2 (the short version of the rule)
- the architecture spec §11 (OWD math)
- `contracts/parquet/clock.parquet.yaml` (the recorded shape)
- `core/src/timestamp.rs` (the implementation)

---

## 1. The dual-clock rule

Every timestamp in this system carries **two** values, sampled together:

| Field     | Domain                                | Authoritative for                                  |
|-----------|---------------------------------------|----------------------------------------------------|
| `mono_ns` | `CLOCK_MONOTONIC` — local, no leaps   | ordering, intervals, jitter, inter-arrival, bursts |
| `utc_ns`  | `CLOCK_REALTIME` — wall-clock UTC     | cross-node correlation, OWD, multi-device alignment|

The two are never collapsed into one. The monotonic clock is *always* used
for local-only quantities — it cannot jump and cannot go backwards. The UTC
clock is the *only* clock that can be compared across two nodes — it can
jump (NTP step) and skew (drift, suspend), so any quantity that uses it
carries an uncertainty bound.

The Rust type that enforces this lives in `core/src/timestamp.rs`:

```rust
#[derive(Clone, Copy, ...)]
pub struct Timestamp {
    pub mono_ns: u64,   // clock_gettime(CLOCK_MONOTONIC)
    pub utc_ns:  u64,   // clock_gettime(CLOCK_REALTIME)
}
impl Timestamp { pub fn now() -> Self { ... } }
```

`Timestamp::now()` calls `clock_gettime` twice, in order: monotonic first,
then realtime, with no allocation or syscall in between. The asymmetry
between the two samples is bounded by a few nanoseconds on modern kernels —
much smaller than any phenomenon this system measures.

---

## 2. Symmetric implementation across server and Android

The shared Rust core (`fieldtest-core`) is what stamps every log row on
**both** nodes. Because `core/` is cross-compiled to Android via
`cargo-ndk`, `Timestamp::now()` runs exactly the same code on:

- **Linux server** — `libc::clock_gettime` against the host kernel
- **Android Logger** — `libc::clock_gettime` against the Linux kernel that
  Android runs on (`CLOCK_MONOTONIC` and `CLOCK_REALTIME` are POSIX
  primitives, available identically on bionic)

This is the single largest symmetry guarantee in the platform: there is no
"Java timestamp" and "Rust timestamp" to reconcile. The Java shell on the
Logger app never calls `System.currentTimeMillis()` or `System.nanoTime()`
for anything measurement-relevant — it hands control to the Rust core via
JNI and lets the core stamp.

The `realtime_udp` packet carries `(send_mono_ns, send_utc_ns)` in its
payload — both fields sampled by the same `Timestamp::now()` call at send
time, so the receiver gets the sender's view of both clocks atomically.

---

## 3. What keeps each node's UTC accurate

`mono_ns` needs nothing — it's a hardware counter monotonised by the
kernel. `utc_ns` is the one that has to be *disciplined* by an external
reference. Each node does this independently; we never expect them to
agree perfectly, and we record what discipline they had.

### 3.1 Linux server

- **NTP discipline:** chrony recommended (`chrony` or `systemd-timesyncd`),
  pointed at a stable stratum-1 or stratum-2 source. Netnod / national
  time labs are good defaults in the Nordic region.
- **Optional hardware:** a GPS-disciplined PPS source gives sub-microsecond
  accuracy and is recommended for OWD work that needs to distinguish
  network delay from clock skew.
- **Recording:** the server periodically samples `chronyc tracking` (or
  equivalent) and writes a `ClockRow` to its own `server/clock.parquet`
  with `source = "chrony"` and the reported uncertainty.

### 3.2 Android Logger

Android's UTC discipline is *less controllable* and *less accurate*. The
Logger uses what's available, ranked best to worst:

1. **GPS time** when a fix is locked and the underlying location subsystem
   exposes it. This is the only source that's potentially better than the
   server-side NTP discipline (atomic-clock accuracy ± propagation delay).
   The Logger records `source = "gps"`.
2. **NITZ / Network-provided time** from the mobile operator. Reasonable
   on most networks but operator-dependent; quality varies.
3. **Android system NTP** (the kernel `CLOCK_REALTIME` disciplined by
   AOSP's time service). Always available, usually within tens of ms.

The Logger periodically writes a `ClockRow` to `mobile/clock.parquet`
recording which source `CLOCK_REALTIME` was tied to at that moment, the
estimated uncertainty (from GPS HDOP / NTP variance / a fallback of
~100 ms for "system"), and the estimated signed offset between local and
reference.

### 3.3 What "calibration" means here

We don't run our own time-sync protocol. We don't push commands to discipline
the device's clock. Instead, each node tells the system what it observes:

- "At local mono = T1, my UTC was U1, derived from source X with
  uncertainty ±U_unc"

Analysis uses these snapshots to bound OWD uncertainty rather than to
*adjust* timestamps. We never rewrite timestamps in the log; the raw
samples are preserved and uncertainty is propagated explicitly.

---

## 4. `clock.parquet` — the calibration record

Both nodes write a `clock.parquet` segment per test. The cadence is
`logging_config.clock_calibration_interval_sec` (default **30 s**), pushed
by the server in the effective logging config.

Schema (`contracts/parquet/clock.parquet.yaml`):

| Column                  | Type   | Meaning                                                            |
|-------------------------|--------|--------------------------------------------------------------------|
| `mono_ns`               | u64    | Local monotonic at the moment of the snapshot                      |
| `utc_ns`                | u64    | Local realtime at the moment of the snapshot                       |
| `source`                | utf8?  | `gps` \| `ntp` \| `chrony` \| `system`                             |
| `uncertainty_ns`        | u64?   | Estimated ± bound of the UTC sample against the reference          |
| `estimated_offset_ns`   | i64?   | Signed offset of local UTC from the reference (sign matters)       |

`mono_ns` is the join key — it tells analysis "*at this point in the
recording*, here's what was true about the UTC clock." Two `ClockRow`s
bracketing a packet event let us interpolate the clock's accuracy at the
event's `mono_ns`.

Both nodes write this. **The server's uncertainty matters as much as the
mobile's.** If chrony reports ±2 ms and the mobile is on GPS with ±50 µs,
OWD uncertainty is dominated by the server, not by the supposedly "less
accurate" device.

---

## 5. End-to-end OWD math

### 5.1 Downlink (FromServer)

The server send-stamps a packet, the mobile receive-stamps it.

```
OWD_dl     = mobile.recv_utc_ns - server.send_utc_ns
uncertainty = mobile.utc_uncertainty + server.utc_uncertainty
              + clock_skew_between_snapshots
```

`server.send_utc_ns` travels in the packet payload (`realtime_udp` packet
carries `send_mono_ns` *and* `send_utc_ns`). `mobile.recv_utc_ns` comes
from `Timestamp::now()` at the moment `recvmsg` returns.

The uncertainty term is the sum of:

- the server's `uncertainty_ns` at its closest preceding `clock.parquet`
  row (interpolated against `mono_ns`)
- the mobile's `uncertainty_ns` interpolated the same way
- a small drift term `|skew_rate| × Δt` between the calibration sample
  and the packet event (negligible if the calibration interval is short)

A negative OWD is *possible* — it means the uncertainty envelope crosses
zero, i.e. the network delay is below the joint clock-accuracy floor. The
correct interpretation is "OWD ≲ uncertainty", not "OWD < 0". Analysis
clips and flags these rows.

### 5.2 Uplink (ToServer)

Mirror of 5.1, with sender and receiver swapped:

```
OWD_ul     = server.recv_utc_ns - mobile.send_utc_ns
```

### 5.3 RTT vs OWD

Two RTT signals are available and they have different uses:

- **`probe` echo RTT**: `recv_mono - send_mono` on the **same node**
  (UE). Uses *one* clock (mono), so it has *no* clock-uncertainty term —
  it's the cleanest end-to-end latency signal.
- **OWD halves**: `OWD_dl + OWD_ul` should equal RTT modulo asymmetry.
  If it doesn't, the difference between RTT (clean) and `OWD_dl + OWD_ul`
  is the **clock-skew residual** — a useful diagnostic for whether the
  UTC discipline was good enough.

This is the most important sanity check in analysis: probe RTT should not
contradict the sum of OWD halves by more than `2 × uncertainty`.

---

## 6. Within-flow timing (no UTC needed)

Most analysis lives entirely in monotonic space and never touches UTC:

- **Inter-arrival jitter** = `mono_recv[i] - mono_recv[i-1]`; UTC unused.
- **Expected vs actual interval**: `actual_interval_ns` column in
  `packets.parquet` is `mono_recv - mono_prev_recv`; `expected_interval_ns`
  is `1e9 / rate_pps`. Comparison is in mono space.
- **Burst detection**, **packet ordering**, **loss-gap timing** — all in
  monotonic space.

This is why `mono_ns` is "authoritative" and `utc_ns` is "for correlation":
the most important per-flow signals are computed without any cross-node
clock involved, and so are immune to UTC drift entirely.

---

## 7. Failure modes and what we do about them

| Failure                                  | Detection                                            | Mitigation                                                                  |
|------------------------------------------|------------------------------------------------------|-----------------------------------------------------------------------------|
| Server NTP outage (drift grows)          | `chronyc` source state → `clock.parquet`             | `uncertainty_ns` grows; OWD rows get marked "loose"; analysis can filter    |
| Phone loses GPS                          | source falls back from `gps` to `system`             | uncertainty jumps from ~50 µs to ~100 ms; visible in plots                  |
| NTP step (UTC jumps)                     | `utc_ns` discontinuity not matched by `mono_ns`      | analysis splits the trace at the step; mono-based metrics unaffected        |
| Suspended app (mono pauses)              | `mono_ns` gap with no rows                           | usually unrelated to active tests (foreground service prevents); detectable |
| Realtime UDP send-time vs receive-time   | `actual_interval_ns` outliers                        | direct jitter signal; not a clock problem, a measurement signal             |
| Mobile clock simply wrong (no GPS, no NTP, fresh install) | first `clock.parquet` row has `source = "system"` only | analysis warns "OWD may be unbounded for this session"                      |

The platform never silently *fixes* a bad clock. The raw sample is
recorded, the uncertainty inflates accordingly, and analysis decides what
to keep.

---

## 8. System-level integrity checks

When `analysis/` opens a test it runs:

1. **Monotonic ordering**: every `packets.parquet` row's `mono_ns` ≥ the
   prior row's `mono_ns`. Violation = data corruption (we have never
   observed this; the type system prevents it on the write side).
2. **Calibration coverage**: at least one `clock.parquet` row brackets
   every `packets.parquet` segment on each node. Missing → analysis
   degrades OWD to "RTT-only".
3. **RTT vs OWD agreement**: `RTT_probe` vs `OWD_dl + OWD_ul` should
   agree within `2 × max(uncertainty)`. Disagreement is a clock-quality
   flag, not a measurement error.
4. **Physical floor**: OWD must exceed `distance / c`. Used as a smoke
   check for catastrophic clock errors (e.g. wrong time zone applied).

---

## 9. Recipe for a high-accuracy run

If you want OWD numbers tight enough to argue about radio scheduling
(single-digit ms or better):

- **Server side**: chrony pointed at stratum-1 with a PPS-disciplined
  GPS receiver. `chronyc tracking` reporting sub-100 µs offset.
- **Mobile side**: clear sky-view, location services on, GPS lock
  acquired and confirmed in the first `clock.parquet` row
  (`source = "gps"`).
- **Logging config**: `clock_calibration_interval_sec = 5` or `10` (more
  frequent calibration = tighter interpolation).
- **Test**: include a `probe` flow alongside the `realtime_udp` flows so
  the RTT-vs-OWD sanity check is available.

If you only have "phone in pocket, server on the office LAN", chrony /
system NTP gets you to the tens-of-milliseconds range — fine for jitter,
loss, throughput, handover detection, and DSCP transparency, all of
which use mono-only or non-timing signals.

---

## 10. Reference: the relevant files

- `core/src/timestamp.rs` — `Timestamp::now()` (both platforms)
- `core/src/aux_log.rs` — `ClockRow` definition + Parquet writer
- `contracts/parquet/clock.parquet.yaml` — committed schema
- `contracts/schemas/logging_config.schema.json` —
  `clock_calibration_interval_sec` field
- `planning/5_g_field_test_platform_architecture_spec.md` §3.2 — short rule
- `planning/5_g_field_test_platform_architecture_spec.md` §11 — OWD math
- `analysis/fieldtest_analysis/owd.py` — the implementation of §5

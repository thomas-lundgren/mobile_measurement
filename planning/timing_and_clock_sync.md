# Timing and clock synchronisation

Timing is the most fragile signal in this platform. RTT, one-way delay,
jitter, inter-arrival, handover timing, GPS-correlated radio events — every
high-value metric the platform exists to produce — collapses to noise if
clocks drift more than the effect we want to see, or if the *brackets*
inside which we measure quietly include phenomena we did not intend to
measure.

This document is the single reference for **what the platform measures**,
**when the stopwatch starts and stops**, **what software and hardware
layers sit inside that bracket**, and **how clocks on the two nodes are
disciplined and recorded** so analysis can attach the right uncertainty to
every number.

Read paths:

- "I just want to interpret an OWD number" → §1, §2, §10
- "I need to know if a result is trustworthy" → §1, §6, §11, §12
- "I'm porting / extending the engine" → §3, §4, §5
- "I want sub-millisecond OWD" → §14

Companion files:

- architecture spec §3.2 (the one-paragraph rule), §11 (the math)
- `core/src/timestamp.rs` — the implementation
- `contracts/parquet/clock.parquet.yaml` — the calibration record

---

## 1. What an OWD number actually contains — the measured path

Every OWD value in `packets.parquet` is, exactly:

```
OWD = (recv-side Timestamp::now() captured immediately AFTER socket.recv returns)
    − (send-side Timestamp::now() captured immediately BEFORE socket.send was called)
```

Read that again. The receive timestamp is the first thing the user-space
thread does after the kernel hands it the packet. The send timestamp is
the last thing the user-space thread does before asking the kernel to
deliver. Everything between those two points sits *inside* the OWD bracket.
Everything outside — packet construction on the sender side after the
timestamp, post-receive processing on the receiver side, Parquet writes,
JNI hops, GC pauses on the Java shell — is *outside*.

For a UE→server UDP datagram (uplink, `ToServer`), what's inside the
bracket is the whole path:

```
USER SPACE (UE Rust core)
   │  Timestamp::now() captured  ──────────────── ◄ stopwatch starts
   │  serialise packet (~ 100 ns)
   │  socket.send(bytes)
   ▼
KERNEL (Android, Linux)
   │  sk_buff build, UDP+IP headers, route lookup, netfilter
   │  qdisc enqueue, rmnet/wwan driver
   ▼
MODEM FIRMWARE
   │  PDCP (ROHC compression, ciphering, integrity)
   │  RLC (segmentation, ARQ)
   │  MAC scheduling — Scheduling Request → UL grant via PDCCH
   │  RRC state must be CONNECTED (forced; see §7.1)
   │  PHY (FEC, modulation), HARQ retransmissions if needed
   ▼
AIR INTERFACE
   │  free-space propagation (~ 3 µs / km, usually negligible)
   ▼
gNB / eNB
   │  PHY decode, RLC reassembly, PDCP decompress + decipher
   ▼
MOBILE CORE  (UPF / SGW+PGW)
   │  GTP-U decap, QoS enforcement, NAT/CGNAT, egress to internet
   ▼
INTERNET
   │  many AS hops, queueing at each
   ▼
SERVER NIC
   │  DMA to RX ring, IRQ → softirq (NAPI)
   ▼
KERNEL (Linux, server)
   │  network stack, UDP demux, socket-buffer enqueue
   │  scheduler wakes the data-generator's flow thread
   ▼
USER SPACE (server Rust core)
   │  recvmsg returns
   │  Timestamp::now() captured  ──────────────── ◄ stopwatch stops
```

The downlink (`FromServer`) path is the same in reverse. **Both
directions measure exactly the same set of components** plus an OS-noise
component that depends on the OS (server-side OS noise on uplink, mobile-side
OS noise on downlink); see §7.

### What an OWD number *does not* contain

- **The cost of `Timestamp::now()` itself** — sampling happens *outside*
  the network event. The stopwatch starts before the call to `send` and
  stops after `recv` returns, so the timing call's own cost is *not*
  part of the OWD. (See §6 for what that cost is, and why it does not
  alias into nearby measurements.)
- **Post-receive processing**: `PacketRow` construction, the SPSC ring push,
  Parquet writing. All these happen *after* the receive timestamp.
- **Pre-send work on the sender after the timestamp**: just the packet
  serialisation (~100 ns) and the syscall wrapper, all sub-microsecond
  on every platform we run on, well below the network delay floor.
- **JNI hops on Android**: the data plane never crosses JNI. The Java
  shell starts the Rust thread once; from then on, send/recv runs entirely
  in Rust against `libc`. There is no Java in the OWD bracket.
- **ART garbage collection**: same reason — no Java code in the hot path,
  no ART activity to pause it.
- **tokio executor scheduling**: the data-generator flow threads are raw
  OS threads, not tokio tasks. There is no async executor in the data
  bracket. (`tokio` is used only for the REST control plane.)

### The one consequential asymmetry

The send-side timestamp fires *before* the `send` syscall returns and
*before* the kernel begins egress processing. So the sender's
**kernel send-path** is **inside** OWD, attributed to "the network." On
the receiving side, the timestamp fires *after* the kernel has fully
processed reception, so the receiver's **kernel receive-path** is also
**inside** OWD.

On a modern Linux server this OS contribution is ~10–50 µs per side and
stable. On Android it is ~50–500 µs per side and more variable (modem
IPC over PCIe/shared memory is the largest term). Section 7 catalogues
the service-layer effects that can move this floor.

---

## 2. The dual-clock rule

Every timestamp in this system carries **two** values, sampled together:

| Field     | Domain                                  | Authoritative for                                   |
|-----------|-----------------------------------------|-----------------------------------------------------|
| `mono_ns` | `CLOCK_MONOTONIC` — local, no leaps     | ordering, intervals, jitter, inter-arrival, bursts  |
| `utc_ns`  | `CLOCK_REALTIME` — wall-clock UTC       | cross-node correlation, OWD, multi-device alignment |

The two are never collapsed. Mono is *always* used for local-only
quantities — it cannot jump and cannot run backwards. UTC is the *only*
clock that can be compared across two nodes — it can jump (NTP step) and
skew (drift, suspend), so any quantity built from it carries an uncertainty
bound.

The consequence in practice: every metric in this platform falls into one
of two camps.

- **Mono-only metrics** (jitter, inter-arrival, RTT, burst structure, loss
  gaps, throughput) involve one clock on one node and therefore *have no
  clock-uncertainty term at all*. These are the highest-confidence outputs
  of the platform.
- **UTC-bridged metrics** (OWD uplink, OWD downlink, cross-node
  correlation) involve UTC on both nodes and therefore carry an
  uncertainty equal to the sum of each node's UTC-discipline error. These
  are useful but always reported with bounds.

---

## 3. One implementation, both nodes

`fieldtest-core` (the `core/` crate) is what stamps every log row on
**both** nodes. Because `core/` is cross-compiled to Android via
`cargo-ndk`, `Timestamp::now()` runs exactly the same code on:

- **Linux server** — `clock_gettime` against the host kernel
- **Android Logger** — `clock_gettime` against the Linux kernel that
  Android runs on (bionic exposes the POSIX primitives identically)

The implementation is small enough to quote (`core/src/timestamp.rs`):

```rust
pub struct Timestamp { pub mono_ns: u64, pub utc_ns: u64 }

impl Timestamp {
    pub fn now() -> Self {
        Self {
            mono_ns: clock_gettime(CLOCK_MONOTONIC),  // sampled first
            utc_ns:  clock_gettime(CLOCK_REALTIME),   // sampled immediately after
        }
    }
}
```

This is the single largest symmetry guarantee in the platform. There is no
"Java timestamp" to reconcile with a "Rust timestamp." The Java shell on
the Logger app never calls `System.currentTimeMillis()` or
`System.nanoTime()` for anything measurement-relevant — it hands control
to the Rust core and the core stamps.

The `realtime_udp` packet carries `(send_mono_ns, send_utc_ns)` in its
payload, both sampled by the same `Timestamp::now()` call at send time,
so the receiver gets the sender's view of *both* clocks atomically.

---

## 4. When the stopwatch starts and stops — the exact code points

For every flow type, the timestamping pattern is identical and visible in
the source:

| Flow side          | When `Timestamp::now()` is called                                 | What's inside the bracket                            |
|--------------------|-------------------------------------------------------------------|------------------------------------------------------|
| Sender             | **immediately before** the `socket.send` call                     | sender serialisation (sub-µs), sender kernel/NIC TX  |
| Receiver           | **immediately the line after** `socket.recv` / `recvmsg` returns  | receiver kernel/NIC RX + scheduler wake              |
| Probe echo (server)| recv timestamp first line after recv; send timestamp last line before send_to | echo turnaround in user space (~100 ns)              |

A representative fragment (`server/data-generator/src/flow.rs`,
probe echo loop):

```rust
match socket.recv_from(&mut buf) {
    Ok((n, peer)) => {
        let recv_ts = Timestamp::now();       // ◄ first line after recv returns
        …
        let send_ts = Timestamp::now();       // ◄ last line before send_to
        if socket.send_to(&buf[..n], peer).is_ok() {
            …
```

This pattern is the same in `core/src/ue.rs` for the UE side, in
`realtime_send`, `realtime_recv`, the bulk paths, and `dscp_probe_ue`.
No exceptions.

### What the embedded packet timestamp means

The sender's `Timestamp::now()` is captured *before* packet serialisation
and *before* `send`. So the value embedded in the wire packet
(`RealtimePacket::send_mono_ns`, `send_utc_ns`) is *the moment user-space
decided to send* — not the moment the NIC actually put bits on the wire.
For analysis this is the right semantic: it lines up with the equivalent
moment on the receiving node and is reproducible.

### Probe RTT vs OWD: why probe RTT is the gold standard

The `probe` flow's RTT is computed on the **UE alone**, using **mono only**:

```
RTT_probe = recv_ts.mono_ns − send_ts.mono_ns   (one node, one clock)
```

No UTC is involved, so no clock-discipline uncertainty contaminates the
measurement. The full app-to-app round trip — UE kernel TX, network in
both directions, server echo turnaround, UE kernel RX — sits inside that
single difference. RTT_probe is the most trustworthy latency number this
platform produces.

OWD (uplink + downlink, see §10) is more *useful* (you get each direction
separately) but less *clean* (it depends on UTC discipline on both nodes).
The cross-check is documented in §10.3.

---

## 5. Symmetric implementation across the codebase

The same pattern is enforced everywhere a timestamp is sampled:

- One `Timestamp::now()` call → one struct → both fields populated.
- No code path samples `mono` without also sampling `utc`.
- No code path uses `chrono` or `Instant::now()` for anything that ends up
  in a log row.
- The Java Logger shell never produces a timestamp that lands in
  `packets.parquet`, `radio.parquet`, `gps.parquet`, or `clock.parquet`.

The result is that any line you can attribute to "the platform stamped a
time" went through exactly one function, in one source file, compiled
once for two targets.

---

## 6. Cost of the timing mechanism itself

`clock_gettime` for both `CLOCK_MONOTONIC` and `CLOCK_REALTIME` is served
through the **vDSO** on every Linux build — including bionic on Android —
which means it does not transition into the kernel. Measured cost:

- single `clock_gettime` ≈ 20–50 ns on x86_64 server, ~30–80 ns on AArch64
- one `Timestamp::now()` (two calls) ≈ 50–150 ns total
- asymmetry between the two reads ≈ same as one call, with no allocation
  or preemption point in between
- preemption between the two reads is *possible* but rare (an unlucky
  scheduler tick can split the pair by ~10 µs). This shows up as a
  bounded, occasional inflation of OWD by µs; it does not bias the mean.

These numbers sit ~3–4 orders of magnitude below the network delay floor.
The timing mechanism is not a meaningful contributor to any platform
metric and does not alias into nearby measurements.

---

## 7. Service-layer impact on what we measure

The data plane is deliberately structured so the service layer (REST,
HTTP, JNI, tokio executors, foreground services) sits *outside* the
measurement bracket once a test is running. But there are paths through
which it can still affect the OS-noise floor and the stability of the
measurement.

### 7.1 Android Logger

The Android service layer affects timing in three ways: it determines
*whether the app gets to run at all*, *what radio state the modem is in*,
and *what kind of CPU contention the data threads face*.

| Mechanism                          | Impact on measurement                                                                                                                                                            |
|------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Foreground service                 | Required. Prevents Android from killing the app during a test. The flow threads inherit the foreground priority class; CPU is granted promptly even under thermal/power pressure. |
| Doze / App Standby                 | Bypassed because the FGS is alive. Without an FGS, deep-Doze would freeze the app, breaking measurement entirely.                                                                |
| RRC state (CONNECTED / IDLE / INACTIVE) | We force `RRC_CONNECTED active` via continuous traffic so transition latency (50–500 ms idle→connected) does not appear inside OWD. DRX is configured out of scope for MVP — its effect would otherwise add 10–100 ms of variable delay per packet. |
| ART garbage collection             | Not in the bracket. Hot path is pure Rust; the Java shell does nothing per-packet. GC pauses in the FGS thread do not interrupt the Rust flow threads.                            |
| Thermal throttling                 | Can lower CPU frequency. Does not change `clock_gettime` accuracy but can extend the OS recv-path latency by tens of µs. Visible in `clock.parquet`-bracketed analysis as a stable shift, not as jitter. |
| Modem firmware (PDCP/RLC/MAC)      | Inside OWD. Modem IPC over PCIe / shared memory contributes 50–500 µs to the OS-noise floor and is the dominant local term on Android.                                           |
| Telephony / Location subsystems    | Not in the data bracket but sample radio + GPS into `radio.parquet` and `gps.parquet`. Their sampling rate is set by `logging_config`; they consume CPU on background threads but do not interrupt the flow threads. |

The headline rule: **on Android, the foreground service exists to make
measurement *possible*; the choice of Rust + raw threads makes
measurement *trustworthy***.

### 7.2 Linux server

The server's service layer is process-level: `server-main`, `test-control`,
and `data-generator` are three separate processes. Once a test is
prepared, the data plane lives entirely in `data-generator`'s per-flow
threads. `server-main` and `test-control` continue running but do not
sit in the data path.

| Mechanism                          | Impact on measurement                                                                                                                                                                |
|------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| tokio runtime                      | Used by the REST control plane only. The flow threads in `data-generator/src/flow.rs` are raw `std::thread::spawn` — no async executor in the data bracket, no future polling latency.|
| SPSC ring (`rtrb`)                 | Decouples the network-touching thread from the Parquet writer. The flow thread `push`es a row in O(1), wait-free; the writer thread drains and serialises off the hot path. No mutex, no allocation, no syscall on the push side. |
| Process / thread scheduling        | Standard Linux CFS, default priorities. The flow thread spends most of its life blocked in `recvmsg`; wake-up latency is the dominant OS-noise term on the server side (~5–20 µs typical, more under load). |
| NIC interrupt coalescing / NAPI    | Inside OWD. Coalescing trades latency for throughput; under high RX rate, NAPI batching adds tens of µs.                                                                              |
| Other processes on the host        | Co-tenancy can preempt the flow thread. For accurate runs the server should be otherwise idle. Visible as inflated tail OWD with stable median.                                       |
| `IP_RECVTOS` cmsg parsing (dscp_probe only) | Inside OWD. `recvmsg` ancillary-data walk is sub-µs; negligible.                                                                                                                      |
| SQLite / control-plane I/O         | Out of the bracket entirely. Runs in different processes.                                                                                                                            |

The data-generator's design intent is "one thread, one socket, one ring;
nothing else." Any addition to this layer must demonstrably not add a
syscall, lock, or allocation per packet.

### 7.3 Where the service layer *does* affect a test

- **Test startup latency** (operator clicks Run → first packet) is service-
  layer-dominated: REST hop, command enqueue, device poll interval,
  RRC wake-up. This is not part of any per-packet metric.
- **Effective logging config arrival**: a `SET_LOGGING_CONFIG` push travels
  via the command queue; the device picks it up on its next poll. The
  `logging_config_version` column on each `packets.parquet` row lets
  analysis confirm which config a measurement was taken under.
- **Clock-calibration cadence**: `clock_calibration_interval_sec` is part
  of the effective logging config (§9). Lower interval = tighter OWD
  uncertainty bounds; higher = less log volume.

---

## 8. What disciplines UTC on each node

Mono needs nothing — it's a hardware counter monotonised by the kernel.
UTC has to be disciplined by an external reference. Each node does this
independently; the platform records what discipline they had at every
calibration snapshot.

### 8.1 Linux server

- **NTP discipline** via chrony (recommended) or systemd-timesyncd,
  pointed at a stable stratum-1 or stratum-2 source.
- **Optional hardware**: a GPS-disciplined PPS source gives sub-µs
  accuracy and is recommended for OWD work where network delay is itself
  in the µs range.
- **Recording**: `server-main` periodically samples `chronyc tracking`
  and writes a `ClockRow` to `server/clock.parquet` with
  `source = "chrony"` and the reported root delay + dispersion as
  `uncertainty_ns`.

### 8.2 Android Logger

UTC discipline on Android is *less controllable* and *less accurate*. The
Logger uses what's available, ranked best to worst:

1. **GPS time** when a fix is locked and the underlying location subsystem
   exposes the GNSS clock. Potentially better than NTP-disciplined
   `CLOCK_REALTIME`. Recorded as `source = "gps"`,
   `uncertainty_ns ≈ tens of µs`.
2. **NITZ / network-provided time** from the mobile operator. Reasonable
   on most networks but operator-dependent. Recorded as `source = "ntp"`
   (the system clock's nominal source).
3. **Android system NTP** — AOSP's time service, normally tens of ms
   accurate. Recorded as `source = "system"`,
   `uncertainty_ns ≈ ~100 ms`.

### 8.3 The platform does not adjust clocks

We never push commands to re-sync a device's clock. We never rewrite
timestamps in logs. Each node *observes* its own state and *records* it.
Analysis attaches uncertainty bounds to OWD using those observations; it
does not retroactively correct timestamps. The raw samples are preserved
forever.

---

## 9. `clock.parquet` — the calibration record

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

`mono_ns` is the join key: it pins each calibration snapshot to a point
on the local monotonic timeline, which packet events also live on.
Analysis interpolates `(uncertainty_ns, estimated_offset_ns)` against the
two `ClockRow`s bracketing each packet event's `mono_ns`.

Both nodes write this. **The server's uncertainty matters as much as the
mobile's.** If chrony reports ±2 ms and the mobile is on GPS with ±50 µs,
OWD uncertainty is dominated by the server, not by the supposedly
"less accurate" device.

---

## 10. End-to-end OWD math

### 10.1 Downlink (`FromServer`)

The server send-stamps a packet, the mobile receive-stamps it.

```
OWD_dl       = mobile.recv.utc_ns − server.send.utc_ns
uncertainty  = U(server.send) + U(mobile.recv) + ε_drift
```

`server.send.utc_ns` travels in the packet payload
(`RealtimePacket::send_utc_ns`). `mobile.recv.utc_ns` is captured by
`Timestamp::now()` immediately after `recv` returns.

The uncertainty term sums:

- `U(server.send)` — the server's UTC-discipline uncertainty at the
  `clock.parquet` row whose `mono_ns` brackets the *send* event
- `U(mobile.recv)` — same on the mobile, bracketing the *receive* event
- `ε_drift` — a small drift term `|skew_rate| × Δt` between the
  calibration sample and the packet event, negligible when calibration is
  frequent

A negative OWD is *possible*: it means the uncertainty envelope crosses
zero — network delay is at or below the joint clock-accuracy floor. The
correct interpretation is "OWD ≲ uncertainty", not "OWD < 0". Analysis
clips and flags these rows.

### 10.2 Uplink (`ToServer`)

Mirror of §10.1, with sender and receiver swapped:

```
OWD_ul       = server.recv.utc_ns − mobile.send.utc_ns
uncertainty  = U(mobile.send) + U(server.recv) + ε_drift
```

### 10.3 RTT vs OWD — the integrity check

Two latency signals are available and they have different uses:

- **`probe` echo RTT** (mono-only, one node): no clock-uncertainty term;
  the cleanest end-to-end latency the platform can produce.
- **OWD halves** (UTC, two nodes): `OWD_dl + OWD_ul` should equal RTT
  modulo path asymmetry.

The difference `RTT_probe − (OWD_dl + OWD_ul)` is the **clock-skew
residual** for that pair of samples. If it exceeds `2 × max(uncertainty)`,
either:

- the UTC discipline on one or both nodes is worse than what `clock.parquet`
  reported (a calibration bug or a missed NTP step), or
- a clock jumped between calibration snapshots (look for a step in
  `estimated_offset_ns`)

This cross-check is the single most useful integrity test in the platform.
If it passes, every UTC-bridged number in the run is trustworthy to within
the reported bounds. If it fails, OWD halves cannot be trusted and the
analysis falls back to RTT-only.

---

## 11. Within-flow signals — UTC not required

Most of the analytic value of the platform lives entirely in monotonic
space and never touches UTC:

- **Inter-arrival jitter** = `mono_recv[i] − mono_recv[i−1]`.
- **Expected vs actual interval**: `actual_interval_ns` column in
  `packets.parquet` is `mono_recv − mono_prev_recv`;
  `expected_interval_ns` is `1e9 / rate_pps`. Comparison is mono-space.
- **Burst detection**, **packet ordering**, **loss-gap timing** — all
  mono-only.
- **Throughput** (bulk_tcp): `Σ payload_size / Δmono_ns` per node.
- **DSCP transparency**: pure equality test on `dscp_sent` vs `dscp_recv`;
  no clock involved at all.

This is why mono is "authoritative" and UTC is "for correlation": the
most important per-flow signals are computed without any cross-node clock,
and so are immune to UTC drift entirely.

---

## 12. Failure modes and what we do about them

| Failure                                  | Detection                                          | Effect                                                                       |
|------------------------------------------|----------------------------------------------------|------------------------------------------------------------------------------|
| Server NTP outage (drift grows)          | chrony source state → `clock.parquet`              | `uncertainty_ns` grows; OWD rows widen bounds; mono-only metrics unaffected  |
| Phone loses GPS                          | `source` falls from `gps` to `system`              | UTC uncertainty jumps from ~50 µs to ~100 ms; visible in plots               |
| NTP step (UTC jumps)                     | `utc_ns` discontinuity not matched by `mono_ns`    | analysis splits the trace at the step; mono-based metrics keep working       |
| Foreground service killed (Android)      | `mono_ns` gap with no rows + FGS state log         | usually catastrophic — test data ends; the FGS exists precisely to prevent  |
| Thermal CPU throttle on phone            | OS recv-path latency floor shifts upward           | stable shift in OWD median, no UTC consequence                               |
| NIC interrupt storm on server            | OWD tail balloons; mono jitter visible             | identifiable in plots; usually a noisy-neighbour signal                      |
| Mobile clock unsynced (no GPS, no NTP)   | first `clock.parquet` row has `source = "system"` only | analysis warns "OWD may be unbounded for this session"; RTT_probe still good |

The platform never silently *fixes* a bad clock or a quiet OS noise
event. The raw sample is recorded, the calibration row records the
node's state, the uncertainty inflates, and analysis decides what to keep.

---

## 13. System-level integrity checks

When `analysis/` opens a test it runs:

1. **Monotonic ordering** — every `packets.parquet` row's `mono_ns` ≥
   prior row's. Violation = data corruption (we have never observed
   this; the type system prevents it on the write side).
2. **Calibration coverage** — at least one `clock.parquet` row brackets
   every `packets.parquet` segment on each node. Missing → analysis
   degrades that segment to RTT-only.
3. **RTT vs OWD agreement** (§10.3) — `RTT_probe` vs `OWD_dl + OWD_ul`
   should agree within `2 × max(uncertainty)`. Disagreement is a
   clock-quality flag, not a measurement error.
4. **Physical floor** — OWD must exceed `distance / c` if `gps.parquet`
   gives a position. Smoke check for catastrophic clock errors (e.g.
   wrong timezone applied to a sample).

---

## 14. Recipe for a high-accuracy run

If you want OWD numbers tight enough to argue about radio-scheduling
behaviour (single-digit ms or below):

- **Server**: chrony pointed at stratum-1 with a PPS-disciplined GPS
  receiver; `chronyc tracking` reporting sub-100-µs offset; otherwise
  idle host; NIC interrupt affinity pinned away from the cores the
  flow threads run on.
- **Mobile**: clear sky-view, location services on, GPS lock acquired
  *before* the test starts (confirm `source = "gps"` in the first
  `clock.parquet` row); thermal headroom; foreground service confirmed
  via system UI.
- **Logging config**: `clock_calibration_interval_sec = 5` or `10`
  (more frequent calibration = tighter interpolation).
- **Test plan**: include a `probe` flow alongside the `realtime_udp`
  flows so the RTT-vs-OWD sanity check (§10.3) is available.

For "phone in pocket, server on the office LAN", chrony + system NTP
gets you to the tens-of-milliseconds range — entirely adequate for
jitter, loss, throughput, handover detection, and DSCP transparency,
none of which need tight UTC.

---

## 15. Reference files

| File                                                     | Role                                              |
|----------------------------------------------------------|---------------------------------------------------|
| `core/src/timestamp.rs`                                  | `Timestamp::now()` — both platforms               |
| `core/src/aux_log.rs`                                    | `ClockRow` + Parquet writer                       |
| `core/src/ue.rs`                                         | UE-side send/recv timestamp call sites            |
| `server/data-generator/src/flow.rs`                      | server-side send/recv timestamp call sites        |
| `contracts/parquet/clock.parquet.yaml`                   | committed schema                                  |
| `contracts/parquet/packets.parquet.yaml`                 | committed schema (`actual_interval_ns`, embedded sender ts) |
| `contracts/schemas/logging_config.schema.json`           | `clock_calibration_interval_sec` field            |
| `contracts/packets/realtime_v1.md`                       | wire packet with embedded `send_mono_ns`, `send_utc_ns` |
| `planning/5_g_field_test_platform_architecture_spec.md` §3.2 | short rule                                        |
| `planning/5_g_field_test_platform_architecture_spec.md` §11  | OWD math (older statement)                        |
| `analysis/fieldtest_analysis/owd.py`                     | the implementation of §10                         |

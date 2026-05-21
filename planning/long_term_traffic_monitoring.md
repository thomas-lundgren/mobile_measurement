# Long-Term Direction: Traffic Logging & Latency Monitoring

> Date: 2026-05-21
> Status: Strategic direction — not yet in MVP scope
> Purpose: steer current design so Phase 0–5 foundations don't block later growth

## Vision

Beyond the controlled test flows (UDP probe, realtime UDP, bulk TCP), the platform should eventually log **real traffic from other apps** on the mobile device and monitor **latency to arbitrary destinations** via background ping. This turns the logger from a "test instrument" into a "network quality probe" that works continuously.

## Key Requirements

| Requirement | Constraint |
|-------------|-----------|
| Log traffic from running apps | Must work without root (VpnService-based capture) |
| Selective IP capture | Server-configurable filter list; only selected IPs/flows are logged |
| Latency polling | ICMP ping to configured targets, slow cadence (e.g., 1/min) |
| Efficiency | Must not measurably slow the device or drain battery |
| Modularity | Logger core must accept multiple data sources without coupling to any one |
| Schema growth | New tables/columns added without breaking existing readers |

## Current Design vs. Long-Term Goal

### What stays (foundations are correct)

- **Shared Rust core via JNI** — the performance-critical path (packet encoding, timestamping, Parquet writing) stays in Rust
- **Segmented Parquet writer** — crash-safe, upload-friendly, proven on Pixel 9 Pro
- **Dual-clock timestamps** — essential for correlating captured traffic with radio metrics
- **Server-configured logging** (`logging_config`) — the server already decides what to log; this extends naturally to traffic filters and ping targets
- **Logging-first + local storage** — traffic capture must never depend on upload availability
- **Capability-tolerant clients** — some devices may not support VpnService or may have restrictions

### What evolves (extension points to build now)

| Current (MVP) | Future Extension |
|---------------|------------------|
| Single-table `packets.parquet` | Multi-table logger: `packets`, `traffic`, `pings`, `events` |
| Fixed schema per table | Tolerant reader + additive schema evolution per table |
| Test-driven flow lifecycle | Always-on background monitor + test-driven flows coexist |
| One `CoreState` per test | Long-lived `CoreState` with pluggable input streams |
| Server sends `RUN_TEST` | Server also sends `SET_MONITOR_CONFIG` (IP filters, ping targets, sampling rate) |

## Proposed Architecture Evolution

### 1. Modular Logger Core

Refactor `fieldtest-core` so the Parquet writer is decoupled from the traffic engine:

```
core/src/
  logger/
    segmented_writer.rs   # generic: any RecordBatch → segmented Parquet
    schema_registry.rs    # maps table name → Arrow Schema + version
    batch_buffer.rs       # ring buffer + flush coordinator
  engines/
    probe.rs              # UDP probe (existing)
    realtime_udp.rs       # realtime flow (existing)
    bulk_tcp.rs           # TCP flow (existing)
    traffic_capture.rs    # NEW: VpnService packet ingestion
    ping_scheduler.rs     # NEW: ICMP latency polling
  jni/
    lib.rs                # JNI exports (thin)
```

The `SegmentedWriter` becomes table-agnostic:
```rust
pub struct SegmentedWriter {
    table_name: String,
    schema_version: u32,
    // ... same segmentation logic
}
```

### 2. Traffic Capture Module

**Android side (Java/Kotlin):**
- `TrafficCaptureService` extends `VpnService`
- Reads IP packets from the VPN tunnel file descriptor
- Parses headers (IPv4/IPv6, TCP/UDP), extracts 5-tuple + size + timestamp
- Feeds a lightweight ring buffer consumed by the Rust core via JNI

**Rust side (`traffic_capture.rs`):**
- Consumes raw packet metadata from the ring buffer
- Samples according to `capture_config` (server-provided IP filter list, sampling rate)
- Writes to `traffic.parquet`

**IP Filter Configuration (from server):**
```json
{
  "capture_config": {
    "enabled": true,
    "sampling_rate": 1.0,
    "filter_mode": "include",
    "targets": [
      {"ip": "142.250.74.46", "label": "youtube"},
      {"ip": "31.13.72.36", "label": "facebook"},
      {"cidr": "104.16.0.0/12", "label": "cloudflare"}
    ],
    "max_bytes_per_day": 536870912
  }
}
```

**Schema: `traffic.parquet`**
```
timestamp_mono_ns: u64
timestamp_utc_ns: u64
src_ip: utf8
dst_ip: utf8
src_port: u32
dst_port: u32
protocol: utf8  # tcp / udp / icmp
packet_size: u32
direction: utf8 # uplink / downlink
app_uid: int32  # Android app UID (optional)
interface: utf8  # rmnet0 / wlan0 (optional)
```

### 3. Ping / Latency Polling Module

**Android side:**
- `PingScheduler` is a coroutine/thread that wakes at configured intervals
- Uses Android `Runtime.exec("ping -c 1 -W 5 <target>")` or ICMP socket (if permitted)
- Alternatively: use the Rust core to send ICMP via raw sockets (requires `CAP_NET_RAW` or root — not baseline)
- **Recommended:** Java-side ping via `ProcessBuilder`, feeding results to Rust logger

**Rust side (`ping_scheduler.rs`):**
- Receives `(target_ip, label, rtt_ms, success, timestamp)` from Java
- Writes to `pings.parquet`

**Ping Configuration (from server):**
```json
{
  "ping_config": {
    "enabled": true,
    "interval_sec": 60,
    "targets": [
      {"ip": "1.1.1.1", "label": "cloudflare"},
      {"ip": "8.8.8.8", "label": "google-dns"},
      {"ip": "142.250.74.46", "label": "youtube"}
    ]
  }
}
```

**Schema: `pings.parquet`**
```
timestamp_mono_ns: u64
timestamp_utc_ns: u64
target_ip: utf8
label: utf8
rtt_ms: float64  # null if timeout
success: boolean
seq: u32
interface: utf8   # rmnet / wlan
```

### 4. Efficiency Design

| Concern | Mitigation |
|---------|-----------|
| CPU (packet parsing) | Java does header parsing (fast enough for Mbps); Rust only sees metadata tuples |
| CPU (Parquet) | Batch writes: accumulate 1000 rows or 5 seconds, whichever comes first |
| Memory | Ring buffer between Java capture and Rust logger (bounded, e.g., 10k entries) |
| Battery | VpnService only active when capture is enabled; sampling rate < 1.0 reduces load |
| Storage | Per-day byte cap in config; oldest segments auto-deleted when cap exceeded |
| Upload | Traffic tables compress well (repetitive IPs); zstd stays; upload deferred to Wi-Fi/charging |

### 5. Schema Evolution Strategy

All tables use the same forward-compatible rules as the MVP:

- `schema_version` per table (not global)
- Additive-only: new columns may appear, existing columns never change meaning
- Tolerant reader: missing columns → `null`, unknown columns → ignored
- Server announces supported schema versions at registration; logger writes the highest version it supports

New tables (`traffic`, `pings`) are added without bumping the global schema version. Analysis tools discover tables by listing the directory.

### 6. Log Directory Layout (Extended)

```text
session_2026_05_21/
  test_001/
    header.json
    gps.parquet
    radio.parquet
    mobile/
      packets.parquet      # controlled test flows
      traffic.parquet      # captured app traffic
      pings.parquet        # latency polls
      events.parquet
      clock.parquet
      summary.json
    server/
      packets.parquet
      events.parquet
      clock.parquet
      summary.json
```

`traffic.parquet` and `pings.parquet` live under `mobile/` because they are mobile-only.

### 7. Android Lifecycle (Extended)

```text
INITIALIZING → REGISTERING → IDLE
    ↓
WAITING_FOR_TEST → PREPARING → RUNNING_TEST → COOLDOWN → UPLOAD_PENDING → UPLOADING
    ↑
MONITORING (always-on background state, parallel to IDLE/WAITING)
```

`MONITORING` is a new orthogonal state:
- Active when `capture_config.enabled == true` or `ping_config.enabled == true`
- Runs in the same foreground service as tests, but with a different notification
- Does not block test execution; test flows and monitoring can run concurrently
- The Rust core holds two `SegmentedWriter` instances: one for test flows, one for monitor data

## Impact on Current MVP Plan

### Phase 0 — Contracts (add these items)

- `CaptureConfig` schema (IP filters, sampling rate, byte cap)
- `PingConfig` schema (targets, interval)
- `traffic.parquet` schema definition
- `pings.parquet` schema definition
- Table-name registry (enum of known tables, extensible)

### Phase 1 — Server Skeleton (no change)

Server framework is unaffected; monitor configs are just new REST endpoints:
- `POST /api/v1/devices/{device_id}/monitor-config`
- `GET /api/v1/devices/{device_id}/monitor-config`

### Phase 2 — Android Logger (MVP) (minor extension)

- Keep `CoreState` as a long-lived singleton (not per-test)
- Add `start_monitor(capture_config, ping_config)` and `stop_monitor()` JNI calls
- VpnService plumbing is out of MVP scope but the JNI interface should reserve the slots

### Phase 3 — Controller (no change)

### Phase 4 — Analysis (new loaders)

- Tolerant `traffic.parquet` reader
- Tolerant `pings.parquet` reader
- Cross-correlation: traffic spikes vs radio metrics vs ping RTT

### Phase 5 — Hardening (add)

- Battery/storage chaos tests for always-on monitor
- CGNAT/IPv6 handover tests for ping targets

## Open Questions (to resolve before implementation)

1. **VpnService vs rooted capture?** VpnService is the baseline (no root). Root-only features (e.g., app name resolution without VPN) are marked as extended capability.
2. **ICMP on Android without root?** `ProcessBuilder` with `ping` works on all devices. Raw ICMP sockets require `CAP_NET_RAW`. The Java-side ping is the baseline.
3. **Traffic attribution without VpnService?** On baseline devices, we only see raw 5-tuples. App-UID mapping requires VpnService or root.
4. **Sampling strategy?** Per-flow sampling (all packets of a selected flow) vs per-packet sampling (random 1-in-N across all traffic). Per-flow is recommended for causal analysis.
5. **Privacy / GDPR?** Captured traffic metadata (IPs, ports, sizes) is PII-adjacent. The server must configure filters narrowly; the device must not log payload content. This needs a privacy policy section in contracts.

## Summary

The current MVP architecture (shared Rust core, segmented Parquet, dual clocks, server-configured logging) is the right foundation. The growth path is:

1. **Now (MVP):** Decouple the Parquet writer from the traffic engine → generic `SegmentedWriter`
2. **Next (Phase 2+):** Add `traffic_capture` and `ping_scheduler` as new input streams to the same writer
3. **Later:** Add VpnService-based capture and always-on monitor lifecycle

Nothing in the MVP needs to be rewritten. The contracts just need to reserve the schema slots.

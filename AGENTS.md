# 5G Field Test Platform — Agent Guide

> This file is written for AI coding agents.
> **For the current implementation status, see `STATUS.md`.** This file covers conventions, stack, and architecture; `STATUS.md` covers what is built, tested, and pending.
**For long-term direction (traffic capture, latency monitoring), see `planning/long_term_traffic_monitoring.md`.
> Expect the reader to know nothing about the project.

---

## Project Overview

This is a **distributed mobile network measurement platform** for real-world 5G experiments using Android devices moving through live mobile networks while controlled traffic is generated from one or more Linux servers.

### Analysis Goals

- Cross-layer latency analysis
- Mobility analysis
- Packet timing analysis
- Radio quality correlation
- Congestion and queueing analysis
- Realtime traffic behavior
- Cross-flow interference
- Head-of-line-like effects in mobile networks

### System Components

1. **Linux Server** — central orchestration; implements traffic generator and receiver for all flow directions
2. **Android Logger App** — autonomous measurement client; implements traffic generator and receiver for all flow directions. The UE always initiates the connection to the configured server port.
3. **Android Controller App** — remote control and monitoring UI

### Current State

As of the latest project state, the codebase consists solely of a planning document:

- `planning/5_g_field_test_platform_architecture_spec.md` — complete architecture and functional specification

**Implementation has started.** The Stage A JNI viability spike is complete and the core library exists.
- `core/` — shared Rust engine+logger (native + JNI `cdylib`)
- `server/` — server skeleton placeholder
- `spikes/stage-a-harness/` — Stage A desktop-JVM test harness + peer echo server
- See `spikes/stage-a-harness/FINDINGS.md` for results.

---

## Planned Technology Stack

### Linux Server (Recommended: Rust)

| Library   | Purpose            |
|-----------|--------------------|
| `axum`    | REST API           |
| `tokio`   | Async runtime      |
| `serde`   | Serialization      |
| `sqlx`    | Database access    |
| `tracing` | Logging            |
| `uuid`    | Identifiers        |
| `chrono`  | Time handling      |
| `jni`     | JNI bindings       |
| `arrow`   | Arrow in-memory    |
| `parquet` | Parquet writer     |
| `libc`    | Raw clock_gettime  |

### Android Apps

- Logger app: foreground service for measurements
- Controller app: remote-control UI
- Language: **Java**. The Logger's measurement engine + log writer are a **shared Rust core linked via JNI** — the same engine the server runs, so the two nodes are symmetric by construction. Java owns lifecycle, GPS, radio, permissions, and the control/REST client. See `planning/development_guide.md` §5.

### Analysis Tools

- Python scripts for post-processing
- Route plots, RTT plots, radio correlation, packet timing analysis

---

## Planned Architecture

### Server Processes

```text
server-main        — central control plane (device registry, REST API, upload manager)
test-control       — test orchestration, scheduler, port allocation, state machines
data-generator(s)  — UDP probe service, realtime UDP service, bulk TCP upload/download
```

### Communication Flow

```text
Controller App
    ↓
server-main
    ↓
test-control
    ↓
data-generator(s)  (server-side)

Android Logger Clients
    ↕
server-main
    ↕
data-generators  (server-side or mobile-side)
```

### Android Logger App Components (Planned)

```text
logger-app
  ├── server connection client
  ├── command poller
  ├── test runner
  ├── traffic generator client
  ├── traffic generator / receiver  (Realtime UDP / Bulk TCP, active role depends on flow direction)
  ├── packet logger
  ├── radio logger
  ├── GPS logger
  ├── clock calibration
  ├── upload manager
  └── local storage
```

---

## Core Design Principles

### 1. Logging First

Measurements must never depend on upload availability.

- All data is first written locally on the mobile device.
- Upload is always secondary.
- Upload failures must never interrupt measurements.
- Tests continue during temporary network loss.

### 2. Dual Timestamp Model

All timestamps use two parallel clock domains:

| Field       | Purpose                              |
|-------------|--------------------------------------|
| `t_mono_ns` | Local monotonic ordering             |
| `t_utc_ns`  | Cross-device/server correlation      |

- Monotonic timestamps are authoritative for packet ordering, jitter, inter-arrival timing, burst detection, and internal event sequencing.
- UTC timestamps are used for one-way delay estimation, mobile/server correlation, multi-device alignment, and session analysis.

### 3. Forward-Compatible Logging

Every log file contains:

- `schema_version`
- `protocol_version`
- `app_version`
- `device_id`
- `session_id`
- `test_id`

Rules:

- New fields may be added by any device (extensible log format).
- Existing fields must not change meaning.
- Missing fields are interpreted as `null`.
- Clients and analysis tools must tolerate unknown fields.
- **The server configures which features to log** based on device capabilities, test plan, and operator policy.

### 4. Capability-Tolerant Clients & Extensible Log Format

Different Android devices expose different radio APIs. The **Google Pixel 9 Pro** is the reference device, but the log format is **extensible** — more capable devices may log additional fields.

- All fields in all log files are **optional** at the row level.
- New fields may be added by any device without changing the schema version.
- Missing fields are interpreted as `null` by analysis tools.
- Analysis tools must tolerate unknown fields (forward compatibility).
- The server stores all fields present in uploaded Parquet files, even if they exceed the baseline schema.
- **Reference device: Google Pixel 9 Pro.** DRX, 5QI/QFI, and RRC state transitions are not available via public APIs on this device.
- Handovers are derived retrospectively from PCI/NCI changes for baseline devices; explicit HO events may be logged by extended-capability devices.

---

### 5. Symmetric Peer Nodes

The server and the mobile (UE) are symmetric peers: each can act as **generator/sender**, **receiver**, **loopback/echo**, and **logger**, and both participate in measurement and produce log files. Flow *direction* only selects which side emits payload; the only fixed asymmetry is that the UE always initiates the connection. A **test** aggregates both nodes' recordings under one `test_id` and may hold many flows; GPS and the radio metric trace are recorded once per test (mobile-only), shared across all flows.

---

## Planned Log Structure

A test aggregates both nodes' recordings under one `test_id`. Each node writes its per-node files into a `mobile/` or `server/` subfolder; the mobile-only GPS and radio traces (one each per test, regardless of flow count) sit at the test root.

```text
session_2026_05_20/
  test_001/
    header.json              # test + flow config, shared
    gps.parquet              # mobile only — one per test
    radio.parquet            # mobile only ("mobile metric trace") — one per test
    mobile/
      packets.parquet        # all flows, mobile's view
      events.parquet
      clock.parquet
      summary.json
    server/
      packets.parquet        # all flows, server's view
      events.parquet
      clock.parquet
      summary.json
```

### File Purposes

- `header.json` — schema version, app version, protocol version, device metadata, test configuration, flow configuration
- `packets.parquet` — packet metadata (timestamps, flow IDs, protocol, ports, sizes, sequence numbers); **one per node** (`mobile/`, `server/`)
- `radio.parquet` — radio metrics (PCI, NCI, TAC, NRARFCN, RSRP, RSRQ, SINR, LTE info if NSA, neighbor cells, derived handover flags); **mobile-only ("mobile metric trace"), one per test** shared by all flows
- `gps.parquet` — GPS snapshots (latitude, longitude, speed, heading, accuracy); **mobile-only, one per test** shared by all flows
- `clock.parquet` — clock calibration snapshots (monotonic timestamp, UTC timestamp, source, uncertainty, estimated offset)
- `events.parquet` — system events + derived events (HO from PCI changes, coverage gaps, band changes)
- `summary.json` — compact test summary (RTT percentiles, one-way latency percentiles, loss, jitter, inter-packet timing, radio statistics, handover count, neighbor cell stats, GPS start/end, PCI changes, upload state)

---

## Planned REST API Endpoints (server-main)

| Method | Endpoint                                | Purpose                  |
|--------|-----------------------------------------|--------------------------|
| POST   | `/api/v1/auth/login`                    | Operator login (Controller) → JWT |
| POST   | `/api/v1/devices/enroll`                | One-time device enrollment (registers pubkey, assigns `device_id`) |
| POST   | `/api/v1/auth/challenge`                | Issue nonce for device challenge/response |
| POST   | `/api/v1/auth/verify`                   | Verify signed challenge → short-lived session token |
| POST   | `/api/v1/devices/register`              | Per-session capabilities report (authenticated) |
| GET    | `/api/v1/devices/{device_id}/command`   | Poll commands            |
| POST   | `/api/v1/devices/{device_id}/status`    | Device status            |
| POST   | `/api/v1/tests/{test_id}/summary`       | Upload summary           |
| POST   | `/api/v1/tests/{test_id}/upload`        | Upload logs              |

All endpoints require TLS and `Authorization: Bearer <token>`. Operator endpoints accept the JWT from `/auth/login`; device endpoints accept the session token from `/auth/verify`. The two audiences are disjoint — a device token cannot call controller APIs and vice versa.

### Authentication

Two principal types, one validation pipeline:

- **Operator (Controller):** username/password → short-lived **JWT** with a role claim (`viewer` / `operator` / `admin`; MVP uses `operator`).
- **Device (Logger/UE): enrollment + challenge/response, no long-lived bearer.** On first launch the app generates a non-exportable **P-256 keypair** in the platform secure store (Android Keystore; iOS Secure Enclave) and `POST /devices/enroll`s its **SPKI public key** with a one-time enrollment token; the server returns the assigned `device_id`/`node_id`. Every later session: `POST /auth/challenge` returns a random nonce; the device signs the domain-separated blob `"mp-auth-v1" || server_id || challenge_id || nonce` with `ECDSA_P256_SHA256` (ASN.1 DER); `POST /auth/verify` returns a short-lived opaque session token. The server derives identity **from the verified signature, never from a client-claimed `device_id`**. Revocation = delete the pubkey row.

**Cross-platform by contract.** The wire format is platform-neutral so iOS (and future clients) work with the same server path. Pinned constants:

- `AUTH_SIGNING_ALG = "ECDSA_P256_SHA256"`
- `AUTH_PUBKEY_FORMAT = "SPKI_DER"` (base64)
- `AUTH_SIG_FORMAT = "ASN1_DER"`
- `AUTH_DOMAIN = "mp-auth-v1"`

`contracts/testvectors/` ships a cross-platform signing vector (`priv_key_pem, pub_key_spki_b64, message_hex, signature_der_b64`) that every implementation must round-trip — guards against the DER-vs-IEEE-P1363 interop bug. **iOS is not in MVP scope** (Pixel 9 Pro is the reference); the contract is platform-neutral so iOS can be added later without a breaking change.

### Device Registration

After authenticating (challenge/verify), devices call `/devices/register` at startup with a capabilities report. The server responds with a `logging_config` telling the device which features to enable. (Identity comes from the session token; the `device_id` is server-assigned at enrollment, not self-claimed.)

**Request example:**

```json
{
  "device_id": "pixel9pro_001",
  "device_label": "Thomas Pixel 9 Pro",
  "app_version": "0.1.0",
  "android_version": "16",
  "manufacturer": "Google",
  "model": "Pixel 9 Pro",
  "capabilities": {
    "radio": {
      "serving_cell": ["pci", "nci", "tac", "nrarfcn", "rsrp", "rsrq", "sinr"],
      "neighbor_cells": true,
      "cqi": false,
      "mcs": false,
      "drx": false
    },
    "qos": {
      "5qi": false,
      "qfi": false
    },
    "mobility": {
      "explicit_ho_events": false,
      "rrc_state": false
    },
    "timestamp_sources": ["mono", "utc", "gps"]
  }
}
```

**Response example:**

```json
{
  "device_id": "pixel9pro_001",
  "registered_at": "2026-05-20T08:00:00Z",
  "logging_config": {
    "radio": {
      "serving_cell": ["pci", "nci", "tac", "nrarfcn", "rsrp", "rsrq", "sinr"],
      "neighbor_cells": true,
      "cqi": false,
      "mcs": false,
      "drx": false
    },
    "events": ["TEST_START", "TEST_STOP", "SIGNAL_LOSS_START", "SIGNAL_LOSS_END"],
    "gps": true,
    "clock_calibration_interval_sec": 30
  }
}
```

More capable devices (e.g., rooted or OEM-diagnostic builds) may report `true` for extended fields. The server can then enable those fields in the `logging_config`.

---

## State Machines

### Per-Device Test State (test-control)

```text
IDLE → PREPARE → WAITING_FOR_GENERATOR → WAITING_FOR_CLIENT → RUNNING → STOPPING → DONE
                                                                     ↓
                                                                   FAILED
```

### Data-Generator States

```text
INITIALIZING → READY → WAITING_FOR_CLIENT → RUNNING → STOPPING → STOPPED
                                              ↓
                                            FAILED
```

### Logger App States

```text
INITIALIZING → REGISTERING → IDLE → WAITING_FOR_TEST → PREPARING → RUNNING_TEST → COOLDOWN → UPLOAD_PENDING → UPLOADING
                                                                                          ↓
                                                                                        ERROR
```

---

## Traffic Types (Planned)

1. **UDP Probe** — RTT, jitter, packet loss, tail latency
   - Packet structure: `flow_id` (u32), `sequence_number` (u64), `send_time_ns` (u64)
   - Client sends to server, server echoes back.

2. **Realtime UDP Flow** — RTP-like traffic with embedded timing metadata for one-way latency and inter-packet gap analysis (initially fixed-rate UDP stream)
   - Configurable direction: `FromServer`, `ToServer`, or `Bidirectional`
   - Both endpoints implement generator and receiver; direction determines which sends payload
   - UE always sends to the server port; for `FromServer` sends keepalive packets (~1 Hz) until first downlink packet received
   - **UE forced to RRC_CONNECTED active state** — DRX effects are out of scope

3. **Bulk TCP Flow** — TCP upload/download for congestion and queue buildup analysis
   - Configurable direction: `FromServer`, `ToServer`, or `Bidirectional`
   - Both endpoints implement generator and receiver; direction determines which sends payload
   - UE always opens the TCP connection to the server port

---

## Time Synchronization

### Mobile Clock

- Android devices maintain monotonic time, UTC time, and GPS-derived timing calibration.
- The client periodically logs clock calibration snapshots.

### Server Clock

- Linux server synchronizes using `chrony` and Netnod NTP servers:
  - `mmo1.ntp.netnod.se`
  - `mmo2.ntp.netnod.se`
  - `sth1.ntp.netnod.se`

### One-Way Delay Estimation

- RTT is considered reliable.
- Uplink delay: `server_recv_utc - client_send_utc`
- Downlink delay: `client_recv_utc - server_send_utc`
- All OWD estimates must include estimated clock uncertainty.

---

## Upload Architecture

- Uploads are client-initiated.
- The server defines upload policies.
- **Rule: Uploads must never interrupt active measurements.**

### Planned Upload Policies

- upload after test
- upload after N tests
- upload after elapsed time
- upload after log size
- upload after session
- upload on request
- upload disabled

---

## Failure Handling

### Network Loss During Test

- Client continues logging.
- Test continues if possible.
- Upload is deferred.
- Reconnect is attempted later.

### Test Result States

A test may be: `COMPLETED`, `FAILED`, or `PARTIAL`. Raw logs are always preserved.

---

## Initial MVP Scope

### Linux Server

- `server-main`
- `test-control`
- `data-generator`
- UDP probe
- Realtime UDP
- Bulk TCP

### Android Logger

- Foreground service
- Device registration
- Local logging
- UDP probe
- Traffic generator / receiver (Realtime UDP, Bulk TCP for all directions)
- Radio logging
- GPS logging

### Android Controller

- Device list
- Start/stop tests
- Upload control

### Analysis

- Python scripts
- Route plots
- RTT plots
- Radio correlation
- Packet timing analysis

---

## Future Extensions (Out of MVP Scope)

- Remote traffic generators
- Distributed generators
- QUIC traffic
- Browser emulation
- RTP framing
- Synchronized multi-device tests
- Advanced web dashboard
- Real-time telemetry
- Machine learning analysis
- Cloud deployment

---

## Development Conventions

### Build commands

```bash
# Debug build (fast feedback)
cargo build --workspace

# Release build (size/performance checks)
cargo build --workspace --release

# Run all Rust unit tests
cargo test --workspace

# Stage A harness (smoke test)
./spikes/stage-a-harness/run.sh

# Stage A individual checks
./spikes/stage-a-harness/test_resilience.sh
./spikes/stage-a-harness/test_sigkill.sh
```

### Java / JNI

- JDK 17+ required (`javac`, `java`)
- JNI library path: `-Djava.library.path=$(pwd)/target/debug` (or `target/release`)
- Java classes compiled to `spikes/stage-a-harness/build/`

### Python validation

- Use `venv/` at repo root (do not commit)
- Required: `pyarrow` (for Parquet validation)
- Activate: `source venv/bin/activate`

### Code style

- Rust: standard `cargo fmt` / `cargo clippy` (enforced in CI when pipeline is added)
- Java: standard `javac` (no special formatter yet)

### Testing strategy

| Level | What | How |
|-------|------|-----|
| Unit | Rust modules (packet, timestamp, log_writer) | `cargo test --lib` |
| Integration | JNI round-trip, UDP echo, Parquet read-back | `run.sh` |
| Chaos | Blackout resilience, SIGKILL survival | `test_resilience.sh`, `test_sigkill.sh` |

### Versioning

- Workspace-level `Cargo.toml` coordinates versions for Rust crates
- Android apps use their own version (independent release cadence)
- No scheme defined yet for Android or Python — will be added when those components mature

### Repo layout

```text
/
  Cargo.toml                # Rust workspace
  core/                     # shared engine+logger lib (cdylib + rlib)
  server/                   # server-main, test-control, data-generator (placeholder)
  spikes/stage-a-harness/   # Stage A desktop-JVM spike (kept; feeds Phase 1)
  contracts/                # (pending) single source of truth
  analysis/                 # (pending) Python package
  android/                  # (pending) Gradle project
  venv/                     # Python virtualenv (gitignored)
```

---

## How to Update This File

Whenever you add or modify:

- Build files (`Cargo.toml`, `pyproject.toml`, `build.gradle`, etc.)
- Source directory structure
- Test configurations
- Deployment scripts
- CI/CD workflows

…update the relevant sections in this file so future agents have accurate, up-to-date context.

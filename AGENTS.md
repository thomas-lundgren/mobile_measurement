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

Built and running live (Oct 2026): the Rust server (server-main, test-control,
data-generator), the Android Logger (shared Rust core over JNI) and Controller,
the on-phone `mp-probe` collector, and the Python analysis that feeds the
server's `/viz` pages. `STATUS.md` has the history; `CLAUDE.md` the layout and
commands. Sections below titled "Planned" describe the design the build
follows; where they differ, the spec (`planning/5_g_field_test_platform_architecture_spec.md`) wins.

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

### 6. Single-Source Contracts

Every value that crosses a component or language boundary is defined **once** in `contracts/`: packet layouts, REST/JSON bodies, enum and state strings, log columns, version constants, and side protocols (reach, mp-probe). Every other copy is either **generated** from that source or **compared** to it by a check that fails CI. A copy that only a comment or a Markdown table keeps in step is drift waiting to happen.

When you add or change such a value:

- Edit the contract first, then the code.
- Adding a copy (a new language, writer, reader, flow type or side protocol)? Add its generator or parity check **in the same commit**.
- Run `ci/check.sh`. It runs every gate below.

| Source in `contracts/` | Copies | Gate |
|---|---|---|
| `enums/enums.yaml`, `versions/versions.yaml` | Rust `mp-contracts`; `packet::APP_ACTIVITIES` (`app_activities`, the wire codes) | `server/contracts-rs/tests/parity.rs`; `packet::app_activities_match_the_contract` |
| `packets/layouts.yaml` (+ `packets/*.md` semantics, `testvectors/packets/`) | `core/src/packet/layouts.rs` (one codec, both nodes via JNI) and the specs' layout tables, **generated** by `contracts/codegen/packet_codec.py` | `packet_codec.py --check`; generated golden tests; `validate_contracts.py` packs the vectors from the YAML |
| `schemas/*.json` | Rust DTOs → generated `openapi.yaml` → generated Kotlin client | `ci/validate_contracts.py` (examples, plus schema vs Rust shape); `openapi_drift`; `ci/check_client_drift.sh` |
| `schemas/internal_*.json` (server-main ↔ test-control ↔ data-generator) | Rust DTOs (`dto/internal.rs`) → generated `openapi/internal.yaml` | `ci/validate_contracts.py` (schema vs Rust shape); `internal_openapi_drift` |
| `parquet/*.parquet.yaml` | Rust writers; `analysis/fieldtest_analysis/schema.py` | `core/tests/parquet_contract.rs`; `analysis/tests/test_schema_contract.py` |
| `db/schema.sql` | server migrations | migration test in `mp-server-main` |
| all of the above, for the Logger | `Contract.java`, **generated** by `contracts/codegen/java_contract.py`: versions, enums, REST paths, row keys, mp-probe | `java_contract.py --check`; Logger JVM tests (`android/logger/src/test`) validate every body it sends against the schemas |
| RUN_TEST flows → core | the Logger passes each flow whole (`Wire.ueConfig`) | `core/tests/test_plan_contract.rs`: the core reads every field of every example, and the examples use every flow property |
| `packets/time_sync.md` + vectors | server-main `time_sync.rs`, Logger `ClockSampler` | golden tests on both sides |
| `packets/reach.md` + `reach_v1.json` | server-main `reach.rs`, Logger `ReachabilityChecker` | golden tests on both sides |
| `schemas/core_stop_summary.schema.json` | core `FlowSummary` → Logger (`Wire.testResult`) | `ue::flow_summary_matches_the_contract`; Logger JVM tests |
| logged value sets (`enums.yaml` log_nodes, packet_directions, packet_protocols, send_statuses, radio_network_types, app_step_kinds, app_outcomes, …) | core writers, Logger `Contract`, analysis `schema.py` / `findings.py` | `log_writer::logged_values_match_the_contract`; `app_traffic::model::logged_kinds_and_outcomes_match_the_contract`; `test_schema_contract.py` |
| `internal/mp_probe.yaml` + `testvectors/mp_probe/` | `probe/` (Rust), Logger `MpProbe`, `analysis` `bench.py` | probe tests, Logger JVM tests, `test_bench.py` |

In the Logger, wire values and the keys it reads come from the generated `Contract` (including `Contract.Fields.<Type>` for OpenAPI and schema-only types); every JSON body it sends is built in `Wire.java` — never inline a wire string elsewhere.

New fields in a JSON body need an example in `contracts/testvectors/` that uses them, because the shape check compares names and enum values, not bounds.

**Still unchecked:** nothing known. A copy found without a gate gets one; don't add copies that rely on a comment to stay in step.

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
      app_events_NNNN.parquet  # app flows' intended operations, one per flow (NNNN = flow_id)
      probe.jsonl.gz         # mp-probe records for the test window, when it runs
    server/
      packets.parquet        # all flows, server's view
      events.parquet
      clock.parquet
```

### File Purposes

- `header.json` — schema version, app version, protocol version, device metadata, test configuration, flow configuration
- `packets.parquet` — packet metadata (timestamps, flow IDs, protocol, ports, sizes, sequence numbers); **one per node** (`mobile/`, `server/`)
- `radio.parquet` — radio metrics (PCI, NCI, TAC, NRARFCN, RSRP, RSRQ, SINR, LTE info if NSA, neighbor cells, derived handover flags); **mobile-only ("mobile metric trace"), one per test** shared by all flows
- `gps.parquet` — GPS snapshots (latitude, longitude, speed, heading, accuracy); **mobile-only, one per test** shared by all flows
- `clock.parquet` — clock calibration snapshots (monotonic timestamp, UTC timestamp, source, uncertainty, estimated offset)
- `events.parquet` — system events + derived events (HO from PCI changes, coverage gaps, band changes)
- `app_events_NNNN.parquet` — one per app_traffic flow (mobile): the workload's intended operations (agent → task → step → object, the plan's bytes and think time) and how each ended; joins `packets` on `flow_id` + `object_id`
- No per-node summary file: the test summary is an analysis product (`fieldtest-analyze`, spec §12.8). Rows lost to a log ring overflow are an `ERROR` event in `events.parquet`, and the mobile uploads the test as `PARTIAL`.

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
   - Configurable direction: `FromServer` or `ToServer`; traffic both ways is two flows (for bulk TCP on separate connections)
   - Both endpoints implement generator and receiver; direction determines which sends payload
   - UE always sends to the server port; for `FromServer` sends keepalive packets (~1 Hz) until first downlink packet received
   - **UE forced to RRC_CONNECTED active state** — DRX effects are out of scope

3. **Bulk TCP Flow** — TCP upload/download for congestion and queue buildup analysis
   - Configurable direction: `FromServer` or `ToServer`; traffic both ways is two flows (for bulk TCP on separate connections)
   - Both endpoints implement generator and receiver; direction determines which sends payload
   - UE always opens the TCP connection to the server port
   - Optional `rate_kbps` cap (paced writes) for head-of-line tests

4. **App Traffic** (`app_traffic`) — simulated users (web, mail, video, social,
   mixed, AI agent with tool placement device/cloud/hybrid, or a scripted
   timeline) as background load beside the realtime flows; the server is a plain
   object server (`AppExchange`, contracts/packets/app_request.md)

5. **DSCP probe** — probe variant cycling DSCP values to measure marking
   transparency per direction

The realtime UDP and bulk TCP send/receive loops exist once, in
`core/src/endpoint.rs`, and the request/response engine behind app traffic in
`core/src/exchange.rs` (UE `transfer`, server `serve`); both are used by the UE
engine and the server's data-generator (symmetric peers). Application
workloads (`core/src/app_traffic`) only decide what to transfer and when —
see `planning/app_workload_layering.md`. The engines report every read and
write to their caller and never log or name a workload themselves; the
server does exactly what each request on the wire asks.
`core/tests/layering.rs` fails if an engine module or a server process
reaches `app_traffic`.

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
cargo build --workspace                 # debug; servers run from target/release (--release)
cargo test --workspace                  # all Rust tests; one: cargo test -p <crate> <name>
ci/check.sh                             # fmt, clippy -D warnings, tests, contracts, analysis, client drift
analysis/.venv/bin/python -m pytest -q analysis/tests
android/build_apk.sh                    # core via cargo-ndk + Logger debug APK
cargo ndk -t arm64-v8a build -p mp-probe --release
```

### Java / Android

- JDK 17+, Android SDK + NDK (`android/local.properties`), cargo-ndk, rustup target `aarch64-linux-android`
- Install: `adb install -r android/logger/build/outputs/apk/debug/logger-debug.apk`

### Python

- Analysis: `analysis/.venv` (`pip install -e ".[dev,plots]"`); contract validation: `venv/` at repo root

### Code style

- Rust: `cargo fmt` / `cargo clippy -D warnings` (CI)
- Java: standard `javac`; Python: pytest

### Testing strategy

| Level | What | How |
|-------|------|-----|
| Unit | Rust modules, analysis modules | `cargo test -p <crate> --lib`, `pytest` |
| Integration | server flows, UE engine against the real servers (`ue_interop`), uploads | `cargo test --workspace` |
| Contracts | schemas, golden packet vectors, OpenAPI drift, Rust↔YAML parity | `ci/validate_contracts.py`, parity tests |
| Field | live phone tests | `scripts/fieldtest/*.sh` (cycles on `lib.sh`) |

### Versioning

- Workspace-level `Cargo.toml` coordinates versions for Rust crates; `mp-probe` has its own
- Wire / log versions: `contracts/versions/versions.yaml`
- App versions and versioned logs: planned in `planning/ota_and_versioning_plan.md`

### Repo layout

```text
/
  Cargo.toml                # Rust workspace
  core/                     # shared engine+logger lib (cdylib + rlib): endpoint, ue, app_traffic, log_writer, ...
  server/                   # contracts-rs, server-main, test-control, data-generator
  probe/                    # mp-probe on-phone collector
  android/                  # logger (Java + JNI), controller (Kotlin), build_apk.sh
  analysis/                 # Python package fieldtest_analysis + tests
  contracts/                # single source of truth (schemas, OpenAPI, parquet, packets, vectors)
  scripts/                  # viz_rebuild.sh, fieldtest/ cycle scripts
  ci/                       # check.sh, contract validation, client drift
  planning/                 # spec and plans
  spikes/                   # stage A/B spikes (kept for reference)
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

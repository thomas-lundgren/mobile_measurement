# 5G Field Test Platform — Architecture & Functional Specification

## 1. Overview

This project defines a distributed mobile network measurement platform for:

- Cross-layer latency analysis
- Mobility analysis
- Packet timing analysis
- Radio quality correlation
- Congestion and queueing analysis
- Realtime traffic behavior
- Cross-flow interference
- HOL-like effects in mobile networks

The platform is intended to run real-world experiments using Android devices moving through live mobile networks while controlled traffic is generated from one or more servers.

The system is designed around:

- Robust local logging
- Central orchestration
- Timestamped packet-level measurements
- Flexible test generation
- Scalable multi-device operation
- Future support for remote traffic generators

---

# 2. High-Level System Architecture

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

Main components:

1. Linux Server
   - server-main
   - test-control
   - data-generator(s) (server-side)

2. Android Logger App
   - autonomous measurement client
   - traffic generator / receiver / loopback (all flow types and directions)

3. Android Controller App
   - remote control and monitoring UI

---

# 3. Core Design Principles

## 3.1 Logging First

Measurements must never depend on upload availability.

Rules:

- All data is first written locally on the mobile device
- Upload is always secondary
- Upload failures must never interrupt measurements
- Tests continue during temporary network loss

---

## 3.2 Time Synchronization Model

All timestamps use two parallel clock domains:

| Field | Purpose |
|---|---|
| `mono_ns` | Local monotonic ordering |
| `utc_ns` | Cross-device/server correlation |

Monotonic timestamps are authoritative for:

- Packet ordering
- Jitter
- Inter-arrival timing
- Burst detection
- Internal event sequencing

UTC timestamps are used for:

- One-way delay estimation
- Mobile ↔ server correlation
- Multi-device alignment
- Session analysis

---

## 3.3 Forward-Compatible Logging

Every file contains:

- `schema_version`
- `protocol_version`
- `app_version`
- `device_id`
- `session_id`
- `test_id`

Rules:

- New fields may be added
- Existing fields must not change meaning
- Missing fields are interpreted as `null`
- Clients must tolerate unknown fields

---

## 3.4 Capability-Tolerant Clients & Extensible Log Format

Different Android devices expose different radio APIs. This platform targets the **Google Pixel 9 Pro** as the reference device, but the log format is **extensible** — more capable devices may log additional fields.

**Known Pixel 9 Pro capabilities (Android 15+):**

- Serving cell info: PCI, NCI, TAC, NRARFCN, RSRP, RSRQ, SINR
- Neighbor cell lists (intra-frequency + inter-frequency)
- Signal strength callbacks (`TelephonyCallback.SignalStrengthsListener`)
- Cell info updates (`TelephonyCallback.CellInfoListener`)
- GPS / Fused location
- **No DRX data available** (not exposed via public APIs)
- **No 5QI/QFI data available** (not exposed via public APIs)
- **No RRC state transitions available** (not exposed via public APIs)

**Extensibility rules:**

- All fields in all log files are **optional** at the row level.
- New fields may be added by any device without changing the schema version.
- Missing fields are interpreted as `null` by analysis tools.
- Analysis tools must tolerate unknown fields (forward compatibility).
- The server stores all fields present in uploaded Parquet files, even if they exceed the baseline schema.
- Devices report their full capabilities during registration.
- **The server configures which features to log** based on device capabilities, test plan, and operator policy. The UE logs only what the server enables.

**Baseline vs. extended fields:**

| Tier | Availability | Examples |
|---|---|---|
| Baseline | All devices (Pixel 9 Pro reference) | PCI, NCI, RSRP, RSRQ, SINR, GPS |
| Extended | OEM-specific or rooted devices | CQI, MCS, BLER, HARQ, DRX config, 5QI, QFI, RRC state |

Therefore:

- Missing fields are acceptable
- Devices report capabilities during registration
- The UE is forced to **RRC_CONNECTED active state** during tests; DRX effects are explicitly out of scope for baseline
- Handovers are derived **retrospectively** from PCI/NCI changes in `radio.parquet` for baseline devices; explicit HO events may be logged by extended-capability devices

---

## 3.5 Symmetric Peer Nodes

The server and the mobile (UE) are **symmetric peers** in a measurement. Each node can act as:

- **Generator / sender** — produces payload traffic for a flow
- **Receiver** — consumes and logs payload traffic for a flow
- **Loopback / echo** — reflects a flow back to its origin (either node may host the loopback; the UDP probe is the common case)
- **Logger** — records its own local log files for the test

Both nodes participate in test and measurement, and both produce log files. The configured flow *direction* decides which side sends payload for a given flow; it does **not** make either node measurement-passive. The only fixed asymmetry is connection initiation — the UE always initiates (see §7.1).

A **test** is the unit that aggregates these recordings: it may contain many data flows, and the recordings from both nodes are collected under the same `test_id` (see §12 and §13).

---

# 4. Linux Server Architecture

The Linux server consists of three primary processes.

```text
server-main
  ├── REST API
  ├── device registry
  ├── upload manager
  ├── session metadata
  └── command queues


test-control
  ├── scheduler
  ├── test orchestration
  ├── port allocation
  ├── state machines
  └── generator management


data-generator(s)
  ├── UDP probe service
  ├── realtime UDP service
  ├── bulk TCP upload/download
  └── future traffic generators
```

Recommended implementation language:

- Rust

Recommended Rust stack:

| Library | Purpose |
|---|---|
| axum | REST API |
| tokio | async runtime |
| serde | serialization |
| sqlx | database |
| tracing | logging |
| uuid | identifiers |
| chrono | time |

---

# 5. server-main

## 5.1 Purpose

`server-main` is the central control-plane process.

It is responsible for:

- Device registration
- Session management
- Command distribution
- Upload handling
- Metadata storage
- API handling
- Authentication — operator (Controller) and device (Logger); see §5.3
- Controller app integration

The logger clients communicate only with `server-main`.

---

## 5.2 Functional Responsibilities

### Device Registration

Devices register themselves at startup.

Example request:

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
      "lte_fallback": true,
      "cqi": false,
      "mcs": false,
      "bler": false,
      "harq": false,
      "drx": false,
      "bwp": false,
      "mimo_layers": false,
      "tx_power": false
    },
    "qos": {
      "5qi": false,
      "qfi": false,
      "slice_info": false
    },
    "mobility": {
      "explicit_ho_events": false,
      "rrc_state": false,
      "beam_switch": false
    },
    "timestamp_sources": ["mono", "utc", "gps"]
  }
}
```

Server response includes the logging configuration for this device:

```json
{
  "device_id": "pixel9pro_001",
  "registered_at": "2026-05-20T08:00:00Z",
  "logging_config": {
    "radio": {
      "serving_cell": ["pci", "nci", "tac", "nrarfcn", "rsrp", "rsrq", "sinr"],
      "neighbor_cells": true,
      "lte_fallback": true,
      "cqi": false,
      "mcs": false,
      "bler": false,
      "harq": false,
      "drx": false,
      "bwp": false,
      "mimo_layers": false,
      "tx_power": false
    },
    "qos": {
      "5qi": false,
      "qfi": false,
      "slice_info": false
    },
    "mobility": {
      "explicit_ho_events": false,
      "rrc_state": false,
      "beam_switch": false
    },
    "events": ["TEST_START", "TEST_STOP", "SIGNAL_LOSS_START", "SIGNAL_LOSS_END"],
    "gps": true,
    "clock_calibration_interval_sec": 30
  }
}
```

---

### Server-Configured Logging

The server — not the UE — decides which features are logged. This enables:

- **Fleet consistency:** All devices in a test session log the same fields.
- **Data volume control:** The operator can disable expensive features (e.g., neighbor cell scans) to save battery/storage.
- **Progressive enhancement:** A baseline config is sent to all devices; extended configs are sent only to capable devices.
- **Dynamic reconfiguration:** The operator can change the logging profile mid-session via `SET_LOGGING_CONFIG`.

**Configuration hierarchy:**

1. **Default logging profile** (server-wide baseline)
2. **Test plan override** (specific to a test plan)
3. **Device capability mask** (only capabilities the device reported as `true` can be enabled)
4. **Per-device override** (administrative override for a specific device)

The effective config is the intersection of the test plan and device capabilities, with per-device overrides applied last.

**Logging config is versioned.** The UE includes `logging_config_version` in its status uploads. The server can detect config drift and push updates via commands.

---

### Session Management

The server maintains:

- sessions
- schedules
- tests
- upload state
- device states

---

### Command Queue

The server queues commands for each device.

Example commands:

- RUN_TEST
- STOP_TEST
- PAUSE
- UPLOAD_NOW
- SET_UPLOAD_POLICY
- SET_LOGGING_CONFIG (update logging configuration dynamically)

**Delivery is at-least-once.** A poll claims the oldest pending command and
marks it delivered; a delivered command not acked within 30 s is handed out
again, so a poll response lost in a coverage gap never drops a command. The UE
de-duplicates (RUN_TEST by `test_id`, persisted before the test starts) and
re-acks duplicates; an ack of an already-acked command (404) counts as success.
The UE acks RUN_TEST when measuring is finished and its upload is durably
queued on the device — not when the upload completes — and server-main stops
the test's server-side generators on that ack. The test reaches DONE when the
upload completes.

---

### Upload Handling

The server receives:

- summaries
- parquet files
- metadata
- session archives

Uploads are decoupled from measurements.

---

## 5.3 Interfaces

**Transport:** all endpoints are served over **TLS**. Every endpoint except operator login,
device enrollment, and the auth challenge/verify exchange requires an
`Authorization: Bearer <token>` header (see Authentication). Auth model and rationale: see
`planning/development_plan.md` §7.

### Authentication

Two principal types, one validation pipeline on `server-main`:

- **Operator (Controller)** — `POST /api/v1/auth/login` with username/password returns a
  short-lived **JWT** bearing a role claim (`viewer` / `operator` / `admin`). The Controller
  sends it as `Authorization: Bearer <jwt>` on all controller endpoints.
- **Device (Logger/UE) — enrollment + challenge/response, no long-lived bearer.** On first
  launch the app generates a non-exportable **P-256 keypair** in the platform secure store
  (Android Keystore; iOS Secure Enclave) and `POST /devices/enroll`s its **SPKI public key**
  using a one-time enrollment token (provisioned out of band, e.g. operator-issued via the
  Controller). The server assigns a `device_id`/`node_id` and stores `(device_id, pubkey)`.
  Every later session: `POST /auth/challenge` returns a random nonce + `challenge_id`; the
  device signs the domain-separated blob
  `"mp-auth-v1" || server_id || challenge_id || nonce` with `ECDSA_P256_SHA256` /
  `ASN.1 DER`. The `||` is realized canonically as the four UTF-8 strings joined by a
  `0x1F` (Unit Separator) byte — unambiguous since the fields are ULIDs / base64 and contain
  no control chars; the exact bytes are pinned in `contracts/testvectors/auth_signing/`.
  `POST /auth/verify` returns a short-lived opaque **session token** used as
  `Authorization: Bearer …` on all subsequent calls. Identity is derived **from the verified
  signature, never from a client-claimed `device_id`**. Revocation = delete the pubkey row.

**Cross-platform by contract.** The wire format is platform-neutral so iOS (and future
clients) work with the same server code path; only the per-platform key-gen/sign glue
differs. Pinned constants (versioned in `contracts/`):

| Constant | Value |
|---|---|
| `AUTH_SIGNING_ALG` | `ECDSA_P256_SHA256` |
| `AUTH_PUBKEY_FORMAT` | `SPKI_DER` (base64) |
| `AUTH_SIG_FORMAT` | `ASN1_DER` |
| `AUTH_DOMAIN` | `mp-auth-v1` |

A cross-platform signing **golden vector**
(`priv_key_pem, pub_key_spki_b64, message_hex, signature_der_b64`) in
`contracts/testvectors/` must round-trip in every implementation (Rust verifier, Android
signer, iOS signer) — guards against the DER-vs-IEEE-P1363 interop bug. iOS is **not in MVP
scope** (Pixel 9 Pro is the reference); the contract is platform-neutral so iOS can be
added later without a breaking change.

```http
POST /api/v1/auth/login          # operator login → JWT
POST /api/v1/devices/enroll      # one-time; registers pubkey, assigns device_id
POST /api/v1/auth/challenge      # device_id → { challenge_id, nonce, expires_at }
POST /api/v1/auth/verify         # challenge_id + signature → session token
```

### REST Endpoints

#### Device Enrollment

```http
POST /api/v1/devices/enroll
Authorization: Bearer <enrollment_token>
```

Request: `device_pubkey` (SPKI DER, base64), `pubkey_format`, `sig_alg`, `sig_format`,
`platform` (`android` | `ios`), `hw_info`, `protocol_version`.

Response: `200 OK` with server-assigned `device_id`/`node_id`, `server_time_utc_ns`,
`schema_version`, `logging_config_version`. The enrollment token is burned.

#### Auth Challenge / Verify

```http
POST /api/v1/auth/challenge   # body: { device_id }
POST /api/v1/auth/verify      # body: { challenge_id, signature (ASN.1 DER, base64) }
```

`/auth/verify` returns a short-lived opaque `session_token` plus the current
`logging_config_version` (so the client can detect drift and trigger `SET_LOGGING_CONFIG`).

#### Device Registration

```http
POST /api/v1/devices/register
Authorization: Bearer <session_token>
```

Request: device capabilities (see §5.2). Sent each session after successful
challenge/verify; the server derives `device_id` from the session token, not the body.

Response: `200 OK` with the `logging_config` object.

#### Poll Commands

```http
GET /api/v1/devices/{device_id}/command
```

#### Device Status

```http
POST /api/v1/devices/{device_id}/status
```

#### Upload Summary

```http
POST /api/v1/tests/{test_id}/summary
```

#### Upload Logs

```http
POST /api/v1/tests/{test_id}/upload
```

#### Reachability Report

```http
POST /api/v1/devices/{device_id}/reachability
```

The device checks on its own which ports/protocols toward the server get through its current network — at service start, on every default-network change, and on request — because on a restrictive network (e.g. a guest Wi-Fi) it cannot receive a command. Every probe socket is bound to the network being checked. Probes: `GET /api/v1/time` on the control port, and the `MPREACH1` echo (`contracts/packets/reach.md`) over TCP/UDP on public ports the router maps onto server-main's reach responders (`MP_REACH_LISTEN`). Each reply carries the device's address as the server saw it, exposing NAT and proxies. Reports (`contracts/schemas/reachability_report.schema.json`) are stored on the device and uploaded once the server is reachable; the server appends them to `<upload_root>/reach/<device_id>.jsonl`.

#### Coverage Map and Charts (operator)

```http
GET /viz                         # map page (static, no data)
GET /viz/charts                  # zoomable time-series page (static, no data)
GET /viz/findings                # analysis findings page (static, no data)
GET /api/v1/viz/track            # radio samples placed at GPS fixes
GET /api/v1/viz/series           # {"days": [...]} with chart data
GET /api/v1/viz/series/{day}     # 200 ms bins for one local day (YYYY-MM-DD)
GET /api/v1/viz/findings/{day}   # per-test overview, regime changes, link pauses, gap episodes
GET /api/v1/viz/reachability     # device reachability reports, newest first
```

The pages log in with the operator account and send its JWT; the data endpoints return 401 without it, since positions reveal where devices have been. server-main only serves files under `<upload_root>/viz/`; the analysis package writes them (`python -m fieldtest_analysis.track` and `.series`). Series bins hold probe RTT, clock-corrected one-way delay per direction (median and max), TCP throughput per direction, RSRP and `network_type`, all on the server clock.

---

# 6. test-control

## 6.1 Purpose

`test-control` orchestrates all active tests.

It manages:

- per-device schedules
- state machines
- generator preparation
- timeout handling
- port allocation
- test synchronization

---

## 6.2 Per-Device State Machine

Each device has an independent state machine.

```text
IDLE
PREPARE
WAITING_FOR_GENERATOR
WAITING_FOR_CLIENT
RUNNING
STOPPING
DONE
FAILED
```

---

## 6.3 Functional Responsibilities

### Prepare Tests

Before a client receives a RUN_TEST command:

1. Allocate ports
2. Start generators
3. Wait until generators are listening
4. Transition generators to READY state
5. Send test information to client

---

### Port Allocation

Every flow receives dedicated ports.

Example:

```text
Device A:
  test_001 → 41000–41009

Device B:
  test_001 → 41010–41019
```

---

### Generator Coordination

`test-control` starts/stops server-side generator/receiver processes and instructs the mobile logger app to start/stop its generator/receiver.

For each flow:

1. Determine direction
2. Start both endpoint processes (server and mobile)
3. Ensure the server is listening on the allocated port
4. Send test config to mobile, including server address and port
5. Mobile initiates connection
6. Coordinate start/stop across both endpoints

Later versions may support:

- remote generators
- cloud generators
- distributed targets

---

## 6.4 Interfaces

### Internal Control API

Example commands:

```text
PREPARE_TEST
START_GENERATOR
STOP_GENERATOR
ABORT_TEST
```

---

# 7. data-generator

## 7.1 Purpose

The data-generator process creates controlled network traffic.

Both the **Linux server** and the **Android logger app** implement generator and receiver capabilities. The flow direction determines which endpoint actively generates payload traffic.

### Flow Direction

Every non-probe flow declares a direction:

| Direction | Payload origin | Payload destination | Connection initiator |
|---|---|---|---|
| `FromServer` | Server | Mobile | Mobile |
| `ToServer` | Mobile | Server | Mobile |
| `Bidirectional` | Both | Both | Mobile |

Both nodes always run generator + receiver + logger (see §3.5); the direction only selects which side emits payload. Both nodes record their own packet log for the flow.

**Connection initiation rule:** The UE (mobile) always initiates the connection by connecting to the configured server port:

- **TCP:** UE opens the TCP connection to the server port.
- **UDP:** UE sends the first packet to the server port (punching through NAT/firewall).

After initiation, payload flows according to the configured direction.

**Loopback / echo flows.** A flow may be configured as loopback, where the receiving node reflects each packet back to the sender. Either node may host the loopback role. The UDP probe is the common loopback case (echo-based and directionless); by default the server echoes, but the UE may host the echo instead.

---

## 7.2 Supported Traffic Types

### UDP Probe

Purpose:

- RTT
- jitter
- packet loss
- tail latency

Packet format:

```c
struct ProbePacket {
    uint32_t flow_id;
    uint64_t sequence_number;
    uint64_t send_time_ns;
};
```

Behavior:

```text
client → server
server → echo
```

---

### Realtime UDP Flow

RTP-like traffic with embedded timing metadata for one-way latency and inter-packet gap analysis.

Initial version:

- fixed-rate UDP stream
- configurable direction: `FromServer`, `ToServer`, or `Bidirectional`
- one-way latency measurement (uplink and downlink)
- inter-packet send/receive gap analysis
- burst and jitter characterization
- **UE forced to RRC_CONNECTED active state** (no DRX expected during test)

Packet format:

```c
struct RealtimePacket {
    uint32_t flow_id;
    uint64_t sequence_number;
    uint64_t send_time_mono_ns;   // sender monotonic timestamp
    uint64_t send_time_utc_ns;    // sender UTC timestamp
};
```

Connection initiation and behavior by direction:

```text
UE always initiates: sends UDP packets to server port

FromServer:
    UE → server  (keepalive/initiation packets, ~1 Hz, until first DL packet received)
    server → UE  (payload, downlink)

ToServer:
    UE → server  (payload, uplink)

Bidirectional:
    UE → server  (payload + keepalive, uplink)
    server → UE  (payload, downlink)
```

Both sender and receiver log:

- `send_time_mono_ns` / `send_time_utc_ns` — when the packet was transmitted
- `recv_time_mono_ns` / `recv_time_utc_ns` — when the packet was received
- `expected_interval_ns` — nominal inter-packet interval for this flow
- `actual_interval_ns` — measured interval between consecutive received packets

This enables:

- **One-way delay (OWD)** — `recv_time_utc_ns - send_time_utc_ns`, with clock uncertainty bounds
- **Inter-packet timing** — deviation from expected spacing, burst detection, clustering
- **Jitter** — variance in arrival intervals relative to sender spacing
- **Packet spacing analysis** — network-induced compression or expansion of packet trains

Delivery timing and back pressure (realtime UDP, both nodes):

- The receiver also logs the **kernel receive time** (`SO_TIMESTAMPNS`) of each packet. Kernel-time spacing shows how the radio/modem hands packets to the host (batches per slot/TTI, HARQ-delayed packets); app time minus kernel time is the host's own wake-up latency.
- The sender logs **every send attempt**, including failed ones, with its outcome (`OK`, `EAGAIN`, `ENOBUFS`, `ERROR`), the syscall duration and the bytes still queued below the socket (`SIOCOUTQ`). A sequence gap at the receiver whose SENT row is not `OK` is a local drop, not network loss.
- Optional `sndbuf_bytes` on the flow limits the sender's `SO_SNDBUF` and makes it non-blocking (with `IP_RECVERR`). If the modem stops draining the host queue, the small buffer fills and sends fail with `EAGAIN`: local back pressure. If the delay grows while sends keep succeeding with an empty `SIOCOUTQ`, the queue sits inside the modem/network instead.
- Rates up to 1000 pps (1 ms spacing) are paced to a few µs (sleep, then spin); actual send times are logged, so analysis never assumes ideal spacing.

Future versions:

- audio-like traffic
- video-like traffic
- variable bitrate
- bursty frame structures

---

### Bulk TCP Flow

Purpose:

- congestion
- queue buildup
- UL/DL stress

Initial implementation:

- configurable direction: `FromServer`, `ToServer`, or `Bidirectional`
- TCP upload (`ToServer`)
- TCP download (`FromServer`)
- simultaneous upload + download (`Bidirectional`)
- optional `rate_kbps` caps the sending rate: the sender writes 5 ms worth of bytes (at most 64 KiB) per paced write, so a low or medium bulk load can run beside realtime flows (head-of-line blocking tests) without saturating the link

Connection initiation and behavior by direction:

```text
UE always initiates: opens TCP connection to server port

FromServer:
    UE connects → server
    server → UE  (payload, download)

ToServer:
    UE connects → server
    UE → server  (payload, upload)

Bidirectional:
    UE connects → server
    UE → server  (payload, upload)
    server → UE  (payload, download, same connection)
```

### App Traffic Flow (`app_traffic`)

Purpose: everyday background load — web browsing, mail sync, video, a social
feed — beside the realtime flows, to see what ordinary app use does to them
(head-of-line blocking under realistic, bursty load rather than steady bulk),
and how the apps themselves fare (page load time, video segment time).

- The UE runs simulated users ("agents"); each opens its own TCP connections
  to the flow's server port (up to 6, reused, closed after 20 s idle like a
  browser's), so bursts start with real handshakes and slow start.
- The server is a plain object server: each request (contracts/packets/app_request.md;
  v2 `AppExchange`, v1 still served) sends bytes up, has the server wait a
  think time, then get bytes back — as fast as TCP takes them, or paced like
  an LLM token stream. Nothing back = an upload, acknowledged by an echo.
- Profiles: `web`, `mail`, `video`, `social`, `mixed` (switches among web,
  social and video every 2–5 min with mail in the background), `agent` (an AI
  agent at work: LLM calls that send the growing context up and stream the
  reply, between them parallel MCP calls and web fetches; `placement` puts the
  tools on the phone, in the provider's cloud inside the call — the reply
  pauses and carries the result text, no tool traffic — or MCP on the phone
  and web fetches in the cloud) and `script` (an
  explicit timeline of actions, so an AI agent playing a user can write the
  plan). `workers` runs several users at once, `seed` makes their choices
  reproducible, `scale` shrinks every object to save data.
- Both nodes log each object as rows sharing its `object_id`
  (`sequence_number`), tagged `app_activity`, `app_object_bytes` and
  `app_up_bytes`: request → first byte → completion per object, the gaps
  inside a stream, and per episode (a page view, a sync, an agent task step).

---

## 7.3 Generator States

```text
INITIALIZING
READY
WAITING_FOR_CLIENT
RUNNING
STOPPING
STOPPED
FAILED
```

---

## 7.4 Timestamping

All generators timestamp:

- packet send
- packet receive
- echo transmit

For the Realtime UDP flow specifically:

- sender includes `send_time_mono_ns` and `send_time_utc_ns` in every packet payload
- receiver logs both sender timestamps and local reception timestamps
- inter-packet intervals are computed on both send and receive sides

This enables:

- RTT analysis (UDP probe)
- one-way delay estimation (all flows)
- packet spacing analysis (all flows)
- burst analysis (all flows)
- inter-packet gap deviation (Realtime UDP)

---

# 8. Android Logger App

## 8.1 Purpose

The logger app is the mobile measurement agent.

It:

- connects to server-main
- receives test instructions
- runs traffic flows
- logs measurements locally
- uploads results later

The logger app must survive temporary network loss.

---

## 8.2 Runtime Model

The logger runs as:

- Android foreground service

This prevents aggressive background termination.

The app displays:

- foreground notification
- status icon

---

## 8.3 Functional Components

```text
logger-app
  ├── server connection client
  ├── command poller
  ├── test runner
  ├── traffic generator client
  ├── traffic generator / receiver  (Realtime UDP / Bulk TCP, active role depends on flow direction)
  ├── app traffic agents  (simulated users requesting objects; app_traffic flows)
  ├── packet logger
  ├── radio logger
  ├── GPS logger
  ├── clock calibration
  ├── upload manager
  └── local storage
```

---

## 8.4 Functional Responsibilities

### Command Polling

The client polls for commands.

Example:

```http
GET /api/v1/devices/{device_id}/command
```

---

### Local Logging

All data is written locally.

Logging continues during:

- server disconnect
- mobile handover
- temporary packet loss

---

### Traffic Execution

The client:

- opens TCP/UDP sockets
- initiates connection to the configured server port (all directions)
- sends payload traffic when configured as sender (`ToServer` / `Bidirectional`)
- receives and logs payload traffic when configured as receiver (`FromServer` / `Bidirectional`)
- timestamps packets
- measures RTT/jitter/loss
- logs received traffic (all directions)

---

### Upload Manager

Uploads occur independently from measurements.

Policies:

- upload after test
- upload after N tests
- upload after log size
- upload after session
- upload on request
- upload disabled

---

## 8.5 Logger States

```text
INITIALIZING
REGISTERING
IDLE
WAITING_FOR_TEST
PREPARING
RUNNING_TEST
COOLDOWN
UPLOAD_PENDING
UPLOADING
ERROR
```

---

# 9. Android Controller App

## 9.1 Purpose

The controller app is a remote-control UI.

It does not perform measurements.

It allows:

- device monitoring
- test control
- session control
- upload management
- status viewing

---

## 9.2 Functional Components

```text
controller-app
  ├── login/auth
  ├── server API client
  ├── device list
  ├── session list
  ├── command UI
  ├── upload controls
  └── status dashboard
```

---

## 9.3 Example Operations

### Start Test

```text
Select device
Select test plan
Start session
```

### Upload Logs

```text
Upload pending logs
Upload specific session
Upload specific test
```

### Device Monitoring

```text
online/offline
active test
GPS position
radio status
upload status
```

---

# 10. Traffic Model

The initial simplified traffic model consists of:

```text
1. UDP probe
2. Realtime UDP flow
3. Bulk TCP flow
```

These run simultaneously.

Primary analysis goals:

- latency inflation
- jitter
- queueing
- cross-flow interference
- mobility effects
- packet bursts

---

# 11. Time Synchronization & Clock Model

## 11.1 Mobile Clock

Android devices maintain:

- monotonic time
- UTC time
- GPS-derived timing calibration

The client periodically logs clock calibration snapshots.

---

## 11.2 Server Clock

Linux server synchronizes using:

- chrony
- Netnod NTP servers

Example:

```text
mmo1.ntp.netnod.se
mmo2.ntp.netnod.se
sth1.ntp.netnod.se
```

---

## 11.3 One-Way Delay Estimation

### RTT

Reliable.

### Uplink Delay

```text
server_recv_utc - client_send_utc
```

### Downlink Delay

```text
client_recv_utc - server_send_utc
```

All OWD estimates must include:

- estimated clock uncertainty

---

# 12. Log Structure

## 12.1 Session Layout

A test aggregates the recordings from **both nodes** under one `test_id`. Each node writes its own per-node files into a `mobile/` or `server/` subfolder. The mobile-only traces that exist **once per test regardless of flow count** — GPS and radio — sit at the test root.

```text
session_2026_05_20/
  test_001/
    header.json              # test + flow config, shared
    gps.parquet              # mobile only — exactly one per test
    radio.parquet            # mobile only ("mobile metric trace") — exactly one per test
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

Notes:

- A test may contain **many data flows**; all flows share the one `gps.parquet` and the one `radio.parquet`. Per-flow packet rows are distinguished by `flow_id` inside each node's `packets.parquet`.
- Only the mobile has GPS and radio/cell metrics, so there are no `server/gps.parquet` or `server/radio.parquet`.
- The server assembles this tree: it already holds its own `server/` recordings and drops the UE's uploaded files into the same `test_id` directory (see §13).

---

## 12.2 header.json

Contains:

- schema version
- app version
- protocol version
- device metadata
- test configuration
- flow configuration

---

## 12.3 packets.parquet

Packet metadata.

Fields:

- timestamps (`mono_ns`, `utc_ns`)
- flow IDs
- protocol
- ports
- packet sizes
- sequence numbers
- sender timestamps (`sender_mono_ns`, `sender_utc_ns`) — for OWD computation
- expected interval (`expected_interval_ns`) — nominal flow spacing
- actual interval (`actual_interval_ns`) — measured inter-arrival gap
- kernel receive time (`kernel_rx_utc_ns`) — realtime UDP RECEIVED rows
- send outcome (`send_status`), send syscall time (`send_call_ns`), queued bytes below the socket (`sock_outq_bytes`) — realtime UDP SENT rows, one per attempt

---

## 12.4 radio.parquet

Radio metrics.

Fields (standard Android Telephony APIs, Pixel 9 Pro reference):

- `mono_ns`, `utc_ns` — sample timestamp
- `pci` — Physical Cell ID
- `nci` — NR Cell Identity
- `tac` — Tracking Area Code
- `nrarfcn` — NR Absolute Radio Frequency Channel Number
- `rsrp` — Reference Signal Received Power (dBm)
- `rsrq` — Reference Signal Received Quality (dB)
- `sinr` — Signal to Interference + Noise Ratio (dB)
- `earfcn` — LTE EARFCN (if NSA)
- `lte_pci` — LTE PCI (if NSA)
- `lte_rsrp` — LTE RSRP (if NSA)
- `bandwidth_mhz` — LTE serving-cell bandwidth (if available)
- `lte_rsrq`, `lte_rssnr`, `lte_cqi`, `lte_ta` — LTE serving-cell RSRQ, RS-SNR, CQI and timing advance, from `SignalStrength` (the modem's current view)
- `ca_bandwidths_khz` — bandwidth of every aggregated carrier, primary first (`ServiceState.getCellBandwidths`)
- `link_dn_kbps`, `link_up_kbps` — the cellular network's link-capacity estimate (`NetworkCapabilities`; the modem's LCE when it reports one)
- `cell_connection_status` — PRIMARY / SECONDARY / UNKNOWN
- `network_type` — radio mode the device reports (Android `TelephonyDisplayInfo`): `LTE`, `LTE_CA`, `NR_NSA`, `NR_ADVANCED`, `NR_SA`, or `OTHER_<n>` for other `TelephonyManager` network types; null when unknown. The NR columns (`pci`…`sinr`) describe a serving 5G cell only while this is `NR_*`; otherwise NR cells the modem measures are listed as neighbors. In NSA the modem often drops the 5G leg (LTE / LTE_CA) while still listing NR cells.
- `transport` — network carrying the data: `CELLULAR`, `WIFI`, `ETHERNET`, `OTHER`, `NONE`. The cellular fields describe the modem even while traffic goes over Wi-Fi, so results must be split by this field.
- `wifi_freq_mhz`, `wifi_channel`, `wifi_bw_mhz`, `wifi_standard`, `wifi_rssi`, `wifi_tx_mbps`, `wifi_rx_mbps` — the Wi-Fi link when `transport = WIFI` (frequency/channel from `WifiInfo`; channel width from the connected AP's scan entry)
- `neighbor_cells` — array of visible neighbor cell PCIs with RSRP (directly available via `getAllCellInfo()`, logged at the serving-cell sample rate)

Derived fields (post-processing):

- `handover_event` — true if PCI or NCI changed since previous sample

**Out of scope (not available on Pixel 9 Pro without root):**

- DRX configuration
- CQI / MCS / BLER / HARQ retransmissions
- 5QI / QFI values
- RRC state transitions
- BWP information
- MIMO layer count
- TX power

All fields remain optional to preserve forward compatibility with other devices.

---

## 12.5 gps.parquet

GPS snapshots.

Fields:

- latitude
- longitude
- speed
- heading
- accuracy

---

## 12.6 clock.parquet

Clock calibration snapshots.

Fields:

- monotonic timestamp
- UTC timestamp
- source
- uncertainty
- estimated offset

---

## 12.7 events.parquet

System and derived events.

Logged explicitly:

- TEST_START
- TEST_STOP
- CLIENT_CONNECTED — the server writes one per flow (`server/events_f<flow_id>.parquet`) with `event_metadata` `{peer_ip, peer_port, protocol}`: the UE's address as the server saw it after NAT, i.e. which network the test actually ran over (operator mobile network vs. a Wi-Fi's internet line)
- SERVER_DISCONNECTED
- SIGNAL_LOSS_START (no serving cell for >N seconds)
- SIGNAL_LOSS_END (serving cell re-acquired)
- UPLOAD_STARTED
- ERROR

Derived in post-processing from `radio.parquet`:

- HANDOVER (PCI or NCI change between consecutive samples)
- NEIGHBOR_COUNT_CHANGE (significant change in neighbor cell list)
- COVERAGE_GAP (RSRP dropped below threshold)
- BAND_CHANGE (NRARFCN changed)

---

## 12.8 summary.json

Compact test summary.

Contains:

- RTT percentiles
- one-way latency percentiles (uplink / downlink)
- loss
- jitter
- inter-packet timing statistics (mean gap deviation, burst count)
- radio statistics (RSRP min/mean/max, SINR min/mean/max)
- handover count (derived from PCI/NCI changes)
- neighbor cell statistics (unique cells seen, max concurrent neighbors)
- GPS start/end
- PCI changes
- upload state

---

# 13. Upload Architecture

Uploads are client-initiated.

Both nodes record locally during a test. The server already holds its own `server/` recordings on disk; when the UE uploads, the server **collects the UE's files under the same `test_id`**, placing them in that test's `mobile/` subfolder (with the mobile-only `gps.parquet`/`radio.parquet` at the test root). The result is the single aggregated test tree shown in §12.1.

The server defines upload policies.

Policies include:

- upload after test
- upload after N tests
- upload after elapsed time
- upload after log size
- upload after session
- upload on request
- upload disabled

Rule:

```text
Uploads must never interrupt active measurements.
```

---

# 14. Failure Handling

## 14.1 Network Loss During Test

If server connectivity is lost:

- client continues logging
- test continues if possible
- upload deferred
- reconnect attempted later

---

## 14.2 Partial Results

A test may be:

- COMPLETED
- FAILED
- PARTIAL

Raw logs are always preserved.

---

# 15. Future Extensions

Potential future features:

- remote traffic generators
- distributed generators
- QUIC traffic
- browser emulation
- RTP framing
- synchronized multi-device tests
- advanced web dashboard
- real-time telemetry
- machine learning analysis
- cloud deployment

---

# 16. Initial MVP Scope

## Linux Server

- server-main
- test-control
- data-generator
- UDP probe
- realtime UDP
- bulk TCP

## Android Logger

- foreground service
- device registration
- local logging
- UDP probe
- traffic generator / receiver / loopback for all flow types and directions (Realtime UDP, Bulk TCP)
- radio logging
- GPS logging

## Android Controller

- device list
- start/stop tests
- upload control

## Analysis

- Python scripts
- route plots
- RTT plots
- radio correlation
- packet timing analysis


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
   - mobile traffic generator (for ToServer / Bidirectional flows)

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
| `t_mono_ns` | Local monotonic ordering |
| `t_utc_ns` | Cross-device/server correlation |

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
- Client authentication (future)
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

### REST Endpoints

#### Device Registration

```http
POST /api/v1/devices/register
```

Request: device capabilities (see §5.2).

Response: `200 OK` with `logging_config` object telling the device which features to enable.

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

| Direction | Payload traffic | Server role | Mobile role |
|---|---|---|---|
| `FromServer` | Server → Mobile | Generator + receiver + logger | Generator + receiver + logger |
| `ToServer` | Mobile → Server | Generator + receiver + logger | Generator + receiver + logger |
| `Bidirectional` | Both directions | Generator + receiver + logger | Generator + receiver + logger |

**Connection initiation rule:** The UE (mobile) always initiates the connection by connecting to the configured server port:

- **TCP:** UE opens the TCP connection to the server port.
- **UDP:** UE sends the first packet to the server port (punching through NAT/firewall).

After initiation, payload flows according to the configured direction.

The UDP probe is always echo-based and directionless.

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

```text
session_2026_05_20/
  test_001/
    header.json
    packets.parquet
    radio.parquet
    gps.parquet
    events.parquet
    clock.parquet
    summary.json
```

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

- timestamps (`t_mono_ns`, `t_utc_ns`)
- flow IDs
- protocol
- ports
- packet sizes
- sequence numbers
- sender timestamps (`sender_mono_ns`, `sender_utc_ns`) — for OWD computation
- expected interval (`expected_interval_ns`) — nominal flow spacing
- actual interval (`actual_interval_ns`) — measured inter-arrival gap

---

## 12.4 radio.parquet

Radio metrics.

Fields (standard Android Telephony APIs, Pixel 9 Pro reference):

- `t_mono_ns`, `t_utc_ns` — sample timestamp
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
- `bandwidth_mhz` — Cell bandwidth (if available)
- `cell_connection_status` — PRIMARY / SECONDARY / UNKNOWN
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
- CLIENT_CONNECTED
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
- mobile traffic generator (Realtime UDP, Bulk TCP for ToServer / Bidirectional)
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


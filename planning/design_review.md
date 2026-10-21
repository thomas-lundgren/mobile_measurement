# Design Review — 5G Field Test Platform Architecture Spec

> Date: 2026-05-20
> Review scope: `planning/5_g_field_test_platform_architecture_spec.md` (v1.0, ~1245 lines)
> Reviewer perspective: Implementation readiness, consistency, and clarity.

---

## Overall Assessment

The architecture is **coherent and well-structured** for a pre-implementation specification. The core abstractions (dual timestamps, local-first logging, server-configured features, capability tolerance) are sound. However, there are several **inconsistencies, ambiguities, and missing pieces** that will block or confuse implementation if not resolved.

**Readiness: 70%** — Ready to start implementing the server framework and UDP probe, but the test orchestration layer, upload protocol, and several data model details need clarification.

---

## 1. Critical Issues (Block Implementation)

### 1.1 No Test Plan / Session Configuration Format

**Problem:** The spec mentions "test plan," "test configuration," and "flow configuration" in `header.json` and throughout §6, but **never defines what a test plan actually contains**.

**What an implementer needs to know:**
- What fields define a test plan?
- What fields define a flow within a test?
- How does the server encode this in a `RUN_TEST` command?
- How does the mobile decode it and configure its generators?

**Missing definition:**

```json
{
  "test_id": "test_001",
  "test_plan_id": "commute_v1",
  "duration_sec": 600,
  "flows": [
    {
      "flow_id": 1,
      "type": "udp_probe",
      "direction": "echo",
      "rate_pps": 10,
      "packet_size_bytes": 64,
      "server_host": "10.0.0.1",
      "server_port": 41000
    },
    {
      "flow_id": 2,
      "type": "realtime_udp",
      "direction": "FromServer",
      "rate_pps": 50,
      "packet_size_bytes": 200,
      "server_host": "10.0.0.1",
      "server_port": 41001
    }
  ]
}
```

**Impact:** BLOCKS implementation of `test-control`, `RUN_TEST` command, and mobile test runner.

**Fix:** Add a §5.2 subsection or §10 subsection defining the test plan schema.

---

### 1.2 `RUN_TEST` Command Payload Is Undefined

**Problem:** §5.2 Command Queue lists `RUN_TEST` as a command type but never specifies its payload. §6.3 "Prepare Tests" says "Send test information to client" but doesn't say what that information is.

**What the mobile needs to receive:**
- Server IP address and port(s) for each flow
- Flow configuration (type, direction, rate, packet size)
- Test duration
- Logging configuration (already sent at registration, but may need per-test override)

**Impact:** BLOCKS implementation of mobile command handler and test runner.

**Fix:** Define the `RUN_TEST` command JSON schema.

---

### 1.3 Upload Protocol Is Underspecified

**Problem:** §13 "Upload Architecture" says uploads are client-initiated and lists policies, but there is **no protocol definition**.

**Missing details:**
- What is the upload body format? (multipart/form-data? binary tarball? individual file PUTs?)
- Is compression used? (gzip? zstd?)
- What is the sequence? (summary first, then files?)
- How is partial upload failure handled? (resume? retry?)
- What authentication/authorization is used on the upload endpoint?

**Impact:** BLOCKS implementation of upload manager on both mobile and server.

**Fix:** Add upload protocol section with HTTP semantics, body format, and retry logic.

---

### 1.4 `supported_features` vs `capabilities` Redundancy

**Problem:** §5.2 Device Registration has two separate objects:

```json
"supported_features": {
  "gps": true,
  "radio_nr": true,
  "packet_metadata": true
},
"capabilities": {
  "radio": { "serving_cell": [...], "neighbor_cells": true, ... },
  ...
}
```

These serve the same purpose. The `supported_features` object is a legacy of an earlier draft and is now superseded by the detailed `capabilities` object.

**Impact:** Confusion during implementation. Which one does the server use? Both? If both, they can contradict.

**Fix:** Remove `supported_features` and use only `capabilities`. Or define `supported_features` as a high-level summary derived from `capabilities`.

---

## 2. Significant Ambiguities (Will Cause Bugs)

### 2.1 Forward-Compatible Logging vs. File-Level Metadata

**Problem:** §3.3 says "Every file contains: schema_version, protocol_version, app_version, device_id, session_id, test_id." But §12 Log Structure shows these as separate files:
- `schema_version` / `protocol_version` / `app_version` → `header.json`
- `device_id` / `session_id` / `test_id` → implied by directory path?

**Ambiguity:** Are these fields repeated in every Parquet file, or only in `header.json`? Parquet files don't have a natural place for free-form metadata unless using Parquet key-value metadata.

**Recommendation:**
- `header.json` contains all metadata.
- Parquet files contain **only data columns**, plus `device_id`, `session_id`, `test_id` as columns for joinability.
- Remove the statement "Every file contains" and replace with "Every session contains `header.json` with metadata; every Parquet row contains identifiers for joinability."

---

### 2.2 Bidirectional TCP on a Single Connection

**Problem:** §7.2 Bulk TCP Flow says for Bidirectional: "UE connects → server ... UE → server (payload, upload) ... server → UE (payload, download, **same connection**)."

**Issue:** Bulk TCP upload and download on the **same TCP connection** is semantically strange. TCP is full-duplex, but bulk throughput tests typically measure one direction at a time to avoid ACK cross-traffic biasing the results. Running simultaneous bulk upload + download on one TCP connection means:
- The upload ACKs compete with download data for bandwidth.
- The measured throughput is not "pure" upload or download.
- This is not what operators or 3GPP mean by "bidirectional TCP test."

**Standard approach:** Bidirectional TCP uses **two separate TCP connections** (one upload, one download) or one connection with application-layer unidirectional streams.

**Fix:** Clarify whether Bidirectional TCP means:
- (a) Two separate TCP connections (one upload, one download) — **recommended**
- (b) One TCP connection with application-level bidirectional bulk transfer
- (c) Sequential: download for N seconds, then upload for N seconds

---

### 2.3 Realtime UDP Bidirectional Is Confusing

**Problem:** §7.2 Realtime UDP Flow says for Bidirectional:
```text
Bidirectional:
    UE → server  (payload + keepalive, uplink)
    server → UE  (payload, downlink)
```

This is asymmetric — the UE sends "payload + keepalive" while the server sends only "payload." But bidirectional should mean both sides send payload at their configured rate.

**Question:** Is there one `flow_id` or two? The diagram shows one flow carrying both directions. *(Corrected, remediation C1: an earlier version cited spec text, "separate flow_id", that does not exist.)*

**Fix:** Define Bidirectional Realtime UDP as **two separate unidirectional flows** with distinct `flow_id`s, each with its own rate and packet size:
- Flow A (`direction: FromServer`): server → UE
- Flow B (`direction: ToServer`): UE → server

---

### 2.4 `traffic generator client` vs `traffic generator / receiver`

**Problem:** §8.3 Functional Components lists:
```text
├── traffic generator client
├── traffic generator / receiver  (Realtime UDP / Bulk TCP, active role depends on flow direction)
```

**Ambiguity:** What does "traffic generator client" do that's different from "traffic generator / receiver"?

**Interpretation:**
- "traffic generator client" = the component that connects to **server-side** generators (receives commands, manages socket lifecycle)
- "traffic generator / receiver" = the actual **mobile-side generator** that sends payload for `ToServer`/`Bidirectional`

But this is unclear. A better naming would be:
```text
├── server flow coordinator      (connects to server generators, manages sockets)
├── mobile traffic generator     (sends payload for ToServer / Bidirectional)
├── mobile traffic receiver      (receives payload for FromServer / Bidirectional)
```

Or simply merge them into one component that handles all directions.

**Fix:** Clarify or merge these two components.

---

### 2.5 `neighbor_cells` Is Listed as "Derived" But Is Directly Available

**Problem:** §12.4 `radio.parquet` lists `neighbor_cells` under "Derived fields (post-processing)" but `TelephonyManager.getAllCellInfo()` returns neighbor cells directly.

**Fix:** Move `neighbor_cells` to the primary fields list. It should be logged at the same frequency as serving cell info.

---

### 2.6 How Is `SIGNAL_LOSS` Detected?

**Problem:** §12.7 lists `SIGNAL_LOSS_START/END` as explicitly logged events with only a partial criterion ("no serving cell for >N seconds"): N and the detection mechanism are undefined. *(Corrected, remediation C2: an earlier version said the criterion was never defined.)*

**Questions:**
- Is it when `getAllCellInfo()` returns empty?
- Is it when RSRP drops below a threshold for N seconds?
- Is it when `TelephonyCallback` reports `onServiceStateChanged` to STATE_OUT_OF_SERVICE?

**Fix:** Define the detection heuristic explicitly.

---

### 2.7 Clock Calibration Protocol Is Mentioned But Not Defined

**Problem:** §3.2 mentions the dual timestamp model. §11.3 mentions OWD estimation. *(Corrected, remediation C3: a dangling reference to a GPS-time recommendation in a review not in the repo was removed.)* But the spec itself **never defines a clock calibration protocol**.

The `clock.parquet` file has fields for `estimated_offset` and `uncertainty`, but there's no protocol explaining how these are computed.

**Fix:** Add §11.4 "Clock Calibration Protocol" defining:
- How the UDP probe burst calibration works
- How GPS time is sampled and correlated
- How uncertainty is estimated

---

### 2.8 Port Allocation vs. Flow Configuration

**Problem:** §6.3 Port Allocation says every flow gets dedicated ports, with an example:
```text
Device A: test_001 → 41000–41009
Device B: test_001 → 41010–41019
```

But there's no connection between port allocation and the flow configuration. Does each flow get one port? Multiple ports? Are ports allocated per-flow or per-device?

**Fix:** Define the port allocation strategy:
- One port per flow? (simplest)
- Port range per device? (as shown in example)
- How does the mobile know which port to connect to for each flow?

This should be part of the `RUN_TEST` command payload.

---

## 3. Moderate Issues (Should Fix Before MVP)

### 3.1 Flow Direction Table Is Uninformative

**Problem:** §7.1 Flow Direction table has identical `Server role` and `Mobile role` for all three directions:

| Direction | Server role | Mobile role |
|---|---|---|
| `FromServer` | Generator + receiver + logger | Generator + receiver + logger |
| `ToServer` | Generator + receiver + logger | Generator + receiver + logger |
| `Bidirectional` | Generator + receiver + logger | Generator + receiver + logger |

This table conveys no information. All rows are identical.

**Fix:** Replace with a table showing what actually differs per direction:

| Direction | Payload origin | Payload destination | Connection initiator |
|---|---|---|---|
| `FromServer` | Server | Mobile | Mobile |
| `ToServer` | Mobile | Server | Mobile |
| `Bidirectional` | Both | Both | Mobile |

Or keep the roles table but use checkmarks to show which role is *active* vs *passive* per direction.

---

### 3.2 `header.json` Contents Are Vague

**Problem:** §12.2 says `header.json` contains "schema version, app version, protocol version, device metadata, test configuration, flow configuration" but doesn't list the actual fields.

**Fix:** Provide a concrete `header.json` example, similar to the registration request/response examples.

---

### 3.3 No Database Schema

**Problem:** The server uses `sqlx` (per §4) but there is **no database schema definition** anywhere.

**Tables that clearly exist:**
- Devices (from registration)
- Sessions
- Tests
- Upload state

**But what are their columns?**

**Fix:** Add a database schema section or appendix. At minimum, define the core tables.

---

### 3.4 Server-Client Internal API Is Undefined

**Problem:** §6.4 lists example internal control API commands (`PREPARE_TEST`, `START_GENERATOR`, etc.) but never defines:
- Who sends them? (test-control → data-generator? test-control → server-main?)
- What is the transport? (gRPC? HTTP? Unix socket?)
- What are the payloads?

**Fix:** Define the inter-process communication mechanism and message schemas.

---

### 3.5 Android Logger Component Name Inconsistency

**Problem:** §8.3 uses "traffic generator / receiver" but §16 MVP Scope still says "mobile traffic generator (Realtime UDP, Bulk TCP for ToServer / Bidirectional)" — outdated phrasing that implies the mobile only generates for certain directions.

**Fix:** Update §16 to match §8.3.

---

### 3.6 `Device Status` Endpoint Is Underspecified

**Problem:** §5.3 lists `POST /api/v1/devices/{device_id}/status` but never defines the request body.

**What should the status report include?**
- Current logger state?
- Current test ID?
- Radio snapshot?
- `logging_config_version`?
- Battery level?
- Storage available?

**Fix:** Define the status request schema.

---

### 3.7 Controller App API Is Not Defined

**Problem:** §9 describes the Controller App's purpose and UI components but never defines its API. How does it:
- List devices? (GET /api/v1/devices?)
- Start a test? (POST /api/v1/tests?)
- View status?

**Fix:** Add Controller-facing REST endpoints.

---

### 3.8 No Authentication Anywhere

**Problem:** Authentication is marked as "(future)" in §5.1 and not mentioned elsewhere. For a system that accepts uploads and executes remote commands, this is a significant security gap.

**At minimum for MVP:**
- Device registration should include a pre-shared key or token.
- Upload endpoint should validate the token.
- Controller app should authenticate users.

**Fix:** Document the authentication model, even if it's simple PSK for MVP.

---

## 4. Minor Issues (Polish)

### 4.1 Spec Uses Both "client" and "UE" and "mobile" Interchangeably

**Locations:**
- §2: "Android Logger Clients"
- §7.1: "UE (mobile)"
- §8: "Android Logger App"
- §7.2: "client → server" (UDP probe)

**Fix:** Standardize terminology. Suggestion:
- **UE / Mobile** = the physical Android device (3GPP context)
- **Logger app / Logger** = the Android application
- **Client** = the network client side of a flow (less preferred, ambiguous)

---

### 4.2 `actual_interval_ns` Is Ambiguous

**Problem:** §7.2 says `actual_interval_ns` is "measured interval between consecutive received packets." But:
- Is this measured at the sender or receiver?
- If receiver, is it `t_mono_ns` of packet N minus `t_mono_ns` of packet N-1?
- What about lost packets? Is the interval computed across gaps or only for consecutive sequence numbers?

**Fix:** Define precisely:
```
actual_interval_ns = recv_mono_ns[i] - recv_mono_ns[i-1]
```
where `i` and `i-1` are **consecutive received packets** (not consecutive sequence numbers — gaps are preserved as large intervals).

---

### 4.3 `packets.parquet` Doesn't Include Direction

**Problem:** For bidirectional flows, `packets.parquet` needs to distinguish packets sent vs received. The current fields don't include a direction indicator.

**Fix:** Add `direction` column (`SENT` / `RECEIVED`) or `is_sender` boolean.

---

### 4.4 `events.parquet` Has No Severity or Metadata Columns

**Problem:** Events are just names. But `SIGNAL_LOSS_START` might have a duration, `HANDOVER` might have old/new PCI, etc.

**Fix:** Add `event_metadata` JSON column for event-specific data.

---

### 4.5 Missing File: `network.parquet` or `connection.parquet`

**Suggestion:** Consider a dedicated file for network-layer events:
- Network type changes (NR → LTE → NR)
- IP address changes
- Interface changes (Wi-Fi → mobile)
- These are distinct from radio events and mobility events.

---

## 5. Positive Design Decisions (Preserve These)

| Decision | Why It's Good |
|---|---|
| Dual timestamp model | Correctly separates ordering from correlation |
| Local-first logging | Essential for mobile measurement integrity |
| Server-configured logging | Enables fleet control and progressive enhancement |
| Capability-tolerant design | Realistic given Android API fragmentation |
| UE-initiated connections | Works through NAT/firewall without port forwarding |
| Parquet format | Efficient for columnar time-series data |
| Directory-per-session layout | Human-inspectable and version-controllable |
| Separate processes for server components | Allows independent scaling and failure domains |

---

## 6. Priority Action List

| Priority | Action | Section |
|---|---|---|
| **P0 (Blocker)** | Define test plan / flow configuration schema | New § or §10 |
| **P0 (Blocker)** | Define `RUN_TEST` command payload | §5.2 / §6.3 |
| **P0 (Blocker)** | Define upload protocol (format, compression, retry) | §13 |
| **P1** | Merge or clarify `supported_features` vs `capabilities` | §5.2 |
| **P1** | Clarify Bidirectional TCP (one conn vs two) | §7.2 |
| **P1** | Clarify Bidirectional Realtime UDP (one flow_id vs two) | §7.2 |
| **P1** | Define clock calibration protocol | §11 |
| **P1** | Define `SIGNAL_LOSS` detection heuristic | §12.7 |
| **P1** | Move `neighbor_cells` from derived to logged | §12.4 |
| **P1** | Define device status payload | §5.3 |
| **P2** | Provide concrete `header.json` example | §12.2 |
| **P2** | Define database schema | New § or appendix |
| **P2** | Define inter-process API (test-control ↔ generators) | §6.4 |
| **P2** | Define Controller app REST API | §9 |
| **P2** | Document authentication model (even if PSK) | §5 |
| **P3** | Standardize terminology (UE/mobile/logger/client) | Global |
| **P3** | Add `direction` to `packets.parquet` | §12.3 |
| **P3** | Add `event_metadata` to `events.parquet` | §12.7 |

---

*Review complete. Recommend resolving all P0 and P1 items before implementation begins.*

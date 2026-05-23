# Phase 2 — Android Logger MVP: detailed design

> Companion to `development_plan.md` (Phase 2 scope), `development_guide.md` §5
> (shared core via JNI), and `phase1_detailed_design.md` (the server it talks
> to). The Stage-B spike (`spikes/stage-b-android/`) already proved the hard
> Android risks on a real Pixel 9 Pro: cargo-ndk cross-compile, JNI load under
> ART, native UDP over cellular + NAT, on-device Parquet, foreground-service
> survival. Phase 2 builds the *real* app on those proven foundations.

## 1. Goals & exit criteria

**Goal:** an autonomous Android Logger that enrolls, authenticates, polls for
commands, runs probe / realtime-UDP / bulk-TCP flows (UE-initiated, all
directions) via the shared Rust `core`, logs locally first, and uploads — fully
interoperable with the Phase-1 server.

**Exit criteria:**
1. The shared `core` runs all three flow types **as the UE** and writes the
   contract Parquet, byte-compatible with the server side.
2. **Interop:** a host-run core (the mobile engine, no Android wrapper) drives a
   full test against the real `data-generator` + `server-main` and uploads —
   green in CI. *(This is the device-independent proof of conformance.)*
3. The Android app (Java shell + JNI core) registers, runs a test, logs, and
   uploads against a running server.
4. **On device:** a Pixel 9 Pro completes a full multi-flow test, logs locally,
   uploads. *(Physical; out of scope for the CI/host environment — gated on
   hardware, like the Stage-B spike.)*

### What is verifiable in this (Linux/CI) environment vs. on device

| Verifiable here | Needs the device |
|---|---|
| core UE engine: probe/realtime/bulk, all directions | cellular sockets / NAT punch-through |
| host interop test vs. live server (criterion 2) | Telephony radio sampling (real modem) |
| JNI surface via a desktop-JVM harness (Stage-A style) | GPS sampling, doze/suspend clock |
| Java code compiles (if Android SDK present) | foreground-service longevity, APK on device |

Phase 2 here drives criteria 1–2 to done and writes the app for 3–4; criteria
3–4 are validated when run against hardware.

## 2. Architecture: Java shell + Rust core via JNI

```
┌──────────────────────── Android app process ────────────────────────┐
│  Java shell (foreground service)                                     │
│   ├── REST client  ── enroll/challenge/verify/register/poll/status/  │
│   │                    upload (Retrofit/OkHttp, from openapi.yaml)   │
│   ├── logger state machine (INITIALIZING…UPLOADING…ERROR)           │
│   ├── command poller         radio sampler (Telephony)              │
│   ├── upload manager         gps sampler (Location)                 │
│   └── Keystore signer (P-256 challenge/response)                    │
│                    │  JNI (serde-JSON config blobs)                  │
│   ┌────────────────▼─────────────── shared Rust core (.so) ────────┐│
│   │  traffic engine: probe / realtime_udp / bulk_tcp, UE-initiated  ││
│   │  dual clock + calibration; segmented Parquet logger (SPSC sink) ││
│   │  fed radio/gps rows from the Java shell                         ││
│   └────────────────────────────────────────────────────────────────┘│
└──────────────────────────────────────────────────────────────────────┘
```

**Division of labor (decided, `development_guide.md` §5):**
- **Core (Rust, shared with server):** sockets, dual-clock sampling, packet
  codec, the traffic engine for every flow/direction, the Parquet logger. The
  *measurement* lives here so mobile and server are byte-identical.
- **Java shell:** everything Android-platform: foreground service lifecycle,
  the REST/control client, Keystore-backed signing, Telephony/Location
  sampling, permissions, upload scheduling. It *drives* the core and *feeds* it
  radio/GPS rows; it does not re-implement measurement.

**Logging-first (invariant 1):** the core writes Parquet locally on its own
writer thread; the Java upload manager reads only sealed segment files and
never touches the active test. The two share no lock.

## 3. The JNI boundary (the real contract surface)

The spike used a `spike.RustBridge` namespace and an `init/startFlow/pollStatus/
stop/destroy/sampleClock` surface. Phase 2 promotes this to the real package and
the full Logger needs. All payloads are **serde-JSON blobs** (one authoring
model; `development_guide.md` §10), so the boundary is stable and testable
without Android.

Proposed surface (`RustCore`):

| JNI fn | In (JSON) | Out | Purpose |
|---|---|---|---|
| `init` | `{log_root, device_id, session_id, node:"mobile"}` | handle | open the core for a session |
| `startTest` | `RunTestPayload` (the RUN_TEST blob, verbatim) | test handle | bind sockets, start a flow thread per flow, open the per-test logger |
| `pollStatus` | — | `{per-flow counters, dropped_rows, state}` | progress for the Java status reporter |
| `pushRadio` / `pushGps` | one `RadioRow` / `GpsRow` JSON | ok | Java sampler feeds the core's logger |
| `pushClock` | a `ClockRow` (calibration sample) | ok | clock calibration trace |
| `stopTest` | — | `{summary.json contents}` | stop flows, flush + seal Parquet, return per-node summary |
| `sealedFiles` | — | `[paths]` | what the upload manager may read |
| `destroy` | — | — | release the session |

The `startTest` input is **exactly the contract `RunTestPayload`** the device
received over REST — so the Java shell passes the command payload straight
through; the core already knows the contract types (shared `mp-contracts` shapes
mirrored in core, or core parses the subset it needs).

## 4. Core engine extensions (UE side)

`core/src/lib.rs` today runs the probe flow as the UE (send + receive echo).
Phase 2 generalizes the engine to all flows/directions, mirroring the
server-side `data-generator` logic but from the UE's initiating role:

- **probe:** unchanged (UE originates, server echoes).
- **realtime_udp / FromServer:** UE sends ~1 Hz keepalives until the first
  downlink, then receives + logs with sender timestamps (OWD).
- **realtime_udp / ToServer:** UE sends the fixed-rate stream.
- **bulk_tcp:** UE opens the connection (invariant 5); FromServer → UE reads,
  ToServer → UE writes; one row per chunk.
- A **test** runs many flows concurrently (one real-time thread each), all
  feeding their own SPSC ring → the per-test `mobile/packets.parquet`. radio /
  gps / clock are single-sampler sinks (mobile-only, once per test).

Much of this is the mirror of `server/data-generator/src/flow.rs`; the shared
codec + sink are reused as-is.

## 5. Java components

- **Foreground service** — owns the session; survives backgrounding (the spike
  validated this). Hosts the poller + upload manager.
- **REST client** — generated from `contracts/openapi/openapi.yaml`
  (openapi-generator → Retrofit/OkHttp) so it can't drift from the server.
- **Keystore signer** — non-exportable P-256 keypair (Android Keystore);
  signs the `mp-auth-v1` challenge blob (matches `auth_signing` golden vector).
- **Logger state machine** — `enums.yaml logger_states`.
- **Radio sampler** — `TelephonyManager` / `getAllCellInfo()` → `RadioRow`s
  pushed to the core (Pixel 9 Pro fields only; extended fields stay null).
- **GPS sampler** — `FusedLocationProvider` → `GpsRow`s.
- **Upload manager** — implements the 3-phase protocol against sealed files;
  decoupled from the test path (logging-first).

## 6. Interop strategy (the device-independent proof)

Two host-runnable harnesses, both CI-able:

1. **Core-as-UE interop test** (Rust, the key one): start a real `data-generator`
   + `server-main` in-process (as in `e2e_probe.rs`), then drive the **core
   engine** as the device — enroll/challenge/verify/register over REST (reuse
   the `e2e_probe` flow), receive `RUN_TEST`, run each flow type via the core,
   upload. Asserts the mobile core and the server produce matching, valid logs.
   This is exit criterion 2 and the conformance gate for the eventual APK.
2. **Desktop-JVM JNI harness** (Stage-A style, reusable): a tiny JVM program
   that loads the `.so` and exercises the `RustCore` JNI surface with JSON
   blobs, proving the Java↔core boundary on x86_64 before it runs under ART.

## 7. Sub-phase sequencing

1. **2a** Core UE engine: realtime (both dirs) + bulk (both dirs) alongside the
   existing probe, as the UE. Unit + against a local generator. *(verifiable here)*
2. **2b** Core-as-UE **interop test** vs. live `server-main` + `data-generator`
   (exit criterion 2). *(verifiable here)*
3. **2c** Promote/extend the JNI surface to `RustCore` (startTest from a
   `RunTestPayload`, push radio/gps/clock, stopTest→summary, sealedFiles) +
   desktop-JVM harness. *(verifiable here)*
4. **2d** Android app skeleton: real package, foreground service, generated REST
   client, Keystore signer, logger state machine. *(compiles; runs on device)*
5. **2e** Radio + GPS samplers → core; wire the upload manager. *(device)*
6. **2f** On-device bring-up on the Pixel 9 Pro: full multi-flow test, local
   logs, upload. *(device — exit criteria 3–4)*

## 8. Open decisions to confirm

- **REST client generation:** `openapi-generator` (Retrofit) from the committed
  `openapi.yaml`, vs. a small hand-written OkHttp client. Generation keeps it
  drift-free but adds a codegen step to the Android build.
- **core ↔ contract types:** does `core` depend on `mp-contracts` for the
  `RunTestPayload`/row shapes, or keep core dependency-free and parse a minimal
  subset? (Core is also the Android `.so`; keeping its dep graph lean matters
  for APK size — the spike's `.so` was ~10.6 MB.)
- **min SDK / target:** Pixel 9 Pro is the reference (Android 15); confirm the
  minSdk floor for the fleet.
- **Language:** spike used Java; confirm Java (not Kotlin) for the shell, per
  `development_guide.md`.

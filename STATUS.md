# 5G Field Test Platform — Current Status

> As of 2026-05-24

## Summary

Stage A + Stage B spikes complete, **Phase-0 contracts complete**, **Phase 1 (server + protocol loopback) complete**, and **Phase 2 (Android Logger MVP) complete (2a–2f)**: the shared Rust `core` runs all three traffic types as the UE; the real `RustCore` JNI surface is built; the `android/logger` app builds to a debug APK; and a **full end-to-end test ran on a physical Pixel 9 Pro** — Keystore P-256 auth, RUN_TEST, bulk-TCP flows, logging-first Parquet, and verified upload (see 2f below). `cargo test --workspace` green (26 test suites) + the JVM JNI harness passes. **Phase 4 (analysis) is underway** — the `analysis/` Python package loads real captures and computes RTT/OWD/jitter/loss + derived radio events + a §12.8 summary (15 tests green). Next up: Phase 3 Controller, plus Phase-4 plots + server-node aggregation.

**Phase 4 — analysis toolchain: 🔶 underway.** `analysis/fieldtest_analysis` (see its README): a tolerant test-tree loader (segmented packets, missing files/columns, unknown columns), metrics (probe RTT, realtime OWD-with-uncertainty, jitter/IPDV, per-flow loss), RAT-aware derived events (HANDOVER/BAND_CHANGE/COVERAGE_GAP/NEIGHBOR_COUNT_CHANGE, NR→LTE fallback), §12.8 `summary.json`, and a `fieldtest-analyze` CLI. 15 pytest cases (deterministic synthetic + smoke on two real captures committed as fixtures).
  - **OWD is now offset-corrected** (`clock.py`). The phone↔server clock offset (≈1.15 s on the captures) is estimated and removed, so the summary reports `raw_ns` + `corrected_ns` per direction plus the `offset_ns`/`offset_method`/`offset_uncertainty_ns`. Sources, preferred first: `clock.parquet` `estimated_offset_ns` (disciplined; Logger doesn't emit it yet), else **probe-RTT symmetry** (probe RTT is offset-free, so one-way ≈ RTT/2 under symmetry → yields the offset, ±RTT/2 absolute bound; jitter exact regardless). On `test_udp` (symmetry path) this turns raw downlink p50 ~1153 ms into corrected p50 ~2.8 ms (≈ half the 5.6 ms probe RTT).
  - **Device-side `clock.parquet` writer: ✅ done (UDP-tight).** The Logger's `ClockSampler` calibrates every 5 s during a test, recording `estimated_offset_ns` (mobile−server) + `uncertainty_ns` + `source` into `mobile/clock.parquet`, which seals and uploads automatically. The **preferred path is a UDP NTP-style burst** against server-main's UDP time responder (`MP_TIME_UDP_PORT`, default 8081, advertised by `GET /api/v1/time`): 8 four-timestamp probes per tick, keeping the minimum-delay one (subtracting the server turnaround `t3−t2`); it falls back to single-timestamp `GET /api/v1/time` (`source=server_http`) if UDP is blocked. The analysis picks the **minimum-uncertainty** clock sample (NTP best-practice). Verified live on the Pixel (`test_clocked` fixture): UDP sync cut the offset uncertainty from ~8 ms (HTTP) to **~1.7 ms**, `offset_method=clock_parquet`, corrected downlink OWD now resolved well below the RTT. The disciplined (measured) offset also exposes real path asymmetry the symmetry fallback would mask.
  - **Finding — LTE indoors:** the Pixel logged an LTE anchor (earfcn 1750, lte_rsrp −109 dBm), no NR serving cell — hence the RAT-agnostic serving-cell handling.
  - **Server-node aggregation + uplink OWD: ✅ done.** The data-generator already logged a `server`-node `packets.parquet`, but (a) each flow's writer collided on `packets_NNNN.parquet` (corrupting multi-flow server data) and (b) nothing stopped the generators (run/log forever, ports leaked) and (c) the files sat in test-control's `log_root`, not the aggregated tree. Fixes: per-flow filename stem (`packets_fN_NNNN.parquet`, core `SegmentedPacketWriter::with_stem`); server-main calls test-control `/internal/tests/{id}/stop` on upload finalize (flushes server writers, frees ports); single-host aggregation by pointing test-control's `MP_LOG_ROOT` at server-main's `MP_UPLOAD_ROOT` so `server/` lands beside `mobile/` under one `test_id` (spec §12.1/§13). The analysis loader reads both nodes and now globs flow-scoped segments. Verified live on the Pixel (`test_twonode` fixture): a probe + uplink + downlink test yields `nodes=['mobile','server']`, **uplink OWD corrected p50 3.8 ms (n=1559)** from server RECEIVED rows and downlink p50 4.6 ms — the small directional asymmetry only measurable with both nodes + a disciplined offset.
  - **Remaining:** plots (route/RTT/radio-correlation/packet-timing).

The working tree is a **git repository** (initialized 2026-05-22; `.gitignore` covers Rust `target/`, Python `venv/`, Gradle/Android caches, throwaway keystores, spike runtime artifacts, and built `jniLibs` `.so`). Note: a full debug `target/` reaches ~19 GB — `cargo clean` between large work blocks keeps disk in check.

### Phase 1 — COMPLETE (2026-05-23)

Cargo workspace: `core` (shared engine/codec/logger/auth), `server/contracts-rs` (Rust DTOs+enums, parity-tested), `server/server-main` (axum REST + sqlx), `server/test-control`, `server/data-generator`.

- **1a** workspace + mp-contracts (enums/versions parity gate, DTOs).
- **1b** core reconciliation: framed `(pkt_t,ver)` packets + golden vectors; `core::auth` verify/sign + auth_signing golden vector; full packets Parquet schema + wait-free SPSC logger sink (drop+count).
- **1c** server-main DB (sqlx/SQLite, migration from `db/schema.sql` + drift gate) + device registry.
- **1d** device auth (enroll/challenge/verify) + operator login (HS256 JWT); tokens stored as hashes only.
- **1e** device control plane: register (→ effective LoggingConfig, `effective ⊆ capability`), command poll/ack queue, status upsert.
- **1f** data-generator: probe echo + internal control API + SPSC sink → server Parquet.
- **1g** test-control: per-device state machine, port allocation, generator coordination (HTTP to data-generator).
- **1h** 3-phase resumable upload (summary/manifest → per-file PUT w/ sha256 verify → complete + tree assembly).
- **1i** operator orchestration (`POST /tests`) + **full probe end-to-end integration test** + code-first OpenAPI (`contracts/openapi/openapi.yaml`) with a drift gate.
- **1j** realtime UDP (both directions, OWD/jitter inputs) + bulk TCP (both directions) generators.

Transport is **plain HTTP on loopback** in dev (TLS deferred to Phase-5 hardening, per the Phase-1 design). Auth JWT = HS256.

### Phase 2 — Android Logger MVP: host-verifiable portion COMPLETE (2026-05-24)

Design: `planning/phase2_detailed_design.md`. Built and verified on the host (no device required):

- **2a** `core/src/ue.rs` — the UE-side traffic engine (mirror of the server data-generator from the initiating side): probe, realtime UDP (both directions, keepalive/NAT-punch + OWD inputs), bulk TCP (both directions). One SPSC ring per flow → one `mobile/packets.parquet` via `spawn_packet_writer_multi`. Lean serde config (no `mp-contracts` dep → small `.so`).
- **2b** `server/server-main/tests/ue_interop.rs` — **exit criterion 2**: the `core` UE engine drives a 5-flow test against the REAL server-main + test-control + data-generator and both sides log; the device-independent conformance gate the APK must pass.
- **2c** `RustCore` JNI surface (`com.measurement.logger.RustCore`: coreNew/startTest/pollStatus/stopTest/sealedFiles/sampleClock/destroy) over a pure-Rust handle API; legacy spike `RustBridge` removed. `core/jni_harness/` desktop-JVM harness loads the real `.so` and drives a probe through the JNI boundary — **passes on host JDK 17**.

Two contract seams surfaced + resolved: `session_id` now travels in `RUN_TEST`; the upload manifest path regex accepts segmented `<table>_NNNN.parquet`.

- **2e (core)** gps/clock/radio Parquet writers (radio neighbor_cells = list<struct>) + `pushGps/pushClock/pushRadio` JNI; the desktop-JVM harness drives all of it. The core's full log surface is complete.
- **2d + 2e (app)** `android/logger` — the real Logger (namespace `com.measurement.logger`): `RustCore` JNI bridge, Keystore P-256 signer (mp-auth-v1), OkHttp `ServerClient` (control plane + 3-phase upload), `LoggerService` foreground orchestration (onboard → poll → run RUN_TEST via the core → upload), Telephony `RadioSampler` + `LocationManager` `GpsSampler`, `MainActivity`. **Builds to a debug APK** (`android/build_apk.sh`: cargo-ndk core → assembleDebug) packaging `libfieldtest_core.so` (arm64, ~11 MB) — verified with the installed SDK/NDK here.

**2f — on-device bring-up: ✅ DONE** (Pixel 9 Pro `46171FDAP001LK`, run 2026-05-24). Step-by-step runbook (servers, `mp-admin`, adb onboarding, LAN vs USB, gotchas): [`planning/device_bringup.md`](planning/device_bringup.md). Full e2e over USB (`adb reverse` for the control plane + generator TCP ports): `libfieldtest_core.so` loads cleanly under ART; Keystore P-256 enroll → challenge/response auth → register → IDLE; operator (`mp-admin`-minted token + operator) starts a 2-flow `bulk_tcp` test; RUN_TEST delivered+acked; both flows ran; logging-first Parquet written to app-private storage (`mobile/packets_0000.parquet`, `radio.parquet` with live NR cell metrics) **then** 3-phase-uploaded to the server (`uploads.state=COMPLETE`, sha256 verified). Tooling added this run: `mp-admin` (`mint-token`/`add-operator`/`list-devices`), `server-main` now reads `MP_SERVER_HOST`/`MP_TEST_CONTROL_URL`/`MP_UPLOAD_ROOT`, `MainActivity` accepts `server`/`token` intent extras (adb-drivable), and a `network_security_config.xml` permitting cleartext (plain-HTTP dev server; TLS is Phase 5). Follow-up LAN runs (phone + server on Wi-Fi, `MP_SERVER_HOST=192.168.x.x`) then exercised what USB couldn't: a **UDP** test (probe echo round-trip + realtime `FromServer` keepalive punch-through, ~3k packet rows) and a **bidirectional** 4-flow test (`bulk_tcp` + `realtime_udp`, each paired `ToServer`+`FromServer`) — both DONE/COMPLETED with live radio + a GPS fix, dual-clock fields populated.
  - **Test-state machine: ✅ fixed.** `tests.state` now advances `WAITING_FOR_CLIENT → RUNNING` (device `post_status` with `logger_state=RUNNING_TEST`) and `→ DONE`/`FAILED` with `result` + `ended_at` on upload `complete` (`tests_api::mark_running` / `finalize_from_upload`). Covered by `upload_flow.rs` and re-verified live on the Pixel (`WAITING_FOR_CLIENT → DONE`, result `COMPLETED`).
  - **Follow-up (Phase 5 robustness):** the Logger's poll loop is inside one outer try/catch, so a *transient* network error (e.g. server restart) throws out of the `while`, ends the worker, and the service stops polling until the app is force-restarted. Per-iteration error handling + reconnect/backoff should keep the worker alive across blips.
  - **Not exercised over USB:** UDP flows (probe/realtime) — `adb reverse` is TCP-only. Needs the phone + server on the same LAN (add the server's 192.168.x.x to `network_security_config.xml`). GPS produced no fix indoors (no `gps.parquet`), as expected.

**Long-term direction:** The platform is designed to grow from a "test instrument" into a continuous network-quality probe that logs real app traffic and monitors latency to arbitrary destinations. See `planning/long_term_traffic_monitoring.md` for the growth roadmap. The MVP architecture (shared Rust core, segmented Parquet, dual clocks, server-configured logging) is intentionally built to support this without rewrite.

---

## Completed Work

### Phase 0 — Foundations & Contracts (partial)

#### Stage A: JNI Viability Spike (desktop JVM / Linux x86_64) — COMPLETE

**Location:** `spikes/stage-a-harness/`

| Check | Result | Key Measurement |
|-------|--------|-----------------|
| JNI binding + serde config round-trip | ✅ PASS | `System.loadLibrary` loads Rust `cdylib`; JSON config parsed by `serde_json` |
| UDP probe echo on localhost | ✅ PASS | 49 sent → 49 received (~100% on localhost) |
| Resilience under blackout | ✅ PASS | Peer killed 3 s mid-run; core kept logging (399 sent, 339 received); 15 Parquet segments closed |
| Dual clock agreement | ✅ PASS | `mono_ns` delta Rust↔Java ≤ 1.2 ms; `utc_ns` delta ≤ 2 ms over 100 samples |
| Segmented Parquet writer | ✅ PASS | zstd-compressed; readable by pyarrow 24 and arrow-rs 54 |
| SIGKILL mid-write survival | ✅ PASS | 4 closed segments fully readable; 1 open segment empty (loss ≤ 1 segment) |
| Footprint (x86_64 proxy) | ✅ RECORDED | `libfieldtest_core.so` release = **11 MB** |

**Artifacts:**
- `core/` — shared Rust library (`cdylib` + `rlib`) with dual-clock timestamps, probe packet codec, segmented Parquet writer, JNI bindings, UDP probe engine
- `server/` — placeholder crate (only `main.rs` stub)
- `spikes/stage-a-harness/` — peer echo server, Java JNI harness, test scripts (`run.sh`, `test_resilience.sh`, `test_sigkill.sh`)
- `spikes/stage-a-harness/FINDINGS.md` — Stage A findings note

#### Stage B: Android Logger App (Pixel 9 Pro) — COMPLETE

**Location:** `spikes/stage-b-android/`

**Completed:**
- `cargo-ndk` + NDK r27c installed; `rustup target add aarch64-linux-android`
- Minimal Android logger app with foreground service (`LoggerService`), `MainActivity`, JNI wrapper
- Cross-compiled `libfieldtest_core.so` for `arm64-v8a`
- Signed release APK built and verified (APK Signature Scheme v2)
- Emulator tests passed (ART load, UDP echo via virtwifi, Parquet FS write, 5-min FG service longevity)
- **All physical Pixel 9 Pro tests passed** — see `spikes/stage-b-android/FINDINGS.md`

**APK measurements:**
- arm64 `.so` release: **10.6 MB** inside APK
- Total signed release APK: **20 MB**
- Installed app on Pixel 9 Pro: **13 MB**
- Target budget: ≤ ~12 MB arm64 `.so` → **within budget**

**Artifacts:**
- `spikes/stage-b-android/` — Android project (Gradle, manifest, service, activity)
- `spikes/stage-b-android/build_release.sh` — one-command signed release build
- `spikes/stage-b-android/FINDINGS.md` — Stage B findings (emulator + physical)

**Physical test results:**
| Check | Result |
|-------|--------|
| Cellular sockets + NAT | ✅ 599/599 packets, 100% delivery through Tele2 CGNAT |
| Wi-Fi ↔ cellular handover | ✅ No crash; service survived; socket doesn't auto-recover (known spike gap) |
| Android clock across suspend/doze | ✅ `mono_ns` monotonic across 5,897 rows |
| FG service OS-kill survival | ✅ Closed segments readable after force-stop |
| Real APK size on device | ✅ 13 MB installed |
| FG longevity (≥5 min, real) | ✅ Completed without OS kill |

#### Contracts (`contracts/`) — COMPLETE 2026-05-23

The single-source-of-truth boundary definitions. Every schema ships with golden vectors / examples that are validated (JSON Schema validation, hex round-trip, or SQL execution).

| Contract | Files | Notes |
|----------|-------|-------|
| Versions + auth constants | `versions/versions.yaml` | protocol/schema/auth_scheme versions; AUTH_* constants; bump policy |
| Enums | `enums/enums.yaml` | commands, 3 state machines, flow types, direction (FromServer/ToServer — Bidirectional dropped), roles, platforms, event types (+ 3-way provenance), test results |
| Wire packets | `packets/{README,probe,realtime}.md` + `testvectors/packets/*.json` | `(pkt_t, ver)` framing, no runtime extensibility — grow by version bump; Probe v1 + Realtime v1; vectors round-trip |
| Test plan / RUN_TEST | `schemas/{test_plan,run_test_command}.schema.json` + `testvectors/commands/` | **port-less TestPlan** (single Flow source); ports as `flow_bindings`; bidirectional = two flows |
| Capability + logging config | `schemas/{capability,logging_config}.schema.json` + testvectors | `supported_features` dropped; effective ⊆ capability |
| Upload protocol | `upload/README.md` + `schemas/upload_{manifest,init_response}.schema.json` + testvectors | 3-phase resumable per-file; test_result + PARTIAL rule (loss ≠ PARTIAL) |
| Log schemas | `parquet/*.yaml` (packets/radio/gps/clock/events) + `schemas/{header,summary}.schema.json` + `testvectors/log/` | dual clock, join columns, nullable capability fields, one JSON `event_metadata` column |
| Device status | `schemas/device_status.schema.json` + testvectors | POST /status heartbeat (logger_state, config version, battery, storage, radio snapshot) |
| Controller REST | `schemas/create_test_request.schema.json` + `planning/controller_openapi_sketch.yaml` | operator surface (list/create/stop/status/upload); port-less create-test body |
| Internal control IPC | `internal/{control_api,logger_sink}.md` + `schemas/internal_*.json` + testvectors | **local HTTP+JSON not gRPC**; generator→logger SPSC ring (drop+count) |
| DB schema | `db/schema.sql` + `db/README.md` | server-main SQLite (WAL), 12 tables, enum CHECKs; executes + constraint-tested |
| Auth + controller endpoints (design sketches) | `planning/{auth,controller}_openapi_sketch.yaml` | to be replaced by utoipa codegen in Phase 1 |

**Key decisions locked in this phase:**
- **Auth:** device = enrollment + per-session challenge/response over a non-exportable P-256 keypair (Android Keystore / iOS Secure Enclave), `ECDSA_P256_SHA256` / SPKI / ASN.1 DER pinned cross-platform; operator = username/password → JWT. Spec §5.3, dev-plan §7, dev-guide §8, AGENTS.md updated.
- **Wire formats grow by version bump, not runtime extensibility** (no TLV tails) — closed monorepo, lockstep release.
- **Port-less TestPlan + flow_bindings**: the operator authors the recipe, the server owns the bindings — kills the flow-definition duplication.
- **Real-time data path:** generator + logger = one process / separate threads on both nodes (forced on mobile by JNI); SPSC ring handoff with drop+count overflow; control IPC is local HTTP+JSON (gRPC dropped).

**Known contract→code drift** (Phase-1 reconciliation): `core/src/packet.rs` (unframed 20-byte probe) and `core/src/log_writer.rs` (6-column packets schema) predate the contracts and must be brought up to the framed packet layout and full packets schema.

---

## Codebase Structure

```
/
├── Cargo.toml                    # Rust workspace
├── core/                         # SHARED Rust engine+logger lib
│   ├── Cargo.toml                # cdylib + rlib
│   └── src/
│       ├── lib.rs                # JNI exports, CoreState, UDP probe flow
│       ├── timestamp.rs          # Dual-clock sampling (CLOCK_MONOTONIC + REALTIME)
│       ├── packet.rs             # Probe packet codec (20-byte big-endian)
│       └── log_writer.rs         # Segmented Parquet writer (zstd)
├── server/                       # Server placeholder
│   ├── Cargo.toml
│   └── src/main.rs               # Stub
├── spikes/
│   ├── stage-a-harness/          # Desktop-JVM spike (kept → feeds Phase 1)
│   │   ├── Cargo.toml            # spike-peer echo server
│   │   ├── peer.rs               # UDP echo peer
│   │   ├── java/spike/           # Java JNI harness
│   │   │   ├── RustBridge.java
│   │   │   ├── StageAHarness.java
│   │   │   ├── DualClockTest.java
│   │   │   ├── ResilienceHarness.java
│   │   │   └── SigkillHarness.java
│   │   ├── run.sh                # Smoke test
│   │   ├── test_resilience.sh    # Blackout chaos test
│   │   ├── test_sigkill.sh       # SIGKILL survival test
│   │   └── FINDINGS.md           # Stage A findings
│   └── stage-b-android/          # Android wrapper (throwaway spike)
│       ├── build.gradle          # Root project
│       ├── settings.gradle
│       ├── local.properties
│       ├── gradle.properties
│       ├── keystore.jks          # Self-signed (spike only)
│       ├── build_release.sh      # One-command release build
│       ├── logger/build.gradle   # App module
│       └── logger/src/main/
│           ├── AndroidManifest.xml
│           ├── java/spike/
│           │   ├── MainActivity.java
│           │   ├── LoggerService.java
│           │   └── RustBridge.java
│           ├── res/layout/activity_main.xml
│           └── jniLibs/arm64-v8a/libfieldtest_core.so
├── planning/                     # Architecture docs (unchanged)
│   ├── 5_g_field_test_platform_architecture_spec.md
│   ├── development_plan.md
│   ├── development_guide.md
│   ├── design_review.md
│   ├── review_remediation_plan.md
│   └── update_after_review/
├── AGENTS.md                     # Agent guide (updated with build commands)
├── CLAUDE.md                     # Original project briefing
└── venv/                         # Python virtualenv (pyarrow)
```

---

## Technology Stack Verified

| Component | Technology | Version | Status |
|-----------|------------|---------|--------|
| Server language | Rust | 1.94.1 | ✅ active |
| Android language | Java + JNI | 17 | ✅ active |
| Android build | Gradle + AGP | 8.13 / 8.4.2 | ✅ active |
| Android NDK | r27c | 27.2.12479018 | ✅ installed |
| Async runtime | tokio | 1.52.3 | ✅ in deps |
| Serialization | serde + serde_json | 1.0.228 | ✅ working |
| JNI | jni crate | 0.21.1 | ✅ working |
| Parquet writer | arrow + parquet | 54.3.1 | ✅ working |
| Time | chrono + libc | 0.4.44 / 0.2.186 | ✅ working |
| Analysis | Python + pyarrow | 3.12 / 24.0.0 | ✅ installed |

---

## Pending Work (by Phase)

### Phase 0 — Contracts (COMPLETE; see Completed Work)

**All remediation contract items resolved:** 1.1, 1.2 (test plan + RUN_TEST), 1.3 (upload protocol), 1.4 (`supported_features` dropped), 2.1 (header metadata + parquet join columns), 2.2/2.3 (bidirectional = two flows), 2.8 (per-flow port via flow_bindings), 3.3 (DB schema), 3.4 (internal control API), 3.6 (device status), 3.7 (controller REST), 4.2/4.3/4.4 (interval/direction/event_metadata), M3 (version bump policy), M4b (test_result + PARTIAL rule).

All Phase-1-coupled contract remnants are now resolved (utoipa OpenAPI + drift gate, auth_signing vector, spec §12 rename, core reconciliation).

### Phase 1 — Server skeleton + protocol loopback (COMPLETE; see Summary)

**CI gate wired** (2026-05-23): `ci/check.sh` (and `.github/workflows/ci.yml`) run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` (the test suite also enforces enum/version parity, packet + auth golden vectors, OpenAPI drift, DB-migration drift, and the probe e2e). `ci/validate_contracts.py` validates every `contracts/` example + packet vector. The whole workspace is fmt-clean and clippy-clean (`-D warnings`).

Deferred (optional polish, not blocking Phase 2):
- Standalone `test-client` binary + `scripts/e2e_probe.sh` (the in-process integration test is the authoritative proof; a CLI client + shell wrapper is convenience for manual runs).
- Per-connection real-time tuning (CPU pinning / SCHED_FIFO) — deferred to hardening.

### Phase 2 — Android Logger MVP (2a–2e COMPLETE; 2f on-device only; see Summary)

UE engine, server interop, RustCore JNI + aux writers, and the full Android Logger app (builds to an APK) are done. Only 2f — the physical Pixel 9 Pro run — remains.

### Phase 3 — Android Controller (not started)

### Phase 4 — Analysis Toolchain (not started)

### Phase 5 — Hardening (not started)

---

## Known Issues / Risks

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Stage B fails on physical Pixel (cellular/Parquet/JNI) | Medium | High | Fallbacks documented: Java sockets, Arrow-IPC, or drop shared-core |
| Contracts take longer than expected | Medium | High | Spike code can be refactored; no production code blocked yet |
| Parquet .so size grows with features | Medium | Medium | Currently 11 MB; monitor; consider feature flags or stripped builds |
| Arrow/Parquet on Android API 28 | Low | High | Tested on emulator API 36; need API 28 verification |

---

## Next Recommended Steps

Spikes, Phase 0, Phase 1, and the host-verifiable part of Phase 2 (2a–2c) are complete. Options, in priority order:

1. **Phase 2 finish — on the device (2d–2f).** Needs Android SDK/Gradle + a Pixel 9 Pro. 2d: app skeleton (foreground service, REST client generated from `openapi.yaml`, Keystore P-256 signer, logger state machine). 2e: radio/gps/clock Parquet writers + `push*` JNI + Telephony/Location samplers. 2f: on-device full test → local logs → upload. Conformance targets already in place: `ue_interop.rs` + `core/jni_harness` + the generated OpenAPI.
2. **Phase 4 — Analysis toolchain (Python)** can run in parallel here (no device): tolerant Parquet loaders against `parquet/*.yaml`; OWD-with-uncertainty, RTT/jitter/loss; against real uploaded sessions.
3. **Phase 3 — Android Controller** — Kotlin client from `contracts/openapi/openapi.yaml`.
4. **Deferred polish:** `test-client` bin + `e2e_probe.sh`; real-time CPU pinning (Phase-5).

---

## Build Commands (Quick Reference)

```bash
# Rust workspace
cargo build --workspace
cargo test --workspace
cargo build --workspace --release

# Stage A desktop tests
./spikes/stage-a-harness/run.sh
./spikes/stage-a-harness/test_resilience.sh
./spikes/stage-a-harness/test_sigkill.sh

# Stage B Android signed release
./spikes/stage-b-android/build_release.sh
adb install -r spikes/stage-b-android/logger/build/outputs/apk/release/logger-release.apk

# Android cross-compile (manual)
cargo ndk -t arm64-v8a build --release
```

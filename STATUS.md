# 5G Field Test Platform — Current Status

> As of 2026-05-23

## Summary

Stage A + Stage B spikes complete, **Phase-0 contracts complete**, and **Phase 1 (server + protocol loopback) complete**: the full server-side control plane and all three traffic types run end to end on localhost, proven by an in-process integration test, with no Android needed. `cargo test --workspace` green (23 test suites). The Rust server is real; the Android apps (Phase 2/3) and analysis toolchain (Phase 4) are not yet built.

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

Deferred (optional polish, not blocking Phase 2):
- Standalone `test-client` binary + `scripts/e2e_probe.sh` (the in-process integration test is the authoritative proof; a CLI client + shell wrapper is convenience for manual runs).
- Clippy cleanup pass for a `-D warnings` CI gate (a few style lints remain in core 1b code).
- Per-connection real-time tuning (CPU pinning / SCHED_FIFO) — deferred to hardening.

### Phase 2 — Android Logger MVP (not started)

Real Android logger app (not the throwaway spike). Depends on:
- Phase 0 contracts
- Stage B green on physical Pixel

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

Spikes, Phase 0, and Phase 1 are complete. The server is the reference implementation the mobile must match. Options, in priority order:

1. **Phase 2 — Android Logger MVP.** Integrate the shared Rust `core` via JNI (sockets + dual clock + packet codec + SPSC logger sink), wrap it in the Java foreground service + control/REST client, and interop-test against the Phase-1 server. The synthetic-client integration test (`e2e_probe.rs`) + the generated OpenAPI are the conformance targets. Exit: a Pixel 9 Pro runs a full test, logs locally, uploads.
2. **Phase 4 — Analysis toolchain (Python)** can start in parallel: tolerant Parquet loaders against the `parquet/*.yaml` schemas; OWD-with-uncertainty, RTT/jitter/loss; uses real uploaded sessions from the server.
3. **Phase 3 — Android Controller** (operator UI) — generate a Kotlin client from `contracts/openapi/openapi.yaml`.
4. **Phase-1 polish** (deferred): `test-client` bin + `e2e_probe.sh`; clippy `-D warnings` gate.

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

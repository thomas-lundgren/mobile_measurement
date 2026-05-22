# 5G Field Test Platform — Current Status

> As of 2026-05-22

## Summary

The project has moved from **planning-only** to **active implementation**. Stage A (desktop JVM) and Stage B (Pixel 9 Pro physical device) are both complete — all checks pass. The **Phase-0 contracts are now substantially authored** (versions, enums, packets, test plan / RUN_TEST, capability + logging config, upload protocol, log schemas), each with validated golden vectors / examples. No production server or Android logger code exists yet — only the spike artifacts, the contracts, and a skeletal workspace.

The working tree is now a **git repository** (initialized 2026-05-22; `.gitignore` covers Rust `target/`, Python `venv/`, Gradle/Android caches, throwaway keystores, spike runtime artifacts, and built `jniLibs` `.so`).

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

#### Contracts (`contracts/`) — DRAFTED 2026-05-22

Authored the single-source-of-truth boundary definitions. Every schema ships with golden vectors / examples that are validated (JSON Schema validation or hex round-trip).

| Contract | Files | Notes |
|----------|-------|-------|
| Versions + auth constants | `versions/versions.yaml` | protocol/schema/auth_scheme versions; AUTH_* constants; bump policy |
| Enums | `enums/enums.yaml` | commands, 3 state machines, flow types, direction (FromServer/ToServer — Bidirectional dropped), roles, platforms, event types (+ provenance), test results |
| Wire packets | `packets/{README,probe,realtime}.md` + `testvectors/packets/*.json` | `(pkt_t, ver)` framing, no runtime extensibility — grow by version bump; Probe v1 + Realtime v1; vectors round-trip |
| Test plan / RUN_TEST | `schemas/{test_plan,run_test_command}.schema.json` + `testvectors/commands/` | per-flow server_port; bidirectional = two flows |
| Capability + logging config | `schemas/{capability,logging_config}.schema.json` + testvectors | `supported_features` dropped; effective ⊆ capability |
| Upload protocol | `upload/README.md` + `schemas/upload_{manifest,init_response}.schema.json` + testvectors | 3-phase resumable per-file; test_result + PARTIAL rule (loss ≠ PARTIAL) |
| Log schemas | `parquet/*.yaml` (packets/radio/gps/clock/events) + `schemas/{header,summary}.schema.json` + `testvectors/log/` | dual clock, join columns, nullable capability fields, one JSON `event_metadata` column |
| Auth endpoints (design sketch) | `planning/auth_openapi_sketch.yaml` | enrollment + challenge/response (P-256 Keystore/Secure Enclave, cross-platform); to be replaced by utoipa codegen |

**Auth model decided:** device auth is enrollment + per-session challenge/response over a non-exportable P-256 keypair (Android Keystore / iOS Secure Enclave), `ECDSA_P256_SHA256` / SPKI / ASN.1 DER pinned for cross-platform interop. Operator auth is username/password → JWT. Spec §5.3, dev-plan §7, dev-guide §8, AGENTS.md all updated.

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

### Phase 0 — Contracts (mostly drafted; see Completed Work)

**Resolved this session:** 1.1, 1.2 (test plan + RUN_TEST), 1.3 (upload protocol), 1.4 (`supported_features` dropped), 2.1 (header metadata + parquet join columns), 2.2/2.3 (bidirectional = two flows), 2.8 (per-flow port), 4.2/4.3/4.4 (interval/direction/event_metadata), M3 (version bump policy), M4b (test_result + PARTIAL rule).

**Still open in Phase 0:**
- 3.6 Device `status` payload (`POST /status` body)
- 3.7 Controller-facing REST endpoints (P1 — blocks Controller MVP)
- 3.4 Internal test-control ↔ data-generator API
- 3.3 DB schema (sqlx migrations)
- `testvectors/auth_signing/` cross-platform ECDSA golden vector
- OpenAPI assembly via `utoipa` (lands with the Phase-1 server; replaces `planning/auth_openapi_sketch.yaml`)
- Reconcile spec §12 timestamp naming (`t_mono_ns` → `mono_ns`)

### Phase 1 — Server Skeleton (not started)

- `server-main`: REST endpoints, device registry, command queue, persistence (sqlx)
- `test-control`: per-device state machine, port allocation, generator lifecycle
- `data-generator`: UDP probe echo first, then realtime UDP, then bulk TCP
- Synthetic test client

**Blocked by:** Phase 0 contracts.

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

Spike gates are green and the core contracts are drafted. Remaining options, roughly in priority order:

1. **Finish Phase-0 control-plane contracts** — device `status` payload (3.6), controller REST endpoints (3.7, blocks Controller MVP), internal test-control ↔ data-generator API (3.4).
2. **Begin Phase 1 server skeleton** — wire `axum` + `utoipa` into `server/`, which produces the authoritative `contracts/openapi/openapi.yaml` and lets the auth/test-plan/upload schemas become live Rust DTOs. Reconcile `core/packet.rs` + `core/log_writer.rs` to the drafted packet/log contracts as part of this.
3. **Add the `auth_signing` golden vector** so the Rust verifier and (later) the Android signer are pinned against the DER-vs-P1363 interop bug.

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

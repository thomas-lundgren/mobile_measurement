# 5G Field Test Platform — Current Status

> As of 2026-05-20

## Summary

The project has moved from **planning-only** to **active implementation**. Stage A (desktop JVM) and Stage B (Pixel 9 Pro physical device) are both complete — all checks pass. No production server or Android logger code exists yet — only the spike artifacts and a skeletal workspace.

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

### Phase 0 — Contracts (not started)

Blocked: the P0 contracts (`test_plan` schema, `RUN_TEST` payload, upload protocol) are still undefined. The spike uses ad-hoc JSON. Before `test-control` or the mobile test runner can be built, these must be authored in `contracts/`.

**Items from `review_remediation_plan.md`:**
- 1.1 Test plan / flow configuration format
- 1.2 `RUN_TEST` command payload
- 1.3 Upload protocol (body, compression, resume)
- 1.4 `supported_features` vs `capabilities` cleanup
- 2.8 Port allocation connected to flow config
- M3 Versioning/compatibility policy
- M4b Test-result reporting endpoint

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

1. **Run Stage B on physical Pixel 9 Pro** — use `spikes/stage-b-android/build_release.sh` and follow `spikes/stage-b-android/FINDINGS.md`
2. **If Stage B passes** → begin Phase 0 contracts authoring (`contracts/` directory, OpenAPI, packet layout, log schemas)
3. **If Stage B fails** → execute fallback from `development_plan.md` §Step 0
4. **Parallel:** Server placeholder can begin skeleton work using ad-hoc fixtures (not blocked by contracts for basic UDP echo)

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

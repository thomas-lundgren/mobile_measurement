# Stage A — JNI Viability Spike Findings

> Desktop JVM / Linux x86_64. Physical Pixel Stage B is not covered here.

## Checklist & Results

| # | Check | Result | Measurement / Note |
|---|-------|--------|--------------------|
| 1 | JNI binding loads, serde config round-trips | **PASS** | `System.loadLibrary("fieldtest_core")` OK; JSON config parsed by `serde_json` inside Rust |
| 2 | UDP probe echo on localhost | **PASS** | 49 sent → 49 received (~100% on localhost) |
| 3 | Resilience under blackout | **PASS** | Peer killed for 3 s mid-run; core kept logging; 399 sent, 339 received; 15 Parquet segments closed |
| 4 | Dual clock agreement | **PASS** | `mono_ns` delta Rust↔Java ≤ 1.2 ms; `utc_ns` delta ≤ 2 ms over 100 samples |
| 5 | Segmented Parquet writer | **PASS** | Valid zstd-compressed Parquet; readable by pyarrow 24 and arrow-rs 54 |
| 6 | SIGKILL mid-write survival | **PASS** | 4 closed segments fully readable; 1 open segment empty (loss ≤ 1 segment) |
| 7 | Footprint (x86_64 proxy) | **RECORDED** | `libfieldtest_core.so` release = **11 MB** (target/debug = ~100+ MB) |

## RTT / Performance (localhost)

- Rate: 10 pps, 64-byte payload
- Observed: ~100% echo delivery on localhost
- No systematic latency measurement yet (out of scope for Stage A; will be measured on real network in Stage B)

## Architecture boundary — unchanged

No fallback needed. All Stage A criteria pass. The shared-core-over-JNI architecture is viable for the desktop/Linux side.

## Known gaps / Stage B risks

| Risk | Stage A coverage | Stage B confirmation needed |
|------|------------------|----------------------------|
| Cross-compile to `arm64-v8a` | N/A (desktop only) | `cargo-ndk` build + ART load |
| Cellular sockets + NAT | N/A (localhost only) | UDP echo over mobile data |
| Android FS / Parquet on-device | N/A | Write to app-private storage on Pixel |
| Foreground service longevity | N/A | ≥5-min run without OS kill |
| Doze / suspend clock behavior | N/A | Dual clock across suspend |
| Real APK size delta | N/A | Measure `.so` inside APK (target ≤ ~12 MB arm64) |

## Go / No-Go

**GO for Phase 1 (server skeleton + protocol loopback)** — the Stage-A core is kept and feeds Phase 1.

**Stage B must pass before Phase 2 (Android engine)**.

# Stage B — JNI Viability Spike Findings

> Pixel 9 Pro (physical device) + emulator

## Physical Pixel 9 Pro Tests — COMPLETE

| # | Check | Result | Measurement |
|---|-------|--------|-------------|
| B1 | Cellular sockets + NAT | **PASS** | 599 sent → 599 received (100%) over Tele2 LTE/CGNAT |
| B2 | Wi-Fi ↔ cellular handover | **PASS** | No crash; service stayed alive; "Network unreachable" errors logged; socket bound to old interface (known spike limitation) |
| B3 | Android clock across suspend/doze | **PASS** | `mono_ns` monotonic across 5,897 rows; zero violations |
| B4 | FG service OS-kill survival | **PASS** | Force-stop mid-run → closed segments readable; last segment empty (≤1 segment loss) |
| B5 | Real APK size | **RECORDED** | Installed app = **13 MB** total |
| B6 | FG longevity (real) | **PASS** | 5-minute run completed without OS kill |

## Emulator tests (verified in CI)

| # | Check | Result | Measurement |
|---|-------|--------|-------------|
| 1 | Cross-compile (`cargo-ndk`) | **PASS** | `aarch64-linux-android` target builds cleanly; release `.so` = **11 MB** |
| 2 | ART load + JNI call | **PASS** | APK installs; `System.loadLibrary("fieldtest_core")` succeeds; `init` returns handle |
| 3 | Sockets (virtwifi → host) | **PASS** | UDP echo 10 pps for 5 min: 100% delivery on emulator virtual network |
| 4 | Android FS + Parquet | **PASS** | `packets.parquet` written to `/sdcard/Android/data/<pkg>/files/`; valid pyarrow read-back |
| 5 | FG service longevity (emulator) | **PASS** | 5-minute run completed without crash; service remained in foreground |

## Network setup notes

The router (EdgeRouter 6P) required:
1. **Destination NAT rule** (`type destination`, `protocol udp`, `inside-address 192.168.1.65:41000`, `inbound-interface eth5`)
2. **Firewall rule** in `WAN_IN` to accept new UDP to 192.168.1.65:41000
3. **No source address restriction** on the NAT rule (carrier CGNAT IPs are invisible to the router)

Host machine `ufw` was inactive; no local firewall blocking.

## Measurements

- **arm64 `.so` release size:** 11 MB
- **Debug APK total:** 22 MB
- **Release APK total:** 20 MB
- **Installed app on Pixel 9 Pro:** 13 MB
- **Target budget:** ≤ ~12 MB arm64 `.so` → **within budget**

## Go / No-Go

**GO for Phase 1 (server skeleton + protocol loopback).**

All Stage B physical checks pass. The shared-core-over-JNI architecture is viable on the reference device (Pixel 9 Pro). The one limitation found — UDP socket not auto-recovering on network handover — is a known gap for production but does not block the architecture decision; it is addressed by adding `ConnectivityManager` network-change handling in the Java shell.

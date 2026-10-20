# Over-the-air Logger updates and versioned logs — plan

Status: planned 2026-10-20. Owner: operator (Thomas) + Claude.
Related: spec §12 (log layout), `contracts/versions/versions.yaml`, invariants 3–4 in `CLAUDE.md`.

## 1. Goals

1. **Update the Logger without USB.** A new build reaches the Pixel over the air
   (home Wi-Fi or mobile data) and installs itself, with no tap after the first
   update.
2. **Every log says which software wrote it.** Each test records the exact
   Logger build (and the server build) that produced it, so results can be
   grouped, compared or excluded per build.
3. **Updates never break measurement or old data.**
   - An update never interrupts a test or a pending upload (logging-first,
     invariant 1).
   - Tests recorded before an update still upload and analyse.
   - A plan is only sent to a Logger that can run it.

Non-goals:
- Updating the on-phone helper `mp-probe`. It needs adb to start (shell
  uid), so it stays a USB task, and it rarely changes.
- Updating Android itself.
- Fully silent first install. That needs device-owner mode, which only works
  on a phone with no accounts — not realistic on a personal phone.

## 2. Where we are (2026-10-20)

| Item | State |
|---|---|
| `versions.yaml`: `protocol_version`, `schema_version`, `auth_scheme_version` | Defined (all 1). Protocol checked at enroll/verify. |
| `header.json` per test, with `app_version`, `schema_version`, `protocol_version`, plan, bindings | **Specified** (`contracts/schemas/header.schema.json`) **but not written** by the Logger or the server; uploaded tests have no header. |
| `summary.json` per node | Specified, not written. |
| Logger version | `versionCode 1` / `versionName "0.1.0"` forever (`android/logger/build.gradle`). Not reported anywhere. |
| Log readers | Forward compatible: unknown columns ignored, missing read as null (invariant 4; `analysis/fieldtest_analysis/loader.py`). |
| Capability | Enrollment `Capability` has radio/QoS/mobility/timestamp sources, `supports_ping`; **no list of supported flow types**. |
| Install path | `adb install -r` over USB only (`android/build_apk.sh`). Signed with the debug key of this machine. |
| Commands | `RUN_TEST`, `STOP_TEST`, `SET_LOGGING_CONFIG`, `REQUEST_STATUS`, `REQUEST_UPLOAD`, `REBOOT_LOGGER`, `LIST_APPS`. |

## 3. Version model

One identity per build, set by the build, never by hand:

| Field | Example | Source |
|---|---|---|
| `app_version` | `0.2.0+412.fd0a60d` | semver `versionName` + build number + short git commit |
| `version_code` | `412` | `git rev-list --count HEAD` at build time (monotonic on this branch; Android requires each update to be ≥ installed) |
| `git_commit` | `fd0a60d` (+ `-dirty` if uncommitted changes) | `git rev-parse --short HEAD` |
| `core_version` | same commit, from the Rust core | `env!("CARGO_PKG_VERSION")` + commit injected via `build.rs` |
| `protocol_version` | `1` | `versions.yaml` |
| `schema_version` | `1` | `versions.yaml` |
| `server_version` | `0.1.0+415.1a2b3c4` | each server binary, same scheme (`build.rs`) |

Rules:
- **Released builds must not be dirty.** `build_apk.sh` refuses to publish an
  APK with uncommitted changes, so every installed build maps to a commit.
- `schema_version` bumps **only** on a breaking log change (rename/remove
  column, changed meaning or unit). Adding an optional column or enum value
  (e.g. today's `app_activity`) never bumps. Analysis keeps one reader per
  `schema_version` it has seen.
- `protocol_version` bumps only on a breaking control-plane change. The server
  supports the current and previous protocol for one release, so an
  over-the-air update can always reach a phone one version behind.

## 4. Logs carry the version

### 4.1 `header.json` (mobile, test root) — implement the existing contract

The Logger writes it when a test starts, before the first packet, and seals it
with the other files:
- Version fields from §3: `app_version`, `version_code`, `git_commit`,
  `core_version`, `schema_version`, `protocol_version`.
- Identities, test start (UTC + mono), device (model, OS, build fingerprint),
  server host.
- The exact `test_plan` and `flow_bindings` from `RUN_TEST`, and the effective
  `logging_config`.

Schema change (additive): `version_code`, `git_commit`, `core_version` as
optional properties in `header.schema.json`; golden example in
`contracts/testvectors/log/header_examples.json`.

### 4.2 Server-side header

The server writes `server/node.json` per test: `server_version`, data-generator
and test-control versions, and its host. Both ends' builds then sit next to the
data.

### 4.3 Parquet

No version column per row. The header is the single home for test metadata
(spec §12.2). Rows keep only the join keys.

### 4.4 Status, tests table, analysis, pages

- `DeviceStatus` gains optional `app_version` / `version_code` (additive DTO
  field). server-main stores them in `device_status`, and in the `tests` row
  when a test starts (new nullable columns `logger_version`,
  `server_version`; migration `0002`).
- Analysis: `loader.load_test` reads `header.json` (absent → `None`, for old
  tests). `series` / `findings` carry `logger_version` per test.
- Findings page: a "Logger" column in the Tests table and a version filter.
  The charts page marks where the version changes on the time axis.
- Tests recorded before this change show "pre-versioning" (and could be
  backfilled from the install history: 08:20 build = frozen-radio fix, 12:27 =
  rate cap, …).

## 5. Compatibility gates

### 5.1 The Logger declares what it can run

`Capability` gains (additive):
- `flow_types: [probe, realtime_udp, bulk_tcp, app_traffic, …]`
- `features: [bulk_rate_cap, udp_reopen, sndbuf, screen_on, …]` — optional
  fields inside a flow type, e.g. `rate_kbps`.

The Logger re-sends its capability after every update. Today capability only
travels at enroll; add `PUT /api/v1/devices/{id}/capability`, called at service
start when the `version_code` differs from the last one sent.

### 5.2 The server only sends what can run

`POST /api/v1/tests` checks the plan against the device's capability:
- An unknown flow type is rejected with 409 and a clear message, e.g.
  "Logger 0.1.0+398 does not support app_traffic; update first".
- An unknown flow option is rejected the same way, so an option is never
  silently ignored by an old Logger (a capped bulk running uncapped would be
  wrong data).
- Devices with no capability list (pre-versioning Logger) are allowed only the
  flow types that existed at version 1: probe, realtime_udp, bulk_tcp,
  gateway_ping, dscp_probe.

### 5.3 Old logs stay readable

The loader already tolerates missing/unknown columns. Add a test that loads a
fixture from each `schema_version` and Logger generation (pre-header, header
v1) — the "log zoo" in `analysis/tests/fixtures/`.

## 6. Over-the-air update

### 6.1 Server: release store

- Directory `~/mp-data/releases/logger/<version_code>/`: `logger.apk`,
  `release.json`. `release.json` holds `version_code`, `app_version`,
  `git_commit`, `sha256`, `size`, `min_android_sdk`, `built_at`, `notes`,
  and the signing-certificate SHA-256.
- `scripts/publish_logger.sh`:
  1. builds via `build_apk.sh` (refuses dirty trees);
  2. checks the APK signature (`apksigner verify --print-certs`) against the
     pinned certificate;
  3. writes `release.json`;
  4. marks it `current` (symlink).
- Endpoints (server-main, device-authenticated):
  - `GET /api/v1/releases/logger/latest` → `release.json` (or 204 if none).
  - `GET /api/v1/releases/logger/{version_code}/apk` → the APK, supporting
    HTTP `Range`, so a download interrupted by a coverage gap resumes.
- Operator endpoints:
  - `POST /api/v1/releases/logger/{version_code}/promote` → current.
  - `POST /api/v1/devices/{id}/update` → queues `UPDATE_LOGGER`.

### 6.2 Command

- New command `UPDATE_LOGGER {version_code, sha256, url, policy}`, an
  additive enum variant.
- `policy`:
  - `wifi_only` (default): download only on unmetered networks, matching the
    upload hold.
  - `any`: also over mobile data, e.g. on the road.
  - `now`: skip the idle wait, used only when nothing is pending.
- Also periodic: at service start and every 6 h the Logger compares `latest`
  with itself and, if newer and promoted, behaves as if commanded with
  `wifi_only`.

### 6.3 Logger: download, verify, install

1. **Gate.** Logger state is IDLE: no test running or scheduled within the next
   5 min, and `pending_uploads == 0`. Otherwise retry when it next becomes idle.
   Logging-first: an update never pre-empts a test.
2. **Download.** Fetch into `files/update/<version_code>.apk.part`, resuming
   with `Range`, on the network allowed by `policy`
   (`NetworkCapabilities.NET_CAPABILITY_NOT_METERED`).
3. **Verify.**
   - SHA-256 equals `release.json`.
   - The package name is the same.
   - `versionCode` is greater than the installed one.
   - The signing certificate equals the installed app's
     (`PackageManager.getPackageArchiveInfo` with `GET_SIGNING_CERTIFICATES`).

   Android re-checks the signature anyway; this check gives an early, logged
   failure.
4. **Install** with `PackageInstaller`:
   - `SessionParams(MODE_FULL_INSTALL)` and
     `setRequireUserAction(USER_ACTION_NOT_REQUIRED)` (API 31+);
   - `setPackageSource(PACKAGE_SOURCE_OTHER)`;
   - commit with a `PendingIntent` to an `InstallResultReceiver`.

   Manifest permissions: `REQUEST_INSTALL_PACKAGES` and
   `UPDATE_PACKAGES_WITHOUT_USER_ACTION`.
5. **Pending user action.** On `STATUS_PENDING_USER_ACTION` (the first update,
   because the current install came from adb), post a high-priority
   notification "Logger update ready — tap to install". The server sees
   `update_state = awaiting_user` in status.
6. **After install.** Android kills and replaces the process.
   - A `MY_PACKAGE_REPLACED` receiver restarts the foreground service, which
     then sends its capability and status with the new version (§5.1).
   - The pending `UPDATE_LOGGER` command is acked with the result.
7. **Report.** Every step is a Logger event (`update_check`, `download_start`,
   `download_done`, `verify_failed`, `install_requested`, `install_ok`,
   `install_failed` with status code). These go to the status report and to a
   small `updates.jsonl` uploaded with the next test.

Silent updates after the first: Android 12+ allows an app to update **itself**
without user action when:
- it is the installer of record of its own package (true from the first OTA
  install on);
- it targets a recent SDK (we target 35);
- it holds `UPDATE_PACKAGES_WITHOUT_USER_ACTION`.

The first OTA (installer of record = adb shell) needs the one tap.

### 6.4 Failure handling and rollback

- **Download or verify fails:** keep the current build, report, retry at the
  next check (backoff up to 6 h).
- **Install fails:** the old build keeps running (Android installs atomically).
- **Bad build** that installs but misbehaves:
  - The new process reports its version within 60 s of start. If the server
    sees no status from the device within 10 min after `install_requested`,
    it flags the release.
  - Downgrade is not possible without uninstall (which would wipe unsent data),
    so **rollback = roll forward**: `publish_logger.sh --from <good commit>`
    builds the old code with a new, higher `version_code`.
- **Kill switch:** `promote` of an earlier release makes it `latest` (still
  rolled forward with a new version code by the script).
- **USB stays the recovery path:** `adb install -r` always works with the same
  key.

### 6.5 Security

- Device-authenticated downloads only (the existing device auth).
- The integrity chain is: Android's signature check, plus our certificate pin
  and SHA-256. The HTTP channel itself can stay plain (as today), because a
  tampered APK cannot pass the signature check.
- Signing key: today's debug keystore (`~/.android/debug.keystore`).
  - Back it up — losing it means the phone can never be updated again without
    uninstalling.
  - Moving to a dedicated release keystore later forces one uninstall/reinstall,
    so do it before the platform goes to more phones.

## 7. Work breakdown

| # | Step | Where | Tests | Est. |
|---|---|---|---|---|
| 1 | Build identity: `version_code` from git count, `versionName` with commit, dirty guard; `BuildConfig` fields; Rust `build.rs` commit for core and servers | `android/logger/build.gradle`, `build_apk.sh`, `core/build.rs`, `server/*/build.rs` | build fails on dirty tree; versions visible in `--version` / about screen | 1 h |
| 2 | `header.json` written by the Logger at test start, sealed with the test | `LoggerService` (+ JNI if core writes it) | golden header validates against schema; e2e test has header | 1.5 h |
| 3 | `server/node.json` per test | data-generator / test-control | interop test sees file | 0.5 h |
| 4 | Version in `DeviceStatus`; DB columns + migration; tests row stamped at start | contracts-rs, server-main | status/version round trip; migration test | 1 h |
| 5 | Capability: `flow_types`, `features`; `PUT capability`; plan check (409) | contracts-rs, server-main, Logger | old Logger (no list) refused `app_traffic`; new allowed | 1.5 h |
| 6 | Release store, `publish_logger.sh`, release endpoints with Range | server-main, scripts | Range resume; device auth required; sha matches | 1.5 h |
| 7 | `UPDATE_LOGGER` command, periodic check | contracts-rs, server-main, Logger | command queued/acked; idle gate | 1 h |
| 8 | Logger updater: download/resume, verify, PackageInstaller, receivers, events | Logger (`Updater.java`, manifest) | unit: verify rejects wrong sha/cert/lower version; device: two OTA hops | 2.5 h |
| 9 | Analysis + pages: version per test, Tests column, version filter, chart markers; log-zoo test | analysis, viz HTML | loader reads old + new tests; findings carry version | 1.5 h |
| 10 | Spec/AGENTS/CLAUDE docs; contracts (OpenAPI regen, enums, schemas) | planning/, contracts/ | drift + parity tests | 0.5 h |

Total about 12–13 hours of focused work; the critical path to the first OTA is
steps 1, 6, 7 and 8 (~6 h). Steps 2–5 and 9 can follow in the next session
without blocking OTA, but steps 1 and 2 should land **before** the first OTA
build so that every build from then on is identifiable in the logs.

## 8. Rollout

1. **Build A** (USB, the next time the phone is home): steps 1, 2, 4 and 8.
   - Its `header.json` carries the version.
   - Install it with `adb install -r`, as now. The installer of record is
     still the adb shell.
2. **Build B** (first OTA, any trivial change):
   - publish, queue `UPDATE_LOGGER`, phone downloads on home Wi-Fi;
   - **one tap** on the notification;
   - from here the Logger is its own installer of record.
3. **Build C** (second OTA): it must install **with no tap**. That proves the
   silent path. Measure the time from command to new version in status.
4. Then steps 3, 5, 6 (promote), 9 and 10. After that, every feature (e.g. the
   app-traffic Logger) ships over the air.
5. Keep USB for `mp-probe` restarts after reboots and as recovery.

## 9. Open points

- **Version scheme:** semver `0.MINOR.PATCH` set by hand for notable releases,
  or just `0.1.0+<count>.<commit>` until the platform has users? Default: the
  latter.
- **When the phone may update:** `wifi_only` by default (home only) or also
  over mobile data? Default: `wifi_only`, matching the upload hold. The
  operator can send `any` for urgent fixes.
- **Release keystore:** now (one reinstall) or before the second phone?
  Default: before the second phone.
- **The Controller app** should get the same updater later. It shares the
  approach but is not in this plan.

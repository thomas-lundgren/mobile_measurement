# Operators Guide

How to **set up, configure, and operate** the measurement platform end to end:
the Linux server (three processes), the Android Logger, the Android Controller,
and the Python analysis toolchain.

This is the practical operations manual. For *what the system is* see
[`../README.md`](../README.md); for the authoritative design see
[`../planning/5_g_field_test_platform_architecture_spec.md`](../planning/5_g_field_test_platform_architecture_spec.md);
for a copy-paste single-host bring-up see
[`../planning/device_bringup.md`](../planning/device_bringup.md).

---

## 0. Concepts in one minute

- A **device** (Android Logger) enrolls once with a one-time **enrollment token**
  and a hardware-backed P-256 key, then authenticates each session by
  challenge/response. It only ever talks to **server-main**.
- An **operator** (you, or the Controller app) authenticates with a
  username/password and receives a JWT. Operators create devices' tokens, start
  and stop **tests**, and read results.
- A **test** is a port-less **plan** of one or more **flows** (probe / realtime
  UDP / bulk TCP, each `ToServer` or `FromServer`). The server allocates ports,
  pushes `RUN_TEST` to the device, both ends run + log the flows, and the device
  uploads its sealed Parquet logs.
- **Logging-first:** measurement always writes locally first; upload never
  blocks a test.

---

## 1. Prerequisites

| To build/run | You need |
|---|---|
| Server + core | Rust (stable), a Linux host |
| Android Logger | Android SDK + NDK (r27), Gradle 8.13, `cargo-ndk`, a device (reference: Pixel 9 Pro) |
| Android Controller | Android SDK, Gradle 8.13, JDK 17 (for the OpenAPI client generator) |
| Analysis | Python ≥ 3.10 |
| Driving from a workstation | `adb`, `curl` |

Build everything and run the gate:

```bash
cargo build --release           # server binaries in target/release/
ci/check.sh                     # fmt, clippy, tests, contracts, analysis, client drift
```

---

## 2. Server setup

The server is **three cooperating processes**. They may run on one host (typical)
or be split; if split, `test-control`'s log root and `server-main`'s upload root
must be the **same filesystem location** so each test's `server/` and `mobile/`
recordings aggregate under one `test_id` (see §7.7).

### 2.1 The processes and their configuration

All configuration is via environment variables.

**`data-generator`** — owns the per-flow UDP/TCP sockets.

| Var | Default | Meaning |
|---|---|---|
| `MP_GENERATOR_BIND` | `127.0.0.1:9100` | internal control API (test-control talks to this) |

> The per-flow data sockets always bind `0.0.0.0:<port>`, so they are reachable
> on the LAN regardless of `MP_GENERATOR_BIND`.

**`test-control`** — scheduler, port allocation, generator coordination.

| Var | Default | Meaning |
|---|---|---|
| `MP_TEST_CONTROL_BIND` | `127.0.0.1:9000` | internal control API (server-main talks to this) |
| `MP_GENERATOR_URL` | `http://127.0.0.1:9100` | where to reach the data-generator |
| `MP_LOG_ROOT` | `/var/lib/mp` | **server-node** recordings root — set equal to `MP_UPLOAD_ROOT` |

> Flow ports are allocated from a base of **41000** upward. Open these on the
> host firewall for LAN tests (a small range, e.g. 41000–41050, covers many flows).

**`server-main`** — the control plane the operator and devices talk to.

| Var | Default | Meaning |
|---|---|---|
| `MP_BIND` | `127.0.0.1:8080` | REST listen address — use `0.0.0.0:8080` for LAN |
| `MP_DATABASE_URL` | `sqlite://mp.db` | SQLite control-plane DB (use `…?mode=rwc` to auto-create) |
| `MP_JWT_SECRET` | `dev-secret-change-me` | **change in production** — HS256 operator-JWT secret |
| `MP_SERVER_ID` | `srv_main_01` | server identity bound into the auth challenge |
| `MP_SERVER_HOST` | `127.0.0.1` | address the device is told to connect to for **data flows** — must be reachable by the device (e.g. the host's LAN IP) |
| `MP_TEST_CONTROL_URL` | _(unset)_ | `http://127.0.0.1:9000` — **required** to start tests |
| `MP_UPLOAD_ROOT` | `$TMPDIR/mp-uploads` | where uploaded + aggregated test trees live |
| `MP_TIME_UDP_PORT` | `8081` | UDP time-sync responder port (clock calibration) |

### 2.2 Running them (single host, LAN-reachable)

```bash
mkdir -p /srv/mp/data        # one shared root for logs + uploads
DB="sqlite:///srv/mp/mp.db?mode=rwc"

MP_GENERATOR_BIND=127.0.0.1:9100 ./target/release/mp-data-generator &

MP_TEST_CONTROL_BIND=127.0.0.1:9000 MP_GENERATOR_URL=http://127.0.0.1:9100 \
  MP_LOG_ROOT=/srv/mp/data ./target/release/mp-test-control &

MP_BIND=0.0.0.0:8080 MP_DATABASE_URL="$DB" \
  MP_TEST_CONTROL_URL=http://127.0.0.1:9000 \
  MP_SERVER_HOST=192.168.1.65 \
  MP_UPLOAD_ROOT=/srv/mp/data \
  MP_JWT_SECRET="$(openssl rand -hex 32)" \
  MP_TIME_UDP_PORT=8081 ./target/release/mp-server-main &
```

Replace `192.168.1.65` with the server's address as seen by the device.
`server-main` has no health route — any `404` means it's up.

**Firewall (LAN):** open inbound TCP `8080` (control plane), UDP `8081` (time
sync), and the generator port range from `41000` (TCP + UDP, for flow data).

### 2.3 First-time setup — operators and enrollment tokens (`mp-admin`)

`mp-admin` (the `admin` binary) operates directly on the same SQLite DB; point it
at the same `MP_DATABASE_URL`.

```bash
export MP_DATABASE_URL="sqlite:///srv/mp/mp.db?mode=rwc"

./target/release/admin add-operator alice 's3cret-pw'   # create an operator
./target/release/admin mint-token ENROLL-ABC123         # one-time device token (valid 24h)
./target/release/admin list-devices                     # devices that have enrolled
```

### 2.4 Security posture

The MVP serves **plain HTTP** (TLS is a hardening follow-up). Run it on a trusted
LAN only, set a strong `MP_JWT_SECRET`, and don't expose `8080` to untrusted
networks. Device identity is a non-exportable P-256 key (Android Keystore);
operator identity is a username/password → JWT.

---

## 3. Android Logger setup

The Logger is the autonomous measurement client.

### 3.1 Build & install

```bash
android/build_apk.sh        # cargo-ndk builds the core .so, then assembles the APK
adb install -r android/logger/build/outputs/apk/debug/logger-debug.apk
```

### 3.2 Grant permissions

The app requests these at launch; grant them (location is required for radio
sampling):

```bash
pkg=com.measurement.logger
for p in ACCESS_FINE_LOCATION READ_PHONE_STATE POST_NOTIFICATIONS; do
  adb shell pm grant $pkg android.permission.$p; done
```

### 3.3 Configure & start

Open **MP Logger**, enter the **server URL** (`http://<server-host>:8080`) and a
fresh **enrollment token**, and tap **Enroll & start logging**. The device
enrolls once (its `device_id` is then cached; the token is only needed the first
time), authenticates, and goes to `IDLE` waiting for tests.

To drive it from a workstation instead of the UI:

```bash
adb shell am start -n com.measurement.logger/.MainActivity \
  -e server "http://192.168.1.65:8080" -e token "ENROLL-ABC123"
```

### 3.4 Network requirements

- **Same LAN as the server (recommended):** point the app at the server's LAN IP.
  TCP **and** UDP flows work, including `FromServer` UDP punch-through.
- **USB only (`adb reverse`):** TCP works (control plane + bulk TCP), but **UDP
  flows can't traverse** `adb reverse`. Use the LAN for probe/realtime.
- The app permits cleartext HTTP (dev server). Nothing else to configure.

### 3.5 What it does automatically

- **Logging-first:** every test is written to app-private storage first, then
  uploaded; a failed upload never loses data.
- **Reconnects:** the worker survives transient network loss / server restarts
  with exponential backoff (you'll see `RECONNECTING…` then `IDLE`).
- **Telemetry:** reports battery + free storage in its heartbeat.
- **Low-storage guard:** refuses to start a test below ~200 MB free.

> **Expected: the Logger stops itself after ~6 hours.** Android 14+ time-limits a
> `dataSync` foreground service to roughly **6 hours of cumulative runtime per
> 24 h**. When the budget is reached the Logger stops cleanly (state:
> *"foreground-service time limit reached — restart to resume"*) — this is normal,
> not a crash. **Just restart it** (re-open the app / tap start, or re-issue the
> `adb am start`) to continue; the device stays enrolled, so no token is needed.
> Plan long field sessions around this (e.g. restart between drives, or script a
> periodic relaunch). See §9 for how to lift the limit if continuous 24/7
> operation ever becomes a requirement.

---

## 4. Android Controller setup

The operator console — no measurement.

```bash
android/controller/generate_client.sh                  # (re)generate the REST client
(cd android && gradle :controller:assembleDebug)
adb install -r android/controller/build/outputs/apk/debug/controller-debug.apk
```

Open **MP Controller**, enter the server URL + operator username/password, and
tap **Login + list devices**. You then get a live dashboard (auto-refresh every
3 s): the fleet with per-device state/battery, and the recent-tests list.
**Start probe / Start bidir** launch preset tests on the selected device;
**Stop** cancels the newest running test.

---

## 5. Analysis toolchain setup

```bash
cd analysis
python3 -m venv .venv && .venv/bin/pip install -e ".[dev,plots]"
```

Run it against an uploaded test directory (see §7.7 for where those are):

```bash
.venv/bin/python -m fieldtest_analysis <test_dir> -o summary.json --plots out/
```

- `summary.json` (spec §12.8): RTT / OWD (offset-corrected, up+down) / jitter /
  per-flow loss / radio stats / GPS endpoints / derived events.
- `out/*.png`: RTT-over-time, OWD-over-time, jitter, serving-RSRP, GPS route,
  and an RTT-vs-RSRP correlation (only the applicable plots are written).

---

## 6. Operating the system

### 6.1 Enroll a device

1. `mp-admin mint-token <TOKEN>` (server).
2. In the Logger, enter the server URL + that token and start (or `adb am start`
   with the extras, §3.3).
3. Confirm with `mp-admin list-devices` or the Controller's device list.

### 6.2 Define a test plan

A plan is **port-less**; the server allocates ports. Flow types:

| `type` | Fields | Direction |
|---|---|---|
| `probe` | `rate_pps`, `packet_size_bytes` | echo (RTT) — no direction |
| `realtime_udp` | `direction`, `rate_pps`, `packet_size_bytes` | `ToServer` or `FromServer` |
| `bulk_tcp` | `direction`, `target_bytes` | `ToServer` or `FromServer` |

There is **no `Bidirectional`** direction — pair a `ToServer` flow with a
`FromServer` flow to measure both ways at once (symmetric-peers model).

### 6.3 Start a test

**From the Controller:** select the device, tap a preset (Start probe / bidir).

**From a workstation (full control over the plan):**

```bash
JWT=$(curl -s -XPOST http://192.168.1.65:8080/api/v1/auth/login \
  -H 'content-type: application/json' \
  -d '{"username":"alice","password":"s3cret-pw"}' \
  | python3 -c 'import sys,json;print(json.load(sys.stdin)["jwt"])')

curl -s -XPOST http://192.168.1.65:8080/api/v1/tests \
  -H "authorization: Bearer $JWT" -H 'content-type: application/json' -d '{
  "device_id":"<dev_…>",
  "plan":{"name":"survey","version":1,"duration_sec":30,"flows":[
    {"type":"probe","flow_id":1,"rate_pps":50,"packet_size_bytes":22},
    {"type":"realtime_udp","flow_id":2,"direction":"ToServer","rate_pps":100,"packet_size_bytes":200},
    {"type":"realtime_udp","flow_id":3,"direction":"FromServer","rate_pps":100,"packet_size_bytes":200},
    {"type":"bulk_tcp","flow_id":4,"direction":"FromServer","target_bytes":5000000}]}}'
```

The call returns once the generators are ready (`WAITING_FOR_CLIENT`); the device
picks up `RUN_TEST` on its next poll and runs it.

### 6.4 Monitor

- **Controller dashboard** — live test state + per-device status.
- **REST:** `GET /api/v1/tests` (recent), `GET /api/v1/tests/{id}` (one),
  `GET /api/v1/devices` (fleet + last-seen/battery/state).

### 6.5 Stop a test

Controller **Stop** button, or:

```bash
curl -s -XPOST http://192.168.1.65:8080/api/v1/tests/<test_id>/stop \
  -H "authorization: Bearer $JWT"
```

This drives the test terminal (`DONE`/`CANCELLED`) and stops its generators.

### 6.6 Change what a device logs (dynamic config)

Push a logging config; the device applies it without restarting and bumps its
version:

```bash
curl -s -XPOST http://192.168.1.65:8080/api/v1/devices/<dev_…>/logging-config \
  -H "authorization: Bearer $JWT" -H 'content-type: application/json' \
  -d @my_logging_config.json
```

The effective config is the intersection of what you request and the device's
capabilities. A starting example lives in
`contracts/testvectors/logging_config_examples.json`.

### 6.7 Retrieve & analyze results

Uploaded + aggregated test trees live under `MP_UPLOAD_ROOT`:

```
<MP_UPLOAD_ROOT>/<session_id>/<test_id>/
  gps.parquet  radio.parquet            # mobile-only, once per test
  mobile/  packets_*.parquet  clock.parquet
  server/  packets_f*.parquet           # server node's view (uplink OWD)
```

Point the analysis CLI at `<MP_UPLOAD_ROOT>/<session_id>/<test_id>` (§5).

---

## 7. Configuration quick reference

**Ports (defaults):** `8080/tcp` control plane · `8081/udp` time sync ·
`41000+/tcp,udp` flow data · `9000`,`9100` internal (loopback).

**Per-process env:** see the tables in §2.1. The minimum to start tests:
`server-main` needs `MP_TEST_CONTROL_URL` and a device-reachable `MP_SERVER_HOST`;
`test-control`'s `MP_LOG_ROOT` should equal `server-main`'s `MP_UPLOAD_ROOT`.

---

## 8. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| Logger: `CLEARTEXT … not permitted` | Reinstall the current APK (it permits cleartext for the dev server). |
| `POST /tests` → "test-control not configured" | Set `MP_TEST_CONTROL_URL` on `server-main`. |
| Test stuck at `WAITING_FOR_CLIENT` | The device isn't polling — check the Logger is running and reachable; it auto-reconnects, give it a few seconds. |
| UDP flows show no packets | You're on USB (`adb reverse` is TCP-only). Put the phone + server on the same LAN and set `MP_SERVER_HOST` to the LAN IP. |
| Device data flows fail but control plane works | Open the generator port range (41000+) on the firewall; confirm `MP_SERVER_HOST` is device-reachable. |
| Logger: `RECONNECTING…` | Server unreachable/restarting; it backs off and recovers automatically. |
| Logger: `low storage, refusing` | Free space below ~200 MB; clear old logs. |
| Logger stopped after several hours (`…time limit reached…`) | Expected — the Android `dataSync` FGS ~6 h/24 h cap. Restart the Logger to resume (still enrolled, no token needed). See §3.5 and §9. |
| Uplink OWD empty in analysis | The `server/` recordings weren't aggregated — set `MP_LOG_ROOT` = `MP_UPLOAD_ROOT` (single host). |
| Operator login fails after restart | A new `MP_JWT_SECRET` invalidates old JWTs — log in again. |

---

## 9. Production hardening (not yet in the MVP)

Terminate TLS in front of `server-main` (or add native TLS), put the generator
behind appropriate NAT/firewall rules, rotate `MP_JWT_SECRET` and enrollment
tokens, and move `MP_DATABASE_URL` + `MP_UPLOAD_ROOT` to durable storage. See
[`../STATUS.md`](../STATUS.md) for the current state of hardening (Phase 5).

### Continuous 24/7 Logger operation (lifting the ~6 h limit)

The Logger uses a `dataSync` foreground service, which Android 14+ caps at ~6 h
per 24 h (§3.5). The current, accepted behavior is **stop cleanly and restart**.
If uninterrupted multi-hour/continuous logging ever becomes a requirement, the
fix is to change the foreground-service type from `dataSync` to **`specialUse`**,
which is not time-limited:

1. `android/logger/src/main/AndroidManifest.xml`: change the permission
   `FOREGROUND_SERVICE_DATA_SYNC` → `FOREGROUND_SERVICE_SPECIAL_USE`, set the
   service `android:foregroundServiceType="specialUse"`, and add a
   `<property android:name="android.app.PROPERTY_SPECIAL_USE_FGS_SUBTYPE"
   android:value="…justification…"/>` child on the `<service>`.
2. `LoggerService`: pass `ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE` to
   `startForeground` (the `onTimeout` handler can stay as a safety net).
3. **Google Play caveat:** `specialUse` requires a written justification and
   review for Play-distributed apps. For sideloaded/MDM-managed field devices
   (the current deployment model) it works without review.

`location` is **not** a workaround — that type is also time-limited on Android 15.

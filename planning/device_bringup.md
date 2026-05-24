# On-device bring-up runbook (Android Logger ↔ server)

How to drive a full end-to-end test from a workstation against a real Pixel.
Captured from the Phase-2 `2f` bring-up; all commands below were run and verified.

## Components & ports

Three host processes (Rust `target/debug/`), plus the Logger app on the phone:

| Process | Bind | Notes |
|---|---|---|
| `mp-data-generator` | `MP_GENERATOR_BIND=127.0.0.1:9100` (internal API) | per-flow UDP/TCP sockets bind `0.0.0.0:<port>` → LAN-reachable |
| `mp-test-control`   | `MP_TEST_CONTROL_BIND=127.0.0.1:9000` | `port_base=41000`; allocates flow ports upward |
| `mp-server-main`    | `MP_BIND` (control plane) | needs `MP_TEST_CONTROL_URL`, `MP_SERVER_HOST`, `MP_UPLOAD_ROOT` |

`MP_SERVER_HOST` is the host the UE is told to connect to for data flows (it
travels in RUN_TEST). It must be an address the **phone** can reach.

## 1. Start the servers

```bash
mkdir -p /tmp/mp-dev/logs /tmp/mp-dev/uploads
DB="sqlite:///tmp/mp-dev/mp.db?mode=rwc"

# data-generator
MP_GENERATOR_BIND=127.0.0.1:9100 ./target/debug/mp-data-generator &

# test-control
MP_TEST_CONTROL_BIND=127.0.0.1:9000 MP_GENERATOR_URL=http://127.0.0.1:9100 \
  MP_LOG_ROOT=/tmp/mp-dev/logs ./target/debug/mp-test-control &

# server-main — LAN-bound (use 192.168.x.x for MP_SERVER_HOST so the phone can reach flows)
MP_BIND=0.0.0.0:8080 MP_DATABASE_URL="$DB" \
  MP_TEST_CONTROL_URL=http://127.0.0.1:9000 \
  MP_SERVER_HOST=192.168.1.65 \
  MP_UPLOAD_ROOT=/tmp/mp-dev/uploads MP_JWT_SECRET=dev-secret \
  ./target/debug/mp-server-main &
```

> server-main has no `/health` route — a `404` from any path means it's up.

## 2. Mint an operator + enrollment token (`mp-admin`)

`mp-admin` operates directly on the same SQLite DB (`MP_DATABASE_URL`):

```bash
export MP_DATABASE_URL="sqlite:///tmp/mp-dev/mp.db?mode=rwc"
./target/debug/admin add-operator op op-pass-123
./target/debug/admin mint-token DEVTOKEN
./target/debug/admin list-devices         # after the phone enrolls
```

## 3. Onboard the phone

Grant the runtime permissions, then launch `MainActivity` with `server`/`token`
extras — it prefills the fields and auto-starts the foreground service (FGS
starts are allowed from a foreground Activity; `adb am start-foreground-service`
is blocked because the service is not exported).

```bash
pkg=com.measurement.logger
for p in ACCESS_FINE_LOCATION READ_PHONE_STATE POST_NOTIFICATIONS; do
  adb shell pm grant $pkg android.permission.$p; done

adb shell am force-stop $pkg          # MainActivity is singleTop: force-stop so onCreate re-runs
adb shell am start -n $pkg/.MainActivity \
  -e server "http://192.168.1.65:8080" -e token "DEVTOKEN"
```

Once enrolled the `device_id` is saved in app prefs; subsequent launches skip
enroll and re-authenticate with the Keystore key, so the token is then ignored.
Confirm onboarding reached `IDLE`:

```bash
adb shell uiautomator dump /sdcard/u.xml >/dev/null && \
  adb shell cat /sdcard/u.xml | grep -oE 'text="(IDLE|ERROR[^"]*)"'
```

## 4. Start a test (operator REST)

```bash
JWT=$(curl -s -XPOST http://192.168.1.65:8080/api/v1/auth/login \
  -H 'content-type: application/json' \
  -d '{"username":"op","password":"op-pass-123"}' \
  | python3 -c 'import sys,json;print(json.load(sys.stdin)["jwt"])')

curl -s -XPOST http://192.168.1.65:8080/api/v1/tests \
  -H "authorization: Bearer $JWT" -H 'content-type: application/json' -d '{
  "device_id":"<dev_…>","plan":{"name":"lan","version":1,"duration_sec":15,"flows":[
    {"type":"bulk_tcp","flow_id":1,"direction":"ToServer","target_bytes":3000000},
    {"type":"bulk_tcp","flow_id":2,"direction":"FromServer","target_bytes":3000000},
    {"type":"realtime_udp","flow_id":3,"direction":"ToServer","rate_pps":100,"packet_size_bytes":200},
    {"type":"realtime_udp","flow_id":4,"direction":"FromServer","rate_pps":100,"packet_size_bytes":200},
    {"type":"probe","flow_id":5,"rate_pps":50}]}}'
```

**Bidirectional** = a `ToServer` flow paired with a `FromServer` flow. There is
no single `Bidirectional` direction — it was dropped from the contract in favour
of paired flows (symmetric-peers model). `FromServer` UDP exercises the ~1 Hz
keepalive NAT/firewall punch-through.

## 5. Inspect results

```bash
# test reaches DONE/COMPLETED; uploads land under MP_UPLOAD_ROOT
python3 - <<'PY'
import sqlite3; c=sqlite3.connect("/tmp/mp-dev/mp.db")
print(c.execute("SELECT test_id,state,result FROM tests ORDER BY rowid DESC LIMIT 1").fetchone())
PY
find /tmp/mp-dev/uploads -type f      # mobile/packets_*.parquet, radio.parquet, gps.parquet
```

Read the Parquet with pyarrow (e.g. in a throwaway venv); per-flow `direction`
counts (`SENT`/`RECEIVED`) confirm both directions moved. Counts are the
**mobile node's view only** — the server logs its complementary side, but MVP
uploads are mobile-only.

## Transport reachability

- **Same LAN (preferred):** point the phone at the host's `192.168.x.x`. TCP +
  UDP both work, including UDP punch-through. Open the host firewall for `8080`
  and the generator range (`41000+`). server-main is plain HTTP (TLS is Phase 5)
  — fine on a dev LAN, don't expose it further.
- **USB only (`adb reverse`):** TCP-only. `adb reverse tcp:8080 tcp:8080` plus
  the generator ports lets the control plane + `bulk_tcp` work, but **UDP
  (probe / realtime) cannot traverse** — use the LAN for those.

Cleartext HTTP is allowed by `network_security_config.xml`
(`cleartextTrafficPermitted=true`); without it the app fails with
`CLEARTEXT communication … not permitted`.

## Gotchas

- **Don't restart server-main mid-session.** The Logger's poll loop sits in one
  outer try/catch, so a dropped connection ends the worker and it stops polling
  until the app is force-restarted. Start the server first, then the app.
  (Per-iteration retry/backoff is a Phase-5 robustness follow-up.)
- **`MainActivity` is `singleTop`** — a re-`am start` hits `onNewIntent` (not
  handled for auto-start), so `force-stop` first to re-run `onCreate`.
- **`pm clear`** wipes granted permissions *and* the Keystore key (forces a
  fresh enroll); re-grant permissions after it.
- Changing `network_security_config.xml` (or any resource) requires a rebuild +
  reinstall — it ships in the APK, not pushed at runtime.

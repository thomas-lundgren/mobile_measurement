# Shared by the field-test cycle scripts (source it; set LOG_NAME first):
# operator login, phone readiness, starting a test and waiting for its end,
# and phases that wait for the phone (optionally on the mobile network).
#   DATA (default ~/mp-data), STOP_AT, WAIT_MIN (default 60), CELL_ONLY=1,
#   PHASES ("name name ..." runs only those phases; default all).
SERVER=http://127.0.0.1:8080
DEVICE=dev_01M588ENHCVDGS8DZ9B5JCN7EP
STOP_AT="${STOP_AT:-2099-01-01 00:00}"
DATA="${DATA:-$HOME/mp-data}"
LOG="${LOG:-$DATA/logs/${LOG_NAME:-fieldtest}.log}"

log() { echo "$(date '+%F %T') $*" >> "$LOG"; }

if [[ $(date +%s) -ge $(date -d "$STOP_AT" +%s) ]]; then
    log "past STOP_AT ($STOP_AT), skipping — remove the crontab line"
    exit 0
fi

# Phone must be IDLE and have reported recently. Prints the reason if not.
phone_ready() {
    local state age
    read -r state age < <(python3 - "$DATA/mp.db" "$DEVICE" <<'EOF'
import sqlite3, sys, time
c = sqlite3.connect(sys.argv[1])
r = c.execute("select logger_state, received_at from device_status where device_id=?", (sys.argv[2],)).fetchone()
print(r[0], round(time.time() - r[1] / 1e9)) if r else print("UNKNOWN", -1)
EOF
)
    if [[ "$state" != "IDLE" || "$age" -lt 0 || "$age" -gt 120 ]]; then
        echo "phone state=$state last_report=${age}s ago"
        return 1
    fi
}

PW=$(sed -n 's/^operator password: //p' "$DATA/credentials.txt")
JWT=$(curl -s -m 10 -XPOST "$SERVER/api/v1/auth/login" -H 'content-type: application/json' \
    -d "{\"username\":\"thomas\",\"password\":\"$PW\"}" \
    | python3 -c 'import sys,json; print(json.load(sys.stdin)["jwt"])' 2>/dev/null)
if [[ -z "$JWT" ]]; then
    log "error: operator login failed (is server-main running?)"
    exit 1
fi

# run_test NAME DURATION_SEC FLOWS_JSON — start one test and wait for its end
# state. Returns non-zero if it could not run or did not finish.
run_test() {
    local name=$1 duration=$2 flows=$3 why resp test_id res
    if ! why=$(phone_ready); then
        log "skip $name: $why"
        return 1
    fi
    resp=$(curl -s -m 10 -XPOST "$SERVER/api/v1/tests" \
        -H "authorization: Bearer $JWT" -H 'content-type: application/json' -d '{
      "device_id":"'"$DEVICE"'","plan":{"name":"'"$name"'","version":1,"duration_sec":'"$duration"',
      "flows":'"$flows"'}}')
    test_id=$(python3 -c 'import sys,json; print(json.loads(sys.argv[1])["test_id"])' "$resp" 2>/dev/null)
    if [[ -z "$test_id" ]]; then
        log "error $name: test not created: $resp"
        return 1
    fi
    log "started $name $test_id (${duration}s)"
    for _ in $(seq 1 $((duration + 180))); do
        # Finished = the phone acked RUN_TEST (it acks once measuring ends; the
        # test row turns DONE only when the upload lands, which may be held).
        res=$(python3 - "$DATA/mp.db" "$test_id" <<'EOF'
import sqlite3, sys
c = sqlite3.connect(sys.argv[1])
r = c.execute("select state, result from tests where test_id=?", (sys.argv[2],)).fetchone()
acked = c.execute("select 1 from commands where command='RUN_TEST' and status='acked' and payload_json like ?",
                  (f'%"test_id":"{sys.argv[2]}"%',)).fetchone()
print("ACKED (upload pending)" if acked and r and r[0] not in ("DONE", "FAILED") else f"{r[0]} {r[1]}" if r else "")
EOF
)
        case "$res" in
            ACKED*|DONE*|FAILED*|ABORTED*|ERROR*) log "$name $test_id -> $res"; return 0 ;;
        esac
        sleep 1
    done
    log "$name $test_id -> still not finished after $((duration + 180))s (last: $res)"
    return 1
}

# Where the phone last reported its status from, as logged by server-main
# ("status: <device> reports from <ip> (local|remote)"): local = home Wi-Fi
# (private address or our own public IP via hairpin NAT), remote = elsewhere,
# i.e. the mobile network (office Wi-Fi cannot reach the server at all).
# Logger flows do not survive a Wi-Fi <-> mobile switch yet, so with
# CELL_ONLY=1 a test only starts while the phone is on the mobile network.
on_cellular() {
    grep "status: $DEVICE reports from" "$DATA/logs/server-main.log" | tail -n 1 | grep -q "(remote)$"
}

# Wait until the next phase can start; false after WAIT_MIN minutes.
wait_ready() {
    local end=$(( $(date +%s) + ${WAIT_MIN:-60} * 60 ))
    while [[ $(date +%s) -lt $end ]]; do
        if phone_ready >/dev/null && { [[ "${CELL_ONLY:-0}" != 1 ]] || on_cellular; }; then
            return 0
        fi
        sleep 5
    done
    log "gave up waiting for the phone"
    return 1
}

# a selected phase the phone can't start (e.g. home Wi-Fi reached) ends the
# cycle: phase NAME DURATION_SEC FLOWS_JSON
phase() {
    [[ -z "${PHASES:-}" || " $PHASES " == *" $1 "* ]] || return 0
    wait_ready || { log "cycle stopped before $1"; exit 0; }
    run_test "$@"
}

# The realtime streams every cycle measures: 1 ms uplink and downlink, 200 B,
# plus the 50 pps probe. Uplink first: it gets port 41000 (hairpin-safe).
UDP='{"type":"realtime_udp","flow_id":1,"direction":"ToServer","rate_pps":1000,"packet_size_bytes":200},
    {"type":"probe","flow_id":2,"rate_pps":50},
    {"type":"realtime_udp","flow_id":3,"direction":"FromServer","rate_pps":1000,"packet_size_bytes":200}'

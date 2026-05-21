#!/usr/bin/env bash
set -uo pipefail

cd "$(dirname "$0")/../.."

LOG_DIR="spikes/stage-a-harness/logs_resilience"
rm -rf "$LOG_DIR"
mkdir -p "$LOG_DIR"

echo "=== Build ==="
cargo build --workspace 2>&1 | tail -3
javac -d spikes/stage-a-harness/build spikes/stage-a-harness/java/spike/*.java 2>&1 | tail -3

echo "=== Start peer ==="
target/debug/spike-peer > /dev/null 2>&1 &
PEER_PID=$!
sleep 1

kill_peer() {
    kill $PEER_PID 2>/dev/null || true
    wait $PEER_PID 2>/dev/null || true
}
restart_peer() {
    target/debug/spike-peer > /dev/null 2>&1 &
    PEER_PID=$!
}

cleanup() {
    kill_peer
}
trap cleanup EXIT

echo "=== Start resilience flow (20 sec) ==="
java -cp spikes/stage-a-harness/build \
  -Djava.library.path="$(pwd)/target/debug" \
  spike.ResilienceHarness > spikes/stage-a-harness/resilience.log 2>&1 &
JAVA_PID=$!

# Wait for flow to start
while ! grep -q "FLOW_STARTED" spikes/stage-a-harness/resilience.log 2>/dev/null; do
    sleep 0.2
done
echo "Flow started (pid=$JAVA_PID)"

# Blackout: kill peer after 3 seconds, restart after 6 seconds total
sleep 3
echo "=== BLACKOUT: killing peer ==="
kill_peer

sleep 3
echo "=== RECOVERY: restarting peer ==="
restart_peer

# Wait for Java to finish
wait $JAVA_PID
cat spikes/stage-a-harness/resilience.log

echo "=== Analyze Parquet ==="
source venv/bin/activate
python3 <<PY
import glob, json, sys
import pyarrow.parquet as pq

files = sorted(glob.glob("spikes/stage-a-harness/logs_resilience/*.parquet"))
print(f"Segments: {len(files)}")

if not files:
    print("FAIL: no parquet files")
    sys.exit(1)

all_rows = []
for f in files:
    t = pq.read_table(f)
    all_rows.extend(t.to_pylist())

sent = [r for r in all_rows if r["direction"] == "SENT"]
recv = [r for r in all_rows if r["direction"] == "RECEIVED"]
print(f"Total SENT: {len(sent)}, RECEIVED: {len(recv)}")

# Parse final status from log
with open("spikes/stage-a-harness/resilience.log") as fh:
    for line in fh:
        if line.startswith("FINAL_STATUS:"):
            status = json.loads(line.split("FINAL_STATUS:")[1].strip())
            print(f"Stats sent={status['packets_sent']} recv={status['packets_received']}")
            if status['packets_sent'] >= 350 and status['packets_received'] >= 250:
                print("PASS: logging-first holds through blackout")
            else:
                print("FAIL: too few packets")
                sys.exit(1)
            break
PY

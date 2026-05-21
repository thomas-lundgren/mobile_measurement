#!/usr/bin/env bash
set -uo pipefail

cd "$(dirname "$0")/../.."

LOG_DIR="spikes/stage-a-harness/logs_sigkill"
rm -rf "$LOG_DIR"
mkdir -p "$LOG_DIR"

echo "=== Build ==="
cargo build --workspace 2>&1 | tail -3
javac -d spikes/stage-a-harness/build spikes/stage-a-harness/java/spike/*.java 2>&1 | tail -3

echo "=== Start peer ==="
target/debug/spike-peer > /dev/null 2>&1 &
PEER_PID=$!
sleep 1

cleanup() {
    kill $PEER_PID 2>/dev/null || true
    wait $PEER_PID 2>/dev/null || true
}
trap cleanup EXIT

echo "=== Start long flow (60 sec, 50 pps, segment=30 rows) ==="
java -cp spikes/stage-a-harness/build \
  -Djava.library.path="$(pwd)/target/debug" \
  spike.SigkillHarness > spikes/stage-a-harness/sigkill.log 2>&1 &
JAVA_PID=$!

# Wait for flow to start
while ! grep -q "FLOW_STARTED" spikes/stage-a-harness/sigkill.log 2>/dev/null; do
    sleep 0.2
done
echo "Flow started (pid=$JAVA_PID)"

# Wait for at least 5 closed segments (150 rows)
echo "=== Waiting for segments ==="
while true; do
    count=$(find "$LOG_DIR" -name '*.parquet' | wc -l)
    if [ "$count" -ge 5 ]; then
        break
    fi
    sleep 0.2
done
echo "Found $count segments, sending SIGKILL"

kill -9 $JAVA_PID
wait $JAVA_PID 2>/dev/null || true
sleep 1

echo "=== Analyze segments ==="
source venv/bin/activate
python3 <<PY
import glob, sys, os
import pyarrow.parquet as pq

files = sorted(glob.glob("spikes/stage-a-harness/logs_sigkill/*.parquet"))
print(f"Total parquet files: {len(files)}")

closed_ok = 0
open_corrupt = 0
open_missing = 0

for f in files:
    size = os.path.getsize(f)
    if size == 0:
        print(f"  {f}: EMPTY (open segment lost)")
        open_missing += 1
        continue
    try:
        t = pq.read_table(f)
        print(f"  {f}: {t.num_rows} rows OK")
        closed_ok += 1
    except Exception as e:
        print(f"  {f}: CORRUPTED ({e})")
        open_corrupt += 1

print(f"Closed+readable: {closed_ok}, Corrupt: {open_corrupt}, Empty: {open_missing}")
if closed_ok >= 4:
    print("PASS: closed segments survived SIGKILL")
else:
    print("FAIL: too few closed segments readable")
    sys.exit(1)
PY

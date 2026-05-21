#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/../.."

echo "=== Build Rust artifacts ==="
cargo build --workspace

echo "=== Compile Java harness ==="
mkdir -p spikes/stage-a-harness/build
javac -d spikes/stage-a-harness/build spikes/stage-a-harness/java/spike/*.java

echo "=== Start peer echo server ==="
target/debug/spike-peer > spikes/stage-a-harness/peer.log 2>&1 &
PEER_PID=$!
sleep 1

cleanup() {
    echo "=== Cleanup ==="
    kill $PEER_PID 2>/dev/null || true
}
trap cleanup EXIT

echo "=== Run Java Stage-A Harness ==="
java -cp spikes/stage-a-harness/build \
  -Djava.library.path="$(pwd)/target/debug" \
  spike.StageAHarness

echo "=== Validate Parquet with pyarrow ==="
source venv/bin/activate
python3 <<'PY'
import glob, pyarrow.parquet as pq, sys
files = sorted(glob.glob("spikes/stage-a-harness/logs/*.parquet"))
print(f"Parquet files found: {len(files)}")
total_rows = 0
for f in files:
    t = pq.read_table(f)
    total_rows += t.num_rows
    print(f"  {f}: {t.num_rows} rows, columns={t.column_names}")
    # Sanity: mono_ns should be non-decreasing within a segment for same direction
    df = t.to_pandas()
    for direction in ["SENT", "RECEIVED"]:
        sub = df[df["direction"] == direction]
        if len(sub) > 1:
            deltas = sub["mono_ns"].diff().dropna()
            if (deltas < 0).any():
                print(f"    WARNING: mono_ns decreased for {direction}")
print(f"Total rows: {total_rows}")
if not files:
    sys.exit(1)
PY

echo "=== Stage A smoke test complete ==="

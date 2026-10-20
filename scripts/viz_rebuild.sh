#!/usr/bin/env bash
# Rebuild the map, chart and findings data under ~/mp-data/viz (cron, every
# 5 min under flock). It runs on the machine that also runs the data
# generator, so it stays out of the way of measurements: lowest CPU and I/O
# priority, and skipped while a test is running (the next run catches up).
# A cold rebuild (after a SERIES_VERSION bump) takes minutes.
set -uo pipefail

DATA="${DATA:-$HOME/mp-data}"
ANALYSIS="$(cd "$(dirname "$0")/../analysis" && pwd)"

# A test is running when the phone says so (fresh status), or a test that is
# neither finished nor stale (created within 30 min) is still open.
if python3 - "$DATA/mp.db" <<'EOF'
import sqlite3, sys, time
c = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
now = time.time_ns()
phone = c.execute("select 1 from device_status where logger_state = 'RUNNING_TEST' and received_at > ?",
                  (now - 120 * 10**9,)).fetchone()
open_test = c.execute("select 1 from tests where state not in ('DONE', 'FAILED', 'ABORTED') and created_at > ?",
                      (now - 1800 * 10**9,)).fetchone()
sys.exit(0 if phone or open_test else 1)
EOF
then
    echo "$(date '+%F %T') test running, rebuild skipped"
    exit 0
fi

cd "$ANALYSIS" || exit 1
exec nice -n 19 ionice -c 3 sh -c '
    .venv/bin/python -m fieldtest_analysis.track "$1" --db "$1/mp.db" -o "$1/viz/track.json" &&
    .venv/bin/python -m fieldtest_analysis.series "$1" --db "$1/mp.db" -o "$1/viz"' sh "$DATA"

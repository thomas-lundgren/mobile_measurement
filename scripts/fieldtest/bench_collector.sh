#!/usr/bin/env bash
# Keep the USB bench collector (fieldtest_analysis.bench) running while the
# phone is plugged into this machine: modem-link qdisc stats (~12 Hz) and
# radio-log events (carrier configs, link-capacity estimates) into
# ~/mp-data/bench/<day>/. Started from cron every minute under flock; exits
# when the phone is unplugged and comes back on the next minute after.
cd "$(dirname "$0")/../../analysis" || exit 1
adb get-state >/dev/null 2>&1 || exit 0
echo "$(date '+%F %T') collector start ($(adb get-serialno))"
exec .venv/bin/python -m fieldtest_analysis.bench collect --out "$HOME/mp-data/bench"

#!/usr/bin/env bash
# Head-of-line cycle with rate-capped bulk TCP: the realtime UDP streams
# (lib.sh UDP) first alone, then beside low and medium bulk TCP in each
# direction:
#   hol-base, hol-up-<low>, hol-up-<med>, hol-down-<low>, hol-down-<med>
# LOW_KBPS / MED_KBPS (default 2000 / 10000), PHASE_SEC (default 90).
# Data per cycle at the defaults: ~2 x (22 + 110) MB of bulk + ~10 MB of UDP.
# One cycle per run; appends to ~/mp-data/logs/hol.log. Each phase waits (up
# to WAIT_MIN, default 60 min) until the phone is idle and, with CELL_ONLY=1,
# on the mobile network.
set -uo pipefail
LOG_NAME=hol
source "$(dirname "$0")/lib.sh"

PHASE=${PHASE_SEC:-90}
LOW=${LOW_KBPS:-2000}
MED=${MED_KBPS:-10000}
bulk() { echo '{"type":"bulk_tcp","flow_id":4,"direction":"'"$1"'","rate_kbps":'"$2"'}'; }

phase hol-base           "$PHASE" "[$UDP]"
phase "hol-up-$LOW"      "$PHASE" "[$UDP, $(bulk ToServer "$LOW")]"
phase "hol-up-$MED"      "$PHASE" "[$UDP, $(bulk ToServer "$MED")]"
phase "hol-down-$LOW"    "$PHASE" "[$UDP, $(bulk FromServer "$LOW")]"
phase "hol-down-$MED"    "$PHASE" "[$UDP, $(bulk FromServer "$MED")]"
log "hol cycle done"

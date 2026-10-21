#!/usr/bin/env bash
# App-traffic cycle: the realtime UDP streams (lib.sh UDP) first alone, then
# beside simulated users (app_traffic flow):
#   app-base, app-web, app-video, app-social, app-mixed3   (default PHASES)
#   app-agent, app-agent4, app-agent-device/-cloud/-hybrid (AI agents)
#   app-websearch, app-websearch-cloud/-hybrid (an agent answering by web search: task graphs)
# PHASE_SEC (default 90), SCALE (object size factor, default 1.0), SEED.
# Data per default cycle: roughly 100-200 MB.
# One cycle per run; appends to ~/mp-data/logs/app.log. Each phase waits (up
# to WAIT_MIN, default 60 min) until the phone is idle and, with CELL_ONLY=1,
# on the mobile network.
set -uo pipefail
LOG_NAME=app
PHASES=${PHASES:-"app-base app-web app-video app-social app-mixed3"}
source "$(dirname "$0")/lib.sh"

PHASE=${PHASE_SEC:-90}
SCALE=${SCALE:-1.0}
SEED=${SEED:-$(date +%s)}
# users PROFILE WORKERS [PLACEMENT] — PLACEMENT (agent, websearch): device, cloud or hybrid
users() { echo '{"type":"app_traffic","flow_id":4,"profile":"'"$1"'","workers":'"$2"',"seed":'"$SEED"',"scale":'"$SCALE"${3:+',"placement":"'"$3"'"'}'}'; }

phase app-base   "$PHASE" "[$UDP]"
phase app-web    "$PHASE" "[$UDP, $(users web 1)]"
phase app-video  "$PHASE" "[$UDP, $(users video 1)]"
phase app-social "$PHASE" "[$UDP, $(users social 1)]"
phase app-mixed3 "$PHASE" "[$UDP, $(users mixed 3)]"
phase app-agent  "$PHASE" "[$UDP, $(users agent 1)]"
phase app-agent4 "$PHASE" "[$UDP, $(users agent 4)]"
# the same agents (same SEED) with the tools on the phone, in the cloud, split
phase app-agent-device "$PHASE" "[$UDP, $(users agent 2 device)]"
phase app-agent-cloud  "$PHASE" "[$UDP, $(users agent 2 cloud)]"
phase app-agent-hybrid "$PHASE" "[$UDP, $(users agent 2 hybrid)]"
phase app-websearch        "$PHASE" "[$UDP, $(users websearch 2 device)]"
phase app-websearch-cloud  "$PHASE" "[$UDP, $(users websearch 2 cloud)]"
phase app-websearch-hybrid "$PHASE" "[$UDP, $(users websearch 2 hybrid)]"
log "app cycle done"

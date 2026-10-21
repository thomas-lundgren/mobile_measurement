#!/usr/bin/env bash
# DSCP sweep: the same paced-UDP workload (engine udp_burst) once per
# marking, everything else equal, to see what each marking does on the path:
# kept, re-marked or bleached (dscp_recv), lost, delayed. The page's
# "UDP bursts" table shows the results.
#
#   MODE   video  — downlink video segments as bursts (default; profile
#                   video, same SEED in every phase)
#          upload — uplink bursts the phone marks: 1.2 MB at 8 Mbit/s every
#                   4 s (photo_upload)
#          voice  — a voice-like flow: 160-byte frames every 20 ms
#                   (64 kbit/s), half the phase down, then half up
#   MARKS  DSCP values, "none" = unmarked (default "none 46 34 40 8 24 none")
#   PHASE_SEC (default 60), SEED, WITH_UDP=1 adds the realtime streams (they
#   need more flow ports than home Wi-Fi reaches through the router), plus
#   lib.sh's CELL_ONLY / WAIT_MIN / PHASES. Phases are named dscp-MODE-MARK.
# One sweep per run; appends to ~/mp-data/logs/app.log.
set -uo pipefail
LOG_NAME=app
source "$(dirname "$0")/lib.sh"

MODE=${MODE:-video}
MARKS=${MARKS:-"none 46 34 40 8 24 none"}
PHASE=${PHASE_SEC:-60}
SEED=${SEED:-$(date +%s)}

# the flow's script actions (upload, voice) or profile (video)
workload() {
    case $MODE in
        video)
            echo '"profile":"video","engines":{"video_chunk":"udp_burst"}' ;;
        upload)
            local a="" t
            for ((t = 0; t < PHASE * 1000; t += 4000)); do
                a+="${a:+,}"'{"at_ms":'$t',"kind":"up","bytes":1200000,"rate_kbps":8000,"activity":"photo_upload"}'
            done
            echo '"profile":"script","actions":['"$a"'],"engines":{"photo_upload":"udp_burst"}' ;;
        voice)
            local half=$((PHASE * 1000 / 2)) bytes=$((PHASE / 2 * 50 * 160))
            echo '"profile":"script","actions":[{"at_ms":0,"kind":"down","bytes":'$bytes',"rate_kbps":64,"chunk_bytes":160,"activity":"voice"},{"at_ms":'$half',"kind":"up","bytes":'$bytes',"rate_kbps":64,"chunk_bytes":160,"activity":"voice"}],"engines":{"voice":"udp_burst"}' ;;
        *) log "dscp_sweep: unknown MODE $MODE"; exit 1 ;;
    esac
}

activity() {
    case $MODE in video) echo video_chunk ;; upload) echo photo_upload ;; voice) echo voice ;; esac
}

flow() {  # flow MARK
    local d=""
    [[ $1 != none ]] && d=',"dscp":{"'"$(activity)"'":'"$1"'}'
    echo '{"type":"app_traffic","flow_id":4,"workers":1,"seed":'"$SEED"',"scale":1.0,'"$(workload)$d"'}'
}

log "dscp sweep $MODE: $MARKS"
for m in $MARKS; do
    if [[ ${WITH_UDP:-0} == 1 ]]; then
        phase "dscp-$MODE-$m" "$PHASE" "[$UDP, $(flow "$m")]"
    else
        phase "dscp-$MODE-$m" "$PHASE" "[$(flow "$m")]"
    fi
done
log "dscp sweep $MODE done"

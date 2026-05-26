#!/usr/bin/env bash
# Port-forward the measurement-platform ports on a Ubiquiti EdgeRouter (EdgeOS)
# to the server that sits behind it. **Diff-then-apply**: reads the router's
# current port-forward configuration over SSH, computes only the settings that
# aren't already correct, and applies just those. A no-op exit on the second
# run is normal.
#
# Settings desired:
#   - auto-firewall + hairpin-nat enabled (lan loop-back for same-LAN tests)
#   - lan-interface / wan-interface bound
#   - rule 1: tcp  8080            → SERVER_IP:8080            (control plane)
#   - rule 2: udp  8081            → SERVER_IP:8081            (time sync)
#   - rule 3: tcp_udp DATA_RANGE   → SERVER_IP:DATA_RANGE      (flow ports)
#
# Usage:
#   ROUTER=admin@192.168.1.1 SERVER_IP=192.168.1.65 \
#     [WAN_IF=eth0] [LAN_IF=switch0] [DATA_RANGE=41000-41100] \
#     scripts/edgerouter_port_forward.sh [--dry-run]
#
# `--dry-run` still SSHes (read-only) to compute the diff, then prints what
# would change instead of applying. Rules 4+ added by hand on the router are
# left untouched — this script owns rules 1–3 plus the top-level port-forward
# settings.

set -euo pipefail

ROUTER="${ROUTER:-}"
SERVER_IP="${SERVER_IP:-}"
WAN_IF="${WAN_IF:-eth0}"
LAN_IF="${LAN_IF:-switch0}"
DATA_RANGE="${DATA_RANGE:-41000-41100}"
DRY_RUN=0

usage() {
    cat <<USAGE >&2
Usage:
  ROUTER=user@router SERVER_IP=<server LAN IP> \\
    [WAN_IF=eth0] [LAN_IF=switch0] [DATA_RANGE=41000-41100] \\
    $0 [--dry-run]

Required env vars: ROUTER, SERVER_IP.
USAGE
    exit 2
}

case "${1:-}" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help) usage ;;
    "") ;;
    *) echo "unknown arg: $1" >&2; usage ;;
esac
[[ -z "$ROUTER" || -z "$SERVER_IP" ]] && usage

# Desired state — exactly the `set` commands EdgeOS would emit. Order doesn't
# matter for the diff (we compare as a set), but kept logical for readability.
DESIRED=$(cat <<EOF
set port-forward auto-firewall enable
set port-forward hairpin-nat enable
set port-forward lan-interface $LAN_IF
set port-forward wan-interface $WAN_IF
set port-forward rule 1 description 'mp control plane (REST)'
set port-forward rule 1 forward-to address $SERVER_IP
set port-forward rule 1 forward-to port 8080
set port-forward rule 1 original-port 8080
set port-forward rule 1 protocol tcp
set port-forward rule 2 description 'mp time sync'
set port-forward rule 2 forward-to address $SERVER_IP
set port-forward rule 2 forward-to port 8081
set port-forward rule 2 original-port 8081
set port-forward rule 2 protocol udp
set port-forward rule 3 description 'mp data-generator flows'
set port-forward rule 3 forward-to address $SERVER_IP
set port-forward rule 3 forward-to port $DATA_RANGE
set port-forward rule 3 original-port $DATA_RANGE
set port-forward rule 3 protocol tcp_udp
EOF
)

echo "==> reading current port-forward config from $ROUTER"
# `show configuration commands` prints the running config as `set ...` lines,
# the same format we'll diff against. `|| true` so an empty section (router has
# no port-forward at all yet) doesn't trip pipefail.
CURRENT=$(
    ssh -o ConnectTimeout=10 "$ROUTER" \
        'show configuration commands | grep "^set port-forward" || true'
) || { echo "ERROR: ssh to $ROUTER failed" >&2; exit 1; }

# Lines in DESIRED but not in CURRENT — that's the change set.
MISSING=$(comm -23 \
    <(printf '%s\n' "$DESIRED" | sort -u) \
    <(printf '%s\n' "$CURRENT" | sort -u))

if [[ -z "$MISSING" ]]; then
    echo "==> already up to date — no changes."
    exit 0
fi

echo "==> $(printf '%s\n' "$MISSING" | wc -l) line(s) to apply:"
printf '%s\n' "$MISSING" | sed 's/^/    /'

if [[ $DRY_RUN -eq 1 ]]; then
    echo "==> --dry-run: not applying."
    exit 0
fi

APPLY=$(
    printf 'source /opt/vyatta/etc/functions/script-template\nconfigure\n'
    printf '%s\n' "$MISSING"
    printf 'commit\nsave\nexit\n'
)
echo "==> applying"
ssh -o ConnectTimeout=10 "$ROUTER" "vbash -l" <<<"$APPLY"
echo "==> done."

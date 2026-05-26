#!/usr/bin/env bash
# Open the measurement-platform ports on a Ubiquiti EdgeRouter (EdgeOS) to the
# server that sits behind it. **Diff-then-apply over SSH**; safe to re-run; a
# no-op exit means the state already matches.
#
# What this script does (and why):
#
#   1. Direct `service nat rule N` DNAT (NOT `port-forward`). On live ER configs
#      `port-forward` silently refused to generate any DNAT when its
#      `lan-interface` referenced an interface that didn't exist (e.g. the
#      router had `switch0` historically but the LAN is on `eth1`). Direct
#      `service nat` rules never have that surprise.
#   2. `firewall name WAN_IN rule N action accept` for each port. The default
#      EdgeOS wizard creates a WAN_IN ruleset with `default-action drop`, so
#      DNAT alone gets you nowhere — the packet is dropped after translation,
#      before forwarding. Both layers are required.
#   3. Auto-detect WAN + LAN interfaces from the router itself: WAN is the
#      interface with the default route; LAN is the one with an IP in the same
#      /24 as SERVER_IP. Override with WAN_IF / LAN_IF if you have something
#      weirder.
#   4. Cleans up any leftover `port-forward` config (only if present) so the
#      two layers can't fight each other.
#
# Strict ownership: the script owns `service nat rule {1,2,3}` and
# `firewall name WAN_IN rule {110,120,130}`. Anything outside those rule
# numbers (your other NAT, MASQ, other firewall rules) is left alone.
#
#   1. service nat rule 1   →  TCP  8080            → SERVER_IP:8080    (control plane REST)
#   2. service nat rule 2   →  UDP  8081            → SERVER_IP:8081    (UDP time sync)
#   3. service nat rule 3   →  tcp_udp DATA_RANGE   → SERVER_IP:RANGE   (flow ports)
#  110 firewall WAN_IN rule →  accept TCP 8080 → SERVER_IP
#  120 firewall WAN_IN rule →  accept UDP 8081 → SERVER_IP
#  130 firewall WAN_IN rule →  accept tcp_udp DATA_RANGE → SERVER_IP
#
# Usage:
#   ROUTER=admin@192.168.1.1 SERVER_IP=192.168.1.65 \
#     [WAN_IF=auto] [LAN_IF=auto] [DATA_RANGE=41000-41100] \
#     [INTERACTIVE_SSH=1] \
#     scripts/edgerouter_port_forward.sh [--dry-run]
#
# Tips: `ssh-copy-id $ROUTER` once → no password prompts at all (and you can
# drop `INTERACTIVE_SSH=1`). The script also multiplexes SSH so the read +
# apply share one authenticated session.

set -euo pipefail

# ----- args / env --------------------------------------------------------

ROUTER="${ROUTER:-}"
SERVER_IP="${SERVER_IP:-}"
WAN_IF="${WAN_IF:-auto}"
LAN_IF="${LAN_IF:-auto}"
DATA_RANGE="${DATA_RANGE:-41000-41100}"
DRY_RUN=0

usage() {
    cat <<USAGE >&2
Usage:
  ROUTER=user@router SERVER_IP=<server LAN IP> \\
    [WAN_IF=auto] [LAN_IF=auto] [DATA_RANGE=41000-41100] \\
    [INTERACTIVE_SSH=1] \\
    $0 [--dry-run]

Required env vars: ROUTER, SERVER_IP. WAN_IF/LAN_IF default to auto-detect.
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

# ----- input validation --------------------------------------------------

die() { echo "ERROR: $*" >&2; exit 2; }

validate_ipv4() {
    [[ "$1" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]] || die "invalid IPv4: $1"
    local IFS=.
    local -a o=($1)
    for n in "${o[@]}"; do
        (( n >= 0 && n <= 255 )) || die "invalid IPv4: $1"
    done
}

validate_iface() {
    [[ "$1" == "auto" ]] && return 0
    [[ "$1" =~ ^[a-zA-Z0-9._:-]+$ ]] || die "invalid interface name: $1"
}

validate_port_or_range() {
    [[ "$1" =~ ^([0-9]{1,5})(-([0-9]{1,5}))?$ ]] || die "invalid port/range: $1"
    local start="${BASH_REMATCH[1]}" end="${BASH_REMATCH[3]:-${BASH_REMATCH[1]}}"
    (( start >= 1 && start <= 65535 && end >= 1 && end <= 65535 && start <= end )) \
        || die "invalid port/range: $1"
}

validate_ipv4 "$SERVER_IP"
validate_iface "$WAN_IF"
validate_iface "$LAN_IF"
validate_port_or_range "$DATA_RANGE"

# ----- SSH options -------------------------------------------------------

SSH_SOCKET="${TMPDIR:-/tmp}/.mp-er-ssh-$$"
SSH_MUX=(-o "ControlMaster=auto" -o "ControlPath=$SSH_SOCKET" -o "ControlPersist=60")
SSH_OPTS=(-o ConnectTimeout=10 -o BatchMode=yes "${SSH_MUX[@]}")
[[ -n "${INTERACTIVE_SSH:-}" ]] && SSH_OPTS=(-o ConnectTimeout=10 "${SSH_MUX[@]}")
trap '
    ssh -O exit -o ControlPath="$SSH_SOCKET" "$ROUTER" >/dev/null 2>&1 || true
    rm -f "$SSH_SOCKET"
' EXIT

ssh_router() { ssh "${SSH_OPTS[@]}" "$ROUTER" "$@"; }

# ----- read current state ------------------------------------------------
# `show configuration commands` is an alias in interactive vbash only — wrap
# in `vbash -ic` so the non-interactive SSH session sees the alias.

echo "==> reading state from $ROUTER"
CURRENT=$(
    ssh_router 'vbash -ic "show configuration commands | grep -E \"^set (service nat|firewall name WAN_IN|port-forward) \" || true"'
) || die "ssh to $ROUTER failed (use INTERACTIVE_SSH=1 if a password prompt is needed)"

IFACE_INFO=$(
    ssh_router 'vbash -ic "ip -4 -o addr show; echo ---; ip route show default"'
)

# ----- auto-detect WAN_IF / LAN_IF ---------------------------------------

if [[ "$WAN_IF" == "auto" ]]; then
    WAN_IF=$(awk '/^---$/{f=1;next} f && /^default/{
        for (i=1;i<=NF;i++) if ($i=="dev") { print $(i+1); exit }
    }' <<<"$IFACE_INFO")
    [[ -n "$WAN_IF" ]] || die "could not auto-detect WAN_IF (no default route?); set WAN_IF explicitly"
fi

if [[ "$LAN_IF" == "auto" ]]; then
    LAN_PREFIX=$(awk -F. '{print $1"."$2"."$3"."}' <<<"$SERVER_IP")
    LAN_IF=$(awk -v pfx="$LAN_PREFIX" '/^---$/{exit} {
        # `ip -4 -o addr` rows: "N: <iface>    inet <a.b.c.d>/M ..."
        for (i=1;i<=NF;i++) if ($i ~ "^"pfx) { print $2; exit }
    }' <<<"$IFACE_INFO")
    [[ -n "$LAN_IF" ]] || die "could not auto-detect LAN_IF for $SERVER_IP; set LAN_IF explicitly"
fi

echo "    WAN_IF=$WAN_IF  LAN_IF=$LAN_IF  SERVER_IP=$SERVER_IP  DATA_RANGE=$DATA_RANGE"

# ----- desired state -----------------------------------------------------
# Each rule is built as a *set* of lines; the diff compares per rule, and we
# delete + recreate the rule when it doesn't match exactly.

nat_rule_desired() {
    case "$1" in
        1) cat <<EOF
set service nat rule 1 description 'mp control plane (REST)'
set service nat rule 1 type destination
set service nat rule 1 inbound-interface $WAN_IF
set service nat rule 1 protocol tcp
set service nat rule 1 destination port 8080
set service nat rule 1 inside-address address $SERVER_IP
set service nat rule 1 inside-address port 8080
EOF
        ;;
        2) cat <<EOF
set service nat rule 2 description 'mp time sync'
set service nat rule 2 type destination
set service nat rule 2 inbound-interface $WAN_IF
set service nat rule 2 protocol udp
set service nat rule 2 destination port 8081
set service nat rule 2 inside-address address $SERVER_IP
set service nat rule 2 inside-address port 8081
EOF
        ;;
        3) cat <<EOF
set service nat rule 3 description 'mp data-generator flows'
set service nat rule 3 type destination
set service nat rule 3 inbound-interface $WAN_IF
set service nat rule 3 protocol tcp_udp
set service nat rule 3 destination port $DATA_RANGE
set service nat rule 3 inside-address address $SERVER_IP
set service nat rule 3 inside-address port $DATA_RANGE
EOF
        ;;
    esac
}

fw_rule_desired() {
    case "$1" in
        110) cat <<EOF
set firewall name WAN_IN rule 110 action accept
set firewall name WAN_IN rule 110 description 'mp control plane (REST)'
set firewall name WAN_IN rule 110 destination address $SERVER_IP
set firewall name WAN_IN rule 110 destination port 8080
set firewall name WAN_IN rule 110 protocol tcp
EOF
        ;;
        120) cat <<EOF
set firewall name WAN_IN rule 120 action accept
set firewall name WAN_IN rule 120 description 'mp time sync'
set firewall name WAN_IN rule 120 destination address $SERVER_IP
set firewall name WAN_IN rule 120 destination port 8081
set firewall name WAN_IN rule 120 protocol udp
EOF
        ;;
        130) cat <<EOF
set firewall name WAN_IN rule 130 action accept
set firewall name WAN_IN rule 130 description 'mp data-generator flows'
set firewall name WAN_IN rule 130 destination address $SERVER_IP
set firewall name WAN_IN rule 130 destination port $DATA_RANGE
set firewall name WAN_IN rule 130 protocol tcp_udp
EOF
        ;;
    esac
}

# ----- diff: per managed rule (strict ownership) -------------------------

PLAN=""

# Clean up any leftover port-forward block (it's broken / fights us).
if grep -q "^set port-forward" <<<"$CURRENT"; then
    PLAN+="delete port-forward"$'\n'
    echo "--- leftover port-forward block will be deleted ---"
fi

reconcile_rule() {
    # $1=label  $2=current_filter_regex  $3=rule_num  $4=desired_fn
    local label="$1" filter="$2" n="$3" desired_fn="$4"
    local cur des cur_s des_s
    cur=$(grep -E "$filter" <<<"$CURRENT" || true)
    des=$("$desired_fn" "$n")
    cur_s=$(printf '%s\n' "$cur" | sort -u)
    des_s=$(printf '%s\n' "$des" | sort -u)
    if [[ "$cur_s" == "$des_s" ]]; then
        return 0
    fi
    echo "--- $label rule $n out of sync (- current, + desired) ---"
    diff -u --label "rule $n current" --label "rule $n desired" \
        <(printf '%s\n' "$cur_s") <(printf '%s\n' "$des_s") \
        | sed -n '4,$p' | sed 's/^/    /' || true
    if [[ -n "$cur" ]]; then
        PLAN+="delete ${label} rule $n"$'\n'
    fi
    PLAN+="$des"$'\n'
}

for n in 1 2 3; do
    reconcile_rule "service nat" "^set service nat rule $n " "$n" nat_rule_desired
done
for n in 110 120 130; do
    reconcile_rule "firewall name WAN_IN" "^set firewall name WAN_IN rule $n " "$n" fw_rule_desired
done

if [[ -z "$PLAN" ]]; then
    echo "==> already up to date — no changes."
    exit 0
fi

echo "==> plan ($(printf '%s' "$PLAN" | grep -c .) line(s)):"
printf '%s' "$PLAN" | sed 's/^/    /'

if [[ $DRY_RUN -eq 1 ]]; then
    echo "==> --dry-run: not applying."
    exit 0
fi

# ----- apply -------------------------------------------------------------

APPLY=$(
    printf 'source /opt/vyatta/etc/functions/script-template\nconfigure\n'
    printf '%s' "$PLAN"
    printf 'commit\nsave\nexit\n'
)
echo "==> applying"
ssh_router "vbash -l" <<<"$APPLY"
echo "==> done."

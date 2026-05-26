#!/usr/bin/env bash
# Port-forward the measurement-platform ports on a Ubiquiti EdgeRouter (EdgeOS)
# to the server that sits behind it.
#
# Idempotency / ownership model:
#   - Top-level keys (auto-firewall, hairpin-nat, lan-/wan-interface) are leaf
#     values; we apply only the ones whose current value differs from desired
#     (`set` overwrites the leaf).
#   - Rules 1–3 are **owned** by this script. If a rule's current line set
#     doesn't match the desired one exactly (extra fields, different value,
#     etc.), the rule is deleted and recreated within one commit. Rules 4+
#     added by hand on the router are left untouched.
#   - If nothing differs, the run is a no-op (no commit, no save).
#
# Configures rules:
#   1. tcp  8080            → SERVER_IP:8080            (control plane REST)
#   2. udp  8081            → SERVER_IP:8081            (UDP time sync)
#   3. tcp+udp DATA_RANGE   → SERVER_IP:DATA_RANGE      (data-generator flows)
# Also enables hairpin-NAT so phones on the LAN dialing the public address
# loop back (same-LAN caveat, see docs/OPERATORS_GUIDE.md §2.5).
#
# Usage:
#   ROUTER=admin@192.168.1.1 SERVER_IP=192.168.1.65 \
#     [WAN_IF=eth0] [LAN_IF=switch0] [DATA_RANGE=41000-41100] \
#     [INTERACTIVE_SSH=1] \
#     scripts/edgerouter_port_forward.sh [--dry-run]
#
# `--dry-run` still SSHes (read-only) to compute the real diff against the
# router, then prints the plan instead of applying. `INTERACTIVE_SSH=1` drops
# BatchMode so an SSH password prompt can succeed.

set -euo pipefail

# ----- args / env --------------------------------------------------------

ROUTER="${ROUTER:-}"
SERVER_IP="${SERVER_IP:-}"
WAN_IF="${WAN_IF:-eth5}"
LAN_IF="${LAN_IF:-switch0}"
DATA_RANGE="${DATA_RANGE:-41000-41100}"
DRY_RUN=0

usage() {
    cat <<USAGE >&2
Usage:
  ROUTER=user@router SERVER_IP=<server LAN IP> \\
    [WAN_IF=eth5] [LAN_IF=switch0] [DATA_RANGE=41000-41100] \\
    [INTERACTIVE_SSH=1] \\
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
# Default: fail fast (no host-key / password prompts). INTERACTIVE_SSH=1 drops
# BatchMode so a password prompt can succeed in ad-hoc use.
SSH_OPTS=(-o ConnectTimeout=10 -o BatchMode=yes)
[[ -n "${INTERACTIVE_SSH:-}" ]] && SSH_OPTS=(-o ConnectTimeout=10)

# ----- desired state -----------------------------------------------------

top_level_desired() {
    cat <<EOF
set port-forward auto-firewall enable
set port-forward hairpin-nat enable
set port-forward lan-interface $LAN_IF
set port-forward wan-interface $WAN_IF
EOF
}

rule_desired() {
    case "$1" in
        1) cat <<EOF
set port-forward rule 1 description 'mp control plane (REST)'
set port-forward rule 1 forward-to address $SERVER_IP
set port-forward rule 1 forward-to port 8080
set port-forward rule 1 original-port 8080
set port-forward rule 1 protocol tcp
EOF
        ;;
        2) cat <<EOF
set port-forward rule 2 description 'mp time sync'
set port-forward rule 2 forward-to address $SERVER_IP
set port-forward rule 2 forward-to port 8081
set port-forward rule 2 original-port 8081
set port-forward rule 2 protocol udp
EOF
        ;;
        3) cat <<EOF
set port-forward rule 3 description 'mp data-generator flows'
set port-forward rule 3 forward-to address $SERVER_IP
set port-forward rule 3 forward-to port $DATA_RANGE
set port-forward rule 3 original-port $DATA_RANGE
set port-forward rule 3 protocol tcp_udp
EOF
        ;;
    esac
}

# ----- fetch current state ----------------------------------------------

echo "==> reading current port-forward config from $ROUTER"
# `show configuration commands` is an alias loaded only in interactive vbash —
# a plain `ssh user@router '<cmd>'` runs a non-interactive shell that doesn't
# know it. `vbash -ic` loads the interactive profile so the alias resolves.
CURRENT=$(
    ssh "${SSH_OPTS[@]}" "$ROUTER" \
        "vbash -ic 'show configuration commands | grep \"^set port-forward\" || true'"
) || die "ssh to $ROUTER failed (use INTERACTIVE_SSH=1 if a password prompt is needed)"

# ----- diff: top-level (leaf-replace) -----------------------------------

TOP_CURRENT=$(
    printf '%s\n' "$CURRENT" \
        | grep -E '^set port-forward (auto-firewall|hairpin-nat|lan-interface|wan-interface) ' \
        || true
)
TOP_DESIRED=$(top_level_desired)
TOP_MISSING=$(comm -23 \
    <(printf '%s\n' "$TOP_DESIRED" | sort -u) \
    <(printf '%s\n' "$TOP_CURRENT" | sort -u))

# ----- diff: per-rule (strict ownership; delete + recreate if differs) ---

PLAN=""
[[ -n "$TOP_MISSING" ]] && PLAN+="$TOP_MISSING"$'\n'

for n in 1 2 3; do
    cur=$(printf '%s\n' "$CURRENT" | grep -E "^set port-forward rule $n " || true)
    des=$(rule_desired "$n")
    cur_s=$(printf '%s\n' "$cur" | sort -u)
    des_s=$(printf '%s\n' "$des" | sort -u)
    [[ "$cur_s" == "$des_s" ]] && continue   # in sync — leave it alone

    echo "--- rule $n out of sync (diff: - current, + desired) ---"
    diff -u --label "rule $n current" --label "rule $n desired" \
        <(printf '%s\n' "$cur_s") <(printf '%s\n' "$des_s") \
        | sed -n '4,$p' | sed 's/^/    /'

    # Delete existing rule (only if it actually exists) then recreate it whole
    # — this enforces "we own rules 1–3" without surprises from stale fields.
    if [[ -n "$cur" ]]; then
        PLAN+="delete port-forward rule $n"$'\n'
    fi
    PLAN+="$des"$'\n'
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
ssh "${SSH_OPTS[@]}" "$ROUTER" "vbash -l" <<<"$APPLY"
echo "==> done."

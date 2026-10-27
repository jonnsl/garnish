#!/bin/bash
# Host side of the guest network: a pool of tap devices for maitred, plus
# optional NAT.
#   ./setup-host-net.sh up [--nat]     # TAPS=8 ./setup-host-net.sh up for more VMs
#   ./setup-host-net.sh down
# Every tap gets its own /30 out of SUBNET: tap n has the host at .4n+1 and
# the guest at .4n+2, so one VM can't reach another. Keep SUBNET and
# TAP_PREFIX in sync with [Pool] in ~/.config/garnish/maitred.conf.
set -euo pipefail

TAPS="${TAPS:-4}"
TAP_PREFIX="${TAP_PREFIX:-crosvm_tap}"
SUBNET="${SUBNET:-192.168.10.0/24}"
TAP_USER="${TAP_USER:-${SUDO_USER:-$USER}}"

nat_rule=(POSTROUTING -s "$SUBNET" -j MASQUERADE)

IFS=./ read -r a b c d prefix <<<"$SUBNET"
base=$(( (a << 24) | (b << 16) | (c << 8) | d ))
max_taps=$(( 1 << (30 - prefix) ))
(( TAPS <= max_taps )) || { echo "TAPS: $SUBNET has room for $max_taps taps" >&2; exit 1; }

ipv4() {
    local n=$1
    echo "$(( (n >> 24) & 255 )).$(( (n >> 16) & 255 )).$(( (n >> 8) & 255 )).$(( n & 255 ))"
}

# The taps with our prefix, including the single crosvm_tap of older versions.
our_taps() {
    ip -o link show type tun 2>/dev/null | awk -F': ' '{ sub(/@.*/, "", $2); print $2 }' |
        grep -E "^${TAP_PREFIX}[0-9]*\$" || true
}

case "${1:-}" in
    up)
        for (( i = 0; i < TAPS; i++ )); do
            tap="$TAP_PREFIX$i"
            if ! ip link show "$tap" >/dev/null 2>&1; then
                sudo ip tuntap add mode tap user "$TAP_USER" vnet_hdr "$tap"
                sudo ip addr add "$(ipv4 $(( base + 4 * i + 1 )))/30" dev "$tap"
            fi
            sudo ip link set "$tap" up
        done
        echo "taps $TAP_PREFIX{0..$(( TAPS - 1 ))}, guests at $(ipv4 $(( base + 2 ))), $(ipv4 $(( base + 6 ))), ..."
        if [[ "${2:-}" == --nat ]]; then
            sudo sysctl -q -w net.ipv4.ip_forward=1
            sudo iptables -t nat -C "${nat_rule[@]}" 2>/dev/null ||
                sudo iptables -t nat -A "${nat_rule[@]}"
        fi
        ;;
    down)
        sudo iptables -t nat -D "${nat_rule[@]}" 2>/dev/null || true
        for tap in $(our_taps); do
            sudo ip link del "$tap"
        done
        ;;
    *)
        echo "usage: $0 up [--nat] | down" >&2
        exit 1
        ;;
esac

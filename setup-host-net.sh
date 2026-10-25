#!/bin/bash
# Host side of the guest network: a tap device for crosvm, plus optional NAT.
#   ./setup-host-net.sh up [--nat]
#   ./setup-host-net.sh down
# Keep the addresses in sync with GUEST_ADDRESS/GUEST_GATEWAY in mkosi.conf.
set -euo pipefail

TAP="${TAP:-crosvm_tap}"
HOST_ADDRESS="${HOST_ADDRESS:-192.168.10.1/24}"
SUBNET="${SUBNET:-192.168.10.0/24}"
TAP_USER="${TAP_USER:-${SUDO_USER:-$USER}}"

nat_rule=(POSTROUTING -s "$SUBNET" -j MASQUERADE)

case "${1:-}" in
    up)
        if ! ip link show "$TAP" >/dev/null 2>&1; then
            sudo ip tuntap add mode tap user "$TAP_USER" vnet_hdr "$TAP"
            sudo ip addr add "$HOST_ADDRESS" dev "$TAP"
        fi
        sudo ip link set "$TAP" up
        if [[ "${2:-}" == --nat ]]; then
            sudo sysctl -q -w net.ipv4.ip_forward=1
            sudo iptables -t nat -C "${nat_rule[@]}" 2>/dev/null ||
                sudo iptables -t nat -A "${nat_rule[@]}"
        fi
        ;;
    down)
        sudo iptables -t nat -D "${nat_rule[@]}" 2>/dev/null || true
        sudo ip link del "$TAP" 2>/dev/null || true
        ;;
    *)
        echo "usage: $0 up [--nat] | down" >&2
        exit 1
        ;;
esac

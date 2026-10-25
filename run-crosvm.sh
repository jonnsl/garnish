#!/bin/bash
# Boot an image built by this project with crosvm (direct kernel boot).
#   ./run-crosvm.sh debian            # uses mkosi.output/debian.{raw,vmlinuz,initrd}
#   CID=4 MEM=8192 ./run-crosvm.sh fedora
#   TAP= ./run-crosvm.sh arch         # no network
#   SHARE_HOME=~/vm/home ./run-crosvm.sh debian   # host dir as the guest home
# Then: ssh jonnathan@vsock/3   (needs systemd >= 256 on the host for the
#       vsock/ ssh proxy; otherwise use ProxyCommand="socat - VSOCK-CONNECT:3:22")
set -euo pipefail

name="${1:?usage: $0 <archlinux|debian|ubuntu|fedora> [extra crosvm args...]}"
shift
out="${OUTPUT_DIR:-$(dirname "$0")/mkosi.output}"
CID="${CID:-3}"
CPUS="${CPUS:-4}"
MEM="${MEM:-4096}"
# cross-domain is what sommelier --virtgpu-channel uses. Add e.g.
# GPU_CONTEXTS=cross-domain:virgl2 only if crosvm was built with virglrenderer.
GPU_CONTEXTS="${GPU_CONTEXTS:-cross-domain}"
TAP="${TAP-crosvm_tap}"   # created by ./setup-host-net.sh up; TAP= disables networking
# Host directory to mount over the guest user's home with virtio-fs. crosvm
# serves it itself, as your user: no virtiofsd and no root needed.
SHARE_HOME="${SHARE_HOME:-}"
GUEST_USER="${GUEST_USER:-jonnathan}"   # must match the build's GUEST_USER
GUEST_UID="${GUEST_UID:-1000}"          # `id -u` in the guest
GUEST_GID="${GUEST_GID:-$GUEST_UID}"

# Keep in sync with KernelCommandLine= in mkosi.conf.
cmdline="root=PARTLABEL=root rw console=ttyS0 systemd.firstboot=off video=Virtual-1:d"

share_args=()
if [[ -n "$SHARE_HOME" ]]; then
    [[ -d "$SHARE_HOME" ]] || { echo "SHARE_HOME: '$SHARE_HOME' is not a directory" >&2; exit 1; }
    # The device runs in a user namespace where only the guest user's ids
    # exist, mapped to yours. Files the guest user creates are owned by you on
    # the host. Other host owners show up as nobody, and guest root can't
    # create files there.
    share_args=(--shared-dir "$(realpath "$SHARE_HOME"):home:type=fs:cache=auto:uidmap=$GUEST_UID $(id -u) 1:gidmap=$GUEST_GID $(id -g) 1:uid=$GUEST_UID:gid=$GUEST_GID")
    # nofail: if the mount fails, the guest still boots with the home from the image.
    cmdline+=" systemd.mount-extra=home:/home/$GUEST_USER:virtiofs:nofail,x-systemd.before=systemd-user-sessions.service"
fi

net_args=()
if [[ -n "$TAP" ]]; then
    if ip link show "$TAP" >/dev/null 2>&1; then
        net_args=(--net "tap-name=$TAP")
    else
        echo "warning: tap '$TAP' not found, starting without network (run ./setup-host-net.sh up)" >&2
    fi
fi

exec crosvm run \
    --cpus "$CPUS" \
    --mem "$MEM" \
    --block "path=$out/$name.raw" \
    --initrd "$out/$name.initrd" \
    --params "$cmdline" \
    --vsock "cid=$CID" \
    --gpu "context-types=$GPU_CONTEXTS" \
    --wayland-sock "$XDG_RUNTIME_DIR/${WAYLAND_DISPLAY:-wayland-0}" \
    "${net_args[@]}" \
    "${share_args[@]}" \
    "$@" \
    "$out/$name.vmlinuz"

#!/bin/bash
# Boot an image built by this project with crosvm (direct kernel boot).
#   ./run-crosvm.sh debian            # uses mkosi.output/debian.{raw,vmlinuz,initrd}
#   CID=4 MEM=8192 ./run-crosvm.sh fedora
# Then: ssh jonnathan@vsock/3   (needs systemd >= 256 on the host for the
#       vsock/ ssh proxy; otherwise use ProxyCommand="socat - VSOCK-CONNECT:3:22")
set -euo pipefail

name="${1:?usage: $0 <archlinux|debian|ubuntu|fedora> [extra crosvm args...]}"
shift
out="${OUTPUT_DIR:-$(dirname "$0")/mkosi.output}"
CID="${CID:-3}"
CPUS="${CPUS:-4}"
MEM="${MEM:-4096}"

# Keep in sync with KernelCommandLine= in mkosi.conf.
cmdline="root=PARTLABEL=root rw console=ttyS0 systemd.firstboot=off video=Virtual-1:d"

exec crosvm run \
    --cpus "$CPUS" \
    --mem "$MEM" \
    --block "path=$out/$name.raw" \
    --initrd "$out/$name.initrd" \
    --params "$cmdline" \
    --vsock "cid=$CID" \
    --gpu context-types=cross-domain:virgl2 \
    --wayland-sock "$XDG_RUNTIME_DIR/${WAYLAND_DISPLAY:-wayland-0}" \
    "$@" \
    "$out/$name.vmlinuz"

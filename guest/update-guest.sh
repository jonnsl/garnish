#!/bin/bash
# Installs freshly built guest packages in a running VM, over maitre's ssh.
#
#   update-guest.sh VM [DIR]
#
# DIR defaults to mkosi/mkosi.output/packages-<distribution>, where an
# image build (or `mkosi -d <distribution> -t none build`) leaves them;
# the distribution is read from the guest's /etc/os-release.
#
# sudo in the guest may ask for the password. sommelier keeps running the
# old binary until it restarts: reboot the VM, or run
# `systemctl --user restart sommelier` in it, which closes every guest window.
set -euo pipefail

if (( $# < 1 || $# > 2 )); then
    echo "usage: $0 VM [DIR]" >&2
    exit 2
fi
vm=$1
guest=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$guest")

id=$(maitre ssh "$vm" cat /etc/os-release | sed -n 's/^ID=//p' | tr -d '"')
dir=${2:-$repo/mkosi/mkosi.output/packages-$id}
if ! compgen -G "$dir/*.pkg.tar.zst" >/dev/null && ! compgen -G "$dir/*.deb" >/dev/null \
        && ! compgen -G "$dir/*.rpm" >/dev/null; then
    echo "no packages in $dir; build them with: mkosi -d $id -t none build" >&2
    exit 1
fi

echo "copying $(cd "$dir" && echo *) to $vm"
remote=$(maitre ssh "$vm" mktemp -d)
tar -c -C "$dir" . -C "$guest" install-packages.sh \
    | maitre ssh "$vm" tar -x -C "$remote"

# ssh takes options after the host too: -t gives sudo a terminal for the
# password.
status=0
maitre ssh "$vm" -t sudo bash "$remote/install-packages.sh" "$remote" || status=$?
maitre ssh "$vm" "rm -rf $remote; systemctl --user daemon-reload"
if (( status != 0 )); then
    exit "$status"
fi

echo
echo "Done. The new sommelier runs after a reboot of $vm, or after"
echo "\`systemctl --user restart sommelier\` in it (that closes every guest window)."

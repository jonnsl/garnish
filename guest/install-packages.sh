#!/bin/bash
# Runs as root inside a guest: installs the packages in DIR (sommelier,
# garnish-guest) with the guest's package manager, then removes the copies
# that images built before the packages existed put in /usr/local and
# /etc. update-guest.sh copies this script into the guest with the
# packages and runs it.
#
#   install-packages.sh DIR
set -euo pipefail

dir=$1
. /etc/os-release
shopt -s nullglob

# Reinstalls when the version is the same, so a rebuilt package always
# replaces the installed one.
case "$ID" in
    arch)
        pkgs=("$dir"/*.pkg.tar.zst)
        (( ${#pkgs[@]} )) || { echo "no packages in $dir" >&2; exit 1; }
        pacman -U --noconfirm "${pkgs[@]}"
        ;;
    debian|ubuntu)
        pkgs=("$dir"/*.deb)
        (( ${#pkgs[@]} )) || { echo "no packages in $dir" >&2; exit 1; }
        apt-get install -y --reinstall --allow-downgrades "${pkgs[@]}"
        ;;
    fedora)
        pkgs=("$dir"/*.rpm)
        (( ${#pkgs[@]} )) || { echo "no packages in $dir" >&2; exit 1; }
        rpm -U --verbose --hash --replacepkgs --oldpackage "${pkgs[@]}"
        ;;
    *)
        echo "unsupported distribution '$ID'" >&2
        exit 1
        ;;
esac

# What the image scripts used to install. The ones in /etc would hide the
# packaged units and environment, so they have to go.
legacy=(
    /usr/local/bin/sommelier
    /usr/local/bin/carte-agent
    /etc/systemd/user/default.target.wants/sommelier.service
    /etc/systemd/user/sommelier.service
    /etc/systemd/user/default.target.wants/carte-agent.service
    /etc/systemd/user/carte-agent.service
    /etc/systemd/system/multi-user.target.wants/gerant-ready.service
    /etc/systemd/system/gerant-ready.service
    /etc/profile.d/crosvm-wayland.sh
    /etc/environment.d/50-crosvm-wayland.conf
)
for f in "${legacy[@]}"; do
    if [[ -e "$f" || -L "$f" ]]; then
        echo "removing $f (now in a package)"
        rm -f "$f"
    fi
done

systemctl daemon-reload

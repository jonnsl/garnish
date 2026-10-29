#!/bin/sh
# Installs garnish-guest under $1, with the carte-agent binary at $2.
# Every package format runs it, so the file list lives in one place.
#
# The units are enabled with symlinks in the package instead of
# maintainer scripts, so installing the package is all a guest needs.
set -eu

dest=$1
carte_agent=$2
files=$(dirname "$0")/files

install -Dm755 "$carte_agent" "$dest/usr/bin/carte-agent"

install -Dm644 "$files/carte-agent.service" "$dest/usr/lib/systemd/user/carte-agent.service"
install -d "$dest/usr/lib/systemd/user/default.target.wants"
ln -sf ../carte-agent.service "$dest/usr/lib/systemd/user/default.target.wants/carte-agent.service"

install -Dm644 "$files/maitre-ready.service" "$dest/usr/lib/systemd/system/maitre-ready.service"
install -d "$dest/usr/lib/systemd/system/multi-user.target.wants"
ln -sf ../maitre-ready.service "$dest/usr/lib/systemd/system/multi-user.target.wants/maitre-ready.service"

install -Dm644 "$files/garnish-wayland.env" "$dest/usr/lib/environment.d/50-garnish-wayland.conf"
install -Dm644 "$files/garnish-wayland.sh" "$dest/etc/profile.d/garnish-wayland.sh"

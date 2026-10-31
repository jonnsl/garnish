#!/bin/sh
# Installs garnish-guest under $1, with the carte-agent binary at $2 and
# sonnette-agent at $3.
# Every package format runs it, so the file list lives in one place.
#
# The units are enabled with symlinks in the package instead of
# maintainer scripts, so installing the package is all a guest needs.
set -eu

dest=$1
carte_agent=$2
sonnette_agent=$3
files=$(dirname "$0")/files

install -Dm755 "$carte_agent" "$dest/usr/bin/carte-agent"

install -Dm644 "$files/carte-agent.service" "$dest/usr/lib/systemd/user/carte-agent.service"
install -d "$dest/usr/lib/systemd/user/default.target.wants"
ln -sf ../carte-agent.service "$dest/usr/lib/systemd/user/default.target.wants/carte-agent.service"

install -Dm755 "$sonnette_agent" "$dest/usr/bin/sonnette-agent"
install -Dm644 "$files/sonnette-agent.service" "$dest/usr/lib/systemd/user/sonnette-agent.service"
ln -sf ../sonnette-agent.service "$dest/usr/lib/systemd/user/default.target.wants/sonnette-agent.service"
install -Dm644 "$files/org.garnish.sonnette.service" "$dest/usr/share/dbus-1/services/org.garnish.sonnette.service"

install -Dm644 "$files/gerant-ready.service" "$dest/usr/lib/systemd/system/gerant-ready.service"
install -d "$dest/usr/lib/systemd/system/multi-user.target.wants"
ln -sf ../gerant-ready.service "$dest/usr/lib/systemd/system/multi-user.target.wants/gerant-ready.service"

install -Dm644 "$files/garnish-wayland.env" "$dest/usr/lib/environment.d/50-garnish-wayland.conf"
install -Dm644 "$files/garnish-wayland.sh" "$dest/etc/profile.d/garnish-wayland.sh"

install -Dm644 "$files/garnish-wireplumber.conf" "$dest/usr/share/wireplumber/wireplumber.conf.d/50-garnish.conf"

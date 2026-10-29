#!/bin/sh
# Installs the files garnish adds to the sommelier package, under $1.
# Every package format runs it after sommelier's own `meson install`.
set -eu

dest=$1
here=$(dirname "$0")

install -Dm644 "$here/sommelier.service" "$dest/usr/lib/systemd/user/sommelier.service"
install -d "$dest/usr/lib/systemd/user/default.target.wants"
ln -sf ../sommelier.service "$dest/usr/lib/systemd/user/default.target.wants/sommelier.service"

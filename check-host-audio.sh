#!/bin/bash
# Check the host prerequisites for guest audio over vhost-user (option B):
#   crosvm --vhost-user type=sound  <->  vhost-device-sound --backend pipewire
# Read-only: no sudo, installs nothing. Run it as the desktop user, in the
# graphical session (it needs XDG_RUNTIME_DIR and the user's PipeWire).
#   ./check-host-audio.sh
set -uo pipefail

fails=0 warns=0
ok()   { printf '  \e[32mOK\e[0m    %s\n' "$*"; }
warn() { printf '  \e[33mWARN\e[0m  %s\n' "$*"; warns=$((warns + 1)); }
fail() { printf '  \e[31mFAIL\e[0m  %s\n' "$*"; fails=$((fails + 1)); }
hdr()  { printf '\n== %s\n' "$*"; }
pkg()  { pacman -Q "$1" 2>/dev/null; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

hdr "System"
printf '  kernel %s, user %s (uid %s)\n' "$(uname -r)" "$USER" "$(id -u)"
if [[ -n "${XDG_RUNTIME_DIR:-}" && -d "$XDG_RUNTIME_DIR" ]]; then
    ok "XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR"
else
    fail "XDG_RUNTIME_DIR is not set; run this from your desktop session"
fi

hdr "crosvm"
if command -v crosvm >/dev/null; then
    ok "$(command -v crosvm) ($(pacman -Qo "$(command -v crosvm)" 2>/dev/null | awk '{print $(NF-1), $NF}'))"
    # Parsing succeeds (and crosvm only fails on the missing kernel) if the
    # vhost-user frontend knows the sound device type.
    out="$(timeout 10 crosvm run --vhost-user "type=sound,socket=$tmp/none" "$tmp/no-kernel" 2>&1)"
    if grep -q 'arg parsing failed' <<<"$out"; then
        fail "crosvm rejects --vhost-user type=sound:"
        grep 'arg parsing failed' <<<"$out" | sed 's/^/          /'
    else
        ok "crosvm accepts --vhost-user type=sound"
    fi
else
    fail "crosvm not found in PATH"
fi

hdr "vhost-device-sound"
if command -v vhost-device-sound >/dev/null; then
    ok "$(command -v vhost-device-sound) ($(vhost-device-sound --version 2>/dev/null || echo 'version unknown'))"
    help="$(vhost-device-sound --help 2>&1)"
    if grep -qi pipewire <<<"$help"; then
        ok "built with the pipewire backend"
    else
        fail "no pipewire backend in --help (built without the pipewire feature?)"
    fi
    # gerantd passes --no-capture unless the VM has Microphone=yes.
    if grep -q -- '--no-capture' <<<"$help"; then
        ok "supports --no-capture (patched build)"
    else
        fail "no --no-capture option: rebuild the patched package (cd host/vhost-device-sound && makepkg -sfi)"
    fi
    # Smoke test: start it against the user's PipeWire and wait for the socket.
    if grep -qi pipewire <<<"$help"; then
        sock="$tmp/snd.sock"
        vhost-device-sound --socket "$sock" --backend pipewire >"$tmp/vds.log" 2>&1 &
        pid=$!
        for _ in $(seq 20); do [[ -S "$sock" ]] && break; kill -0 "$pid" 2>/dev/null || break; sleep 0.25; done
        if [[ -S "$sock" ]]; then
            ok "starts with --backend pipewire and listens on its socket"
        else
            fail "did not create its socket with --backend pipewire; log:"
            sed 's/^/          /' "$tmp/vds.log" | head -20
        fi
        kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
    fi
else
    fail "vhost-device-sound not found in PATH"
    printf '  info  to install: (cd host/vhost-device-sound && makepkg -si)\n'
    printf '  info  build deps for `cargo install vhost-device-sound`:\n'
    for p in rust clang pkgconf pipewire alsa-lib; do
        if v="$(pkg "$p")"; then ok "$v"; else warn "package '$p' not installed"; fi
    done
    if command -v pkg-config >/dev/null; then
        for pc in libpipewire-0.3 alsa; do
            pkg-config --exists "$pc" && ok "pkg-config finds $pc" || warn "pkg-config can't find $pc"
        done
    fi
    pacman -Ss '^vhost-device' 2>/dev/null | sed 's/^/  repo  /'
fi

hdr "PipeWire"
for p in pipewire wireplumber pipewire-pulse pipewire-media-session; do
    v="$(pkg "$p")" && printf '  pkg   %s\n' "$v"
done
if [[ -S "${XDG_RUNTIME_DIR:-/nonexistent}/pipewire-0" ]]; then
    ok "socket $XDG_RUNTIME_DIR/pipewire-0 exists"
else
    fail "no PipeWire socket at \$XDG_RUNTIME_DIR/pipewire-0"
fi
# pipewire.service being inactive is fine while the socket is active: it is
# socket-activated and starts on the first client.
if [[ "$(systemctl --user is-active pipewire.service 2>/dev/null)" == active ]]; then
    ok "pipewire.service active"
elif [[ "$(systemctl --user is-active pipewire.socket 2>/dev/null)" == active ]]; then
    ok "pipewire.socket active (service starts on demand)"
else
    fail "neither pipewire.service nor pipewire.socket is active"
fi
sm=""
for u in wireplumber.service pipewire-media-session.service; do
    [[ "$(systemctl --user is-active "$u" 2>/dev/null)" == active ]] && sm="$u"
done
if [[ -n "$sm" ]]; then
    ok "session manager running: $sm"
else
    fail "no PipeWire session manager running (install/enable wireplumber); streams won't be routed"
fi

hdr "Output device"
if command -v wpctl >/dev/null && [[ -n "$sm" ]]; then
    if sink="$(wpctl inspect @DEFAULT_AUDIO_SINK@ 2>/dev/null)" && [[ -n "$sink" ]]; then
        name="$(grep -m1 -E 'node.description|node.name' <<<"$sink" | cut -d'"' -f2)"
        ok "default sink: ${name:-?}"
    else
        fail "no default audio sink (wpctl status shows what PipeWire sees)"
    fi
    if src="$(wpctl inspect @DEFAULT_AUDIO_SOURCE@ 2>/dev/null)" && [[ -n "$src" ]]; then
        name="$(grep -m1 -E 'node.description|node.name' <<<"$src" | cut -d'"' -f2)"
        printf '  info  default source (microphone): %s\n' "${name:-?}"
    fi
else
    warn "skipped: needs wpctl (wireplumber) and a running session manager"
fi

hdr "Summary"
if (( fails )); then
    printf '  %d failed, %d warnings\n' "$fails" "$warns"
    exit 1
fi
printf '  all checks passed (%d warnings)\n' "$warns"

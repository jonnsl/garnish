# carte

Puts the applications installed in a crosvm guest into the host's application
menu, and starts them in the guest when you click one. A small take on
ChromeOS's [garcon](https://chromium.googlesource.com/chromiumos/platform2/+/HEAD/vm_tools/garcon/)
for guests whose windows reach the host through sommelier.

Two programs:

- `carte-agent` runs in the guest's user session. It reads the guest's
  `.desktop` files, watches them for changes, and serves the list, the icons
  and launch requests over vsock.
- `carte` runs on the host. It writes a launcher for each guest application
  to `~/.local/share/applications`, and keeps them in sync.

## Install

In the guest, the `garnish-guest` package (see `guest/` at the top of this
repository) installs `/usr/bin/carte-agent` and enables
`carte-agent.service` for every user. The images install it. By hand:

```sh
cargo build --release --locked --bin carte-agent
install -D target/release/carte-agent ~/.local/bin/carte-agent
```

and run it from a user unit, with `SOMMELIER_VM_IDENTIFIER` set as for
sommelier (see [VM names](#vm-names)).

On the host:

```sh
cargo install --locked --path . --bin carte   # to ~/.cargo/bin
```

or, on Arch, `makepkg -si` in `host/carte` (to `/usr/bin`, with the shell
completions).

Shell completions for `carte` complete subcommands, the VMs that have
launchers, and their application IDs for `carte launch`. Without the package:

```sh
install -Dm644 completions/carte.bash ~/.local/share/bash-completion/completions/carte
# zsh: a directory in $fpath, added before compinit in ~/.zshrc, e.g.
# fpath=(~/.local/share/zsh/site-functions $fpath)
install -Dm644 completions/_carte ~/.local/share/zsh/site-functions/_carte
```

[maitred](../maitre/README.md) runs `carte sync` for every VM it starts if
`carte` is in its `PATH` (`AppMenu=no` turns it off) and logs to
`~/.local/state/garnish/<name>/carte.log`.

Builds with Rust 1.85 or newer. Dependencies: `libc`, `serde`, `serde_json`.

## Usage

```sh
carte sync --cid 3          # follow the VM at CID 3 until killed
carte launch debian org.gnome.Calculator
carte remove debian         # delete debian's launchers and icons
```

`carte sync` waits for the agent, retrying every second, and reconnects when
the agent restarts or the VM reboots. On every change in the guest it:

- writes `~/.local/share/applications/carte-<vm>-<id>.desktop`,
- fetches the icon into `~/.local/share/carte/icons/<vm>/<id>.{png,svg}`,
- removes the launchers and icons of applications that are gone.

GNOME picks the files up immediately. Files are replaced atomically and only
when their content changed. While connected, it records the CID in
`$XDG_RUNTIME_DIR/carte/<vm>.cid`, which is how `carte launch` finds the VM.

Each launcher runs `carte launch <vm> <id>`. Launchers stay when the VM stops,
so applications pinned to the dash stay too. Starting one while the VM is off
shows a notification (`notify-send`).

## What gets exported

The agent reads `applications/` under `$XDG_DATA_HOME` and `$XDG_DATA_DIRS`,
including subdirectories, following the desktop entry spec: the first file
with a given desktop ID wins. Like garcon, it leaves out entries that are not
`Type=Application`, have `NoDisplay` or `Hidden`, are in the `Settings`
category, are hidden from GNOME by `OnlyShowIn`/`NotShowIn`, or have a
`TryExec` that isn't installed. It also leaves out `Terminal=true` entries,
since there is no terminal to run them in.

Changes are picked up with inotify, after the directories have been quiet for
a second. Directories that don't exist yet are watched through their nearest
existing parent.

Launchers get the guest's `Name` (with ` (<vm>)` appended), `GenericName`,
`Comment`, `Keywords` (plus the VM name, so searching for it lists all of its
applications) and `Categories`, in every language the guest has.

## Starting applications

The agent re-reads the desktop file, applies the same filter, expands `Exec=`
(field codes for files and URLs are dropped, `%i`, `%c` and `%k` are
expanded), and runs it with

```
systemd-run --user --scope --quiet --collect --unit=app-carte-<id>-<n>.scope -- <argv>
```

so every application gets its own scope, like on a GNOME desktop, and outlives
the agent. It runs in `Path=` if set, else in the home directory, with the user
manager's environment. That environment needs the Wayland variables
(`WAYLAND_DISPLAY` and friends); the images put them in
`/etc/environment.d/`.

## VM names

sommelier renames the app_id of every Wayland window to
`org.chromium.guest_os.<vm>.wayland.<app_id>`, where `<vm>` is
`SOMMELIER_VM_IDENTIFIER` (default `termina`). The agent reads the same
variable and sends it as the VM name, and the launchers set `StartupWMClass` to
that renamed app_id. GNOME uses it to show a window under its launcher instead
of with a generic icon. This is best effort: it only works for applications
whose app_id is their desktop ID or their `StartupWMClass`.

VM names may contain letters, digits, `_` and `.` (no `-`, so the
`carte-<vm>-` prefix belongs to one VM) and are at most 64 characters.

## Protocol

Newline-delimited JSON on vsock port 8890. The agent only accepts connections
from CID 2, the host. One request per line:

| Request                                           | Reply |
|---------------------------------------------------|-------|
| `{"type":"watch"}`                                | `{"vm":"debian","apps":[...]}` now and after every change, until the connection closes |
| `{"type":"icon","id":"<id>","size":256}`          | `{"format":"png","len":N}` followed by N raw bytes, or `{"error":"..."}` |
| `{"type":"launch","vm":"<vm>","id":"<id>"}`       | `{}` or `{"error":"..."}` |

`icon` and `launch` can be repeated on one connection. An application in
`apps` looks like this (localized fields are keyed by locale, `""` is the
default):

```json
{"id": "org.gnome.Calculator", "name": {"": "Calculator", "de": "Rechner"},
 "generic_name": {}, "comment": {"": "Perform calculations"},
 "keywords": {"": ["math"]}, "categories": ["Utility"],
 "icon": "org.gnome.Calculator", "startup_wm_class": null}
```

Lines are limited to 16 MiB and icons to 4 MiB.

## Security

Everything from the guest is untrusted. The host checks the VM name and every
application ID (`[A-Za-z0-9._-]`, no leading `.`, at most 200 characters)
before using them in file names. Guest text only goes into display keys of the
`.desktop` files, escaped. `Exec=` is built on the host from the checked VM
name and ID, so a guest can't make the host run anything but
`carte launch`. Icons are only accepted as PNG or SVG, up to 4 MiB.

In the guest, the agent only starts applications that pass the export filter
and only accepts requests from the host.

## Not done

- opening host files with guest applications (no MIME types are passed on)
- desktop actions (`[Desktop Action ...]`)
- `Terminal=true` applications
- garcon's other direction: opening URLs in the host browser, opening a
  terminal

## Code

| File                    | Contents |
|-------------------------|----------|
| `src/proto.rs`          | messages and ID/VM name checks |
| `src/vsock.rs`          | AF_VSOCK connect and listen |
| `src/desktop.rs`        | desktop entry parsing, filtering, `Exec=` expansion, directory scan |
| `src/icons.rs`          | icon theme lookup |
| `src/inotify.rs`        | recursive directory watching |
| `src/menu.rs`           | the host's `.desktop` files |
| `src/xdg.rs`            | XDG base directories |
| `completions/`          | bash and zsh completions for `carte` |
| `src/bin/carte-agent.rs`| the guest agent |
| `src/bin/carte.rs`      | the host tool |

`cargo test` runs the unit tests.

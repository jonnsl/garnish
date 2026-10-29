# sonnette

Shows the desktop notifications of a crosvm guest on the host, and passes
clicks and dismissals back to the guest application. A small take on
ChromeOS's [notificationd](https://chromium.googlesource.com/chromiumos/platform2/+/HEAD/vm_tools/notificationd/),
which does the same through a Wayland extension; sonnette uses vsock, like
[carte](../carte/README.md). The name is the bell that rings when an order is
ready.

Two programs:

- `sonnette-agent` runs in the guest's user session. It is the guest's
  notification server: it owns `org.freedesktop.Notifications` on the
  session bus and hands every notification to the host over vsock.
- `sonnette relay` runs on the host. It shows the notifications through the
  host's own notification server (GNOME Shell, KDE Plasma, mako, ...) and
  sends back the actions the user picks and the notifications that close.

## Install

In the guest, the `garnish-guest` package (see `guest/` at the top of this
repository) installs `/usr/bin/sonnette-agent`, enables
`sonnette-agent.service` for every user, and adds a D-Bus service file, so
the agent also starts the first time an application sends a notification.
The images install it. By hand:

```sh
cargo build --release --locked --bin sonnette-agent
install -D target/release/sonnette-agent ~/.local/bin/sonnette-agent
```

and run it from a user unit with `Type=dbus` and
`BusName=org.freedesktop.Notifications`.

On the host:

```sh
cargo install --locked --path . --bin sonnette   # to ~/.cargo/bin
```

or, on Arch, `makepkg -si` in `host/garnish` (to `/usr/bin`; sonnette is in
the `garnish-host` package, with maitre and carte).

[maitred](../maitre/README.md) runs `sonnette relay --cid <cid>` for every
VM it starts if `sonnette` is in its `PATH` (`Notifications=no` turns it
off), and logs to `~/.local/state/garnish/<name>/sonnette.log`.

Builds with Rust 1.85 or newer. It uses the `carte` crate next to it (icon
lookup, XDG directories, name checks), so it needs `../carte`. Dependencies:
`zbus`, `tokio`, `serde`, `serde_json`, `futures-util`, `libc`.

## Usage

```sh
sonnette relay --cid 3    # follow the VM at CID 3 until killed
```

It waits for the agent, retrying every second, and reconnects when the agent
restarts or the VM reboots. When the connection ends, or the relay gets
SIGTERM or SIGINT, it closes every notification it showed, since nobody is
left to act on them.

## How notifications look on the host

- The application name gets the VM's name: `Firefox (debian)`.
- If carte has a launcher for the sending application,
  `carte-<vm>.<id>.desktop`, the notification names it in its
  `desktop-entry` hint. GNOME then shows the launcher's name and icon, and
  lists the application under Settings → Notifications, where it can be
  turned off on its own.
- The sending application is the `desktop-entry` hint the application gave
  (GLib's `GNotification` sets it), or else the application carte started
  it in: the agent asks the bus for the sender's PID and reads its scope,
  `app-carte-<id>-<n>.scope`, from `/proc/<pid>/cgroup`.
- Icons are read in the guest: an `app_icon` or `image-path` that is a file
  (path or `file://` URI) or an icon name, looked up in the guest's icon
  themes like carte does. They are sent as PNG or SVG and written to
  `$XDG_RUNTIME_DIR/sonnette/<vm>/` on the host, for the server to read,
  until the notification closes. `image-data` (raw pixels) is passed on as
  it is.
- Hints that mean the same on the host are passed on: `urgency`, `category`,
  `resident`, `transient`, `action-icons`, `sound-name`, `suppress-sound`.
  The others are dropped: `sound-file` names a guest file, `x` and `y` are
  guest screen positions.

`GetCapabilities` in the guest answers with what the host's server can do,
out of `actions`, `action-icons`, `body`, `body-markup`, `icon-static`,
`persistence` and `sound`. Before the host has connected, it answers
`actions`, `body`, `body-markup`, `icon-static`, `persistence`.

Notifications sent while the host isn't connected, e.g. during boot or right
after D-Bus activation, wait in the agent (up to 32) and are shown when it
connects.

## Protocol

Newline-delimited JSON on vsock port 8892, in both directions. The agent
only accepts connections from CID 2, the host. Each side starts with a
hello; after that, messages go either way at any time.

| From  | Message | |
|-------|---------|-|
| agent | `{"type":"hello","vm":"debian"}` | the VM's name, its hostname |
| agent | `{"type":"notify","id":1,"app_name":"...","summary":"...",...}` | show, or replace, notification 1 |
| agent | `{"type":"close","id":1}` | the application closed it |
| host  | `{"type":"hello","capabilities":["actions","body",...]}` | what the host's server can do |
| host  | `{"type":"action","id":1,"key":"default"}` | the user picked an action |
| host  | `{"type":"closed","id":1,"reason":2}` | it's gone; the spec's reasons |

IDs are always the guest's; the host keeps its server's IDs to itself. A
`notify` looks like this (all but `id`, `app_name` and `summary` are
optional; `data` is base64):

```json
{"type": "notify", "id": 1, "app_name": "Firefox", "app_id": "firefox",
 "summary": "Download complete", "body": "<b>file.zip</b>",
 "actions": [{"key": "default", "label": "Open"}],
 "hints": {"urgency": 1, "category": "transfer.complete"},
 "expire_timeout": -1,
 "app_icon": {"format": "png", "data": "iVBORw0..."},
 "image": {"kind": "pixels", "width": 48, "height": 48, "rowstride": 192,
           "has_alpha": true, "bits_per_sample": 8, "channels": 4, "data": "..."}}
```

`image` can also be `{"kind": "file", "format": "svg", "data": "..."}`.
Lines are limited to 8 MiB, icons and pixel data to 4 MiB, pixel data to
1024×1024.

## Security

Everything from the guest is untrusted. The host checks the VM name and the
application ID like carte does before using them in file names, and:

- cuts the application name, summary, body, actions and hints down to
  fixed lengths, and keeps at most 16 actions;
- drops `<a>` and `<img>` tags from the body (keeping a link's text): links
  would open on the host, and image paths would be host paths. The rest of
  the markup is left to the host's server;
- accepts icons only as PNG or SVG, and pixel data only as 8-bit RGB or RGBA
  of a size that matches its length;
- writes icons only under `$XDG_RUNTIME_DIR/sonnette/<vm>/`, named after
  the numeric ID;
- lets a VM show 20 new notifications at once and 2 per second after that.
  Replacing a notification doesn't count, so progress updates aren't
  limited. Notifications over the limit are closed with reason 4.

The application name always has the VM's name in it, so a guest can't pass
its notifications off as the host's.

## Not done

- the `ActivationToken` signal: the host compositor's tokens mean nothing
  to the guest's
- `body-hyperlinks` and `body-images` (see above), and animated icons
- `sound-file`
- stripping markup when the host's server has no `body-markup`

## Code

| File                       | Contents |
|----------------------------|----------|
| `src/proto.rs`             | messages, limits and the host's checks |
| `src/vsock.rs`             | AF_VSOCK on the tokio reactor |
| `src/markup.rs`            | dropping links and images from the body |
| `src/base64.rs`            | base64 for the image data |
| `src/bin/sonnette-agent.rs`| the guest agent |
| `src/bin/sonnette.rs`      | the host relay |

`cargo test` runs the unit tests.

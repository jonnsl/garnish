# maitre

Runs crosvm guests for you. `maitred` is a daemon on your session bus, without
root, that starts and stops VMs when asked to over D-Bus. It gives each one a
vsock CID and a tap device with its own address from a shared pool, starts
crosvm and the programs that go with it, and takes everything back when the
VM stops. `maitre` is the command line client.

It does roughly what concierge does on ChromeOS, for the images in this
repository.

## Install

```sh
cargo install --locked --path .        # maitred and maitre to ~/.cargo/bin
install -Dm644 data/maitred.service ~/.config/systemd/user/maitred.service
install -Dm644 data/org.garnish.Maitre.service ~/.local/share/dbus-1/services/org.garnish.Maitre.service
systemctl --user daemon-reload
# dbus-broker only reads service files when it starts or reloads
busctl --user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus ReloadConfig
```

On Arch, `makepkg -si` in `host/maitre` installs the same files to `/usr`
instead; then reload the bus as above.

Nothing has to be started: the first call over D-Bus starts `maitred.service`
(D-Bus activation). It logs to the journal (`journalctl --user -u maitred`).
You can also run `maitred` in a terminal instead; it then logs to stderr.

The unit puts `~/.cargo/bin` and `~/.local/bin` in `PATH`, where it looks for
`crosvm`, `vhost-device-sound` and `carte`. If yours are elsewhere, change
`Environment=PATH=` in the unit.

Requires Rust 1.88 or newer. Dependencies: `zbus`, `tokio`, `serde`,
`futures-util`, `libc`.

## Usage

```sh
mkdir -p ~/.config/garnish/vms
cp examples/debian.conf ~/.config/garnish/vms/   # and set Image=
../../setup-host-net.sh up --nat                 # the tap pool, once per boot

maitre start debian          # prints the CID and the address
maitre list
maitre ssh debian            # ssh <user>@vsock/<cid>
maitre stop debian           # shut down, and wait until it's off
maitre stop --force debian   # stop crosvm without waiting for the guest
```

There is no serial console to type into. The guest's console output goes to
`console.log` in the VM's log directory (see [Logs](#logs)).

## VM configuration

One file per VM, `~/.config/garnish/vms/<name>.conf`. The name may contain
letters, digits, `_`, `.` and `-`, at most 32 characters. The format is the
one systemd and mkosi use:

- `[Section]` headers and `Key=Value` settings; `#` and `;` start comments
- a value continues on the next line if that line is indented, or if the line
  ends with `\`
- assigning a setting again replaces it, except for lists (marked below),
  which grow; an empty assignment (`Key=`) resets a setting to its default
- drop-ins in `<name>.conf.d/*.conf` are read after the file, in name order
- relative paths are relative to the file they are in, and `~/` is your home
- list items are separated by spaces; quote them (`'...'` or `"..."`) or use
  `\` to put a space in one
- unknown sections and settings are errors

`examples/debian.conf` has them all.

| Setting | Default | Meaning |
|---|---|---|
| `[Boot] Image=` | (required) | disk image |
| `[Boot] Kernel=` | `Image` with `.vmlinuz` | kernel |
| `[Boot] Initrd=` | `Image` with `.initrd`, if it exists | initrd |
| `[Boot] KernelCommandLine=` | see below | replaces the default command line |
| `[Boot] KernelCommandLineExtra=` | | list, added to the command line |
| `[Machine] CPUs=` | `4` | |
| `[Machine] Memory=` | `4G` | with a `K`, `M`, `G` or `T` suffix; a plain number is bytes |
| `[Machine] Network=` | `yes` | a tap and an address from the pool |
| `[Machine] SharedHome=` | | host directory mounted over the guest user's home |
| `[Machine] Audio=` | `no` | virtio-snd through vhost-device-sound |
| `[Machine] Microphone=` | `no` | lets the guest record from your default input |
| `[Machine] Wayland=` | `yes` | GPU with `GpuContextTypes` and the host's Wayland socket, for sommelier |
| `[Machine] GpuContextTypes=` | `cross-domain` | list; add `virgl2` only if crosvm has virglrenderer |
| `[Machine] ExtraArguments=` | | list, passed to `crosvm run` |
| `[Guest] User=` | your user name | the image's `GUEST_USER`: home mount and `maitre ssh` |
| `[Guest] UID=`, `GID=` | `1000`, the UID | the guest user's IDs, for the shared home |
| `[Guest] AppMenu=` | `yes` | run `carte sync` while the VM is up |

The default kernel command line is the image's (`KernelCommandLine=` in
`mkosi/mkosi.conf`): `root=PARTLABEL=root rw console=ttyS0
systemd.firstboot=off video=Virtual-1:d`. maitred adds to it:

- with `Network=yes`, `ip=<guest>::<host>:255.255.255.252::eth0:off:<dns>`
  and `net.ifnames=0`: the guest's address, gateway and DNS servers
- with `SharedHome=`, `systemd.mount-extra=` to mount it over
  `/home/<User>` (`nofail`: if the mount fails, the guest boots with the home
  from the image)

The configuration is read on every start, so a change applies the next time
the VM starts. maitred checks that the image, kernel, initrd and shared home
exist before it starts anything.

## The pool

`~/.config/garnish/maitred.conf` sets what maitred hands out. It is optional
and read once, at startup. `examples/maitred.conf` has the defaults:

| Setting | Default | Meaning |
|---|---|---|
| `[Pool] CIDs=` | `3-99` | vsock context IDs |
| `[Pool] Subnet=` | `192.168.10.0/24` | split into /30 networks, one per VM |
| `[Pool] TapPrefix=` | `crosvm_tap` | tap of the n-th network: `<TapPrefix><n>` |
| `[Pool] DNS=` | `1.1.1.1 9.9.9.9` | list, at most two |
| `[Manager] StopTimeoutSec=` | `30` | how long a guest gets to shut down |

A VM gets the lowest free CID and the lowest free network. In network `n` the
host is `.4n+1` on tap `<TapPrefix><n>` and the guest is `.4n+2`: 192.168.10.2
for the first VM, 192.168.10.6 for the second, and so on. They are given back
when the VM stops, so a VM's CID and address can change between runs; `maitre
list` shows the current ones. A /30 per VM means guests can't reach each
other, only the host and, with NAT, the outside.

Only root can create taps. `setup-host-net.sh up` creates `TAPS` of them
(default 4), owned by you, with the host addresses already on them, and
maitred uses those that exist: with 4 taps, 4 VMs can have a network at the
same time. A VM with `Network=no` doesn't need one.

## Starting and stopping

To start a VM, maitred

1. reads and checks its configuration,
2. takes a CID and, with `Network=yes`, a tap,
3. starts `vhost-device-sound` (with `Audio=yes`) and `carte sync --cid <cid>`
   (with `AppMenu=yes`, if `carte` is installed),
4. starts `crosvm run` and waits for its control socket.

`StartVm` returns after step 4. If crosvm exits before that, the call fails
with the end of crosvm's log.

When the guest reboots, crosvm exits with status 32 (`--extended-status`) and
maitred starts it again with the same resources. A guest that reboots more
than 5 times in 60 seconds is stopped instead.

`StopVm` presses the ACPI power button (`crosvm powerbtn`), so systemd in the
guest shuts down cleanly. If the guest is still running after
`StopTimeoutSec=`, maitred runs `crosvm stop`, and kills crosvm 5 seconds
later if it is still there. `KillVm` (`maitre stop --force`) begins with
`crosvm stop`. When crosvm exits, maitred stops the helpers, gives the
resources back and sends `VmStopped`.

On SIGTERM or SIGINT (`systemctl --user stop maitred`) maitred stops all VMs
the same way and exits. A second signal skips the power button. crosvm and
the helpers get SIGTERM if maitred dies.

## Logs

Each VM has a directory, `~/.local/state/garnish/<name>/`, with:

| File | Contents |
|---|---|
| `console.log` | the guest's serial console |
| `crosvm.log` | crosvm's own messages |
| `vhost-device-sound.log` | with `Audio=yes` |
| `carte.log` | with `AppMenu=yes` |

They are emptied on every start. After a guest reboot crosvm writes to the
end of them. Sockets are in `$XDG_RUNTIME_DIR/garnish/<name>/`.

## D-Bus API

Bus name `org.garnish.Maitre` on the session bus, object
`/org/garnish/Maitre`, interface `org.garnish.Maitre1`:

| Member | Signature | |
|---|---|---|
| `StartVm(s name) → VmInfo` | `s` → `(ssusssus)` | starts the VM, returns once crosvm runs |
| `StopVm(s name)` | `s` | power button, then `crosvm stop`; returns right away |
| `KillVm(s name)` | `s` | `crosvm stop`; returns right away |
| `ListVms() → a(VmInfo)` | → `a(ssusssus)` | configured VMs, running or not |
| signal `VmStarted(VmInfo)` | `(ssusssus)` | |
| signal `VmStopped(s name, s reason)` | `ss` | `reason` is for people, e.g. "the guest shut down" |

`VmInfo` is `(name, state, cid, tap, address, user, pid, log_dir)`. `state` is
`stopped`, `starting`, `running` or `stopping`; for a stopped VM the other
fields are empty or 0.

Errors are `org.garnish.Maitre1.Error.<name>` with a message: `NoSuchVm`,
`InvalidConfig`, `AlreadyRunning`, `NotRunning`, `NoResources` (out of CIDs or
taps), `StartFailed`, `ShuttingDown`.

```sh
busctl --user call org.garnish.Maitre /org/garnish/Maitre org.garnish.Maitre1 StartVm s debian
busctl --user call org.garnish.Maitre /org/garnish/Maitre org.garnish.Maitre1 ListVms
```

Anything running as your user can call these, just as it could run crosvm.

## Not done

- an interactive serial console
- fixed CIDs or addresses for a VM
- more than two DNS servers
- noticing a CID taken by a crosvm that maitred didn't start (the start then
  fails with crosvm's "cid in use" error)

## Code

| File | Contents |
|---|---|
| `src/ini.rs` | the configuration file format |
| `src/config.rs` | VM and daemon settings |
| `src/pool.rs` | CIDs and taps |
| `src/vm.rs` | crosvm's command line, kernel command line, starting processes |
| `src/manager.rs` | the daemon's state, starting, supervising and stopping VMs, the D-Bus object |
| `src/dbus.rs` | names, `VmInfo`, errors and the client proxy |
| `src/bin/maitred.rs` | the daemon |
| `src/bin/maitre.rs` | the client |
| `data/` | systemd user unit and D-Bus service file |
| `examples/` | a VM configuration and the pool defaults |

`cargo test` runs the unit tests.

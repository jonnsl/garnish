# maitre

Runs crosvm guests for you. `maitred` is a daemon on your session bus, without
root, that starts and stops VMs when asked to over D-Bus. It gives each one a
vsock CID and a network, starts crosvm and the programs that go with it, and
takes everything back when the VM stops. `maitre` is the command line
client. `maitre-net` is the small setuid-root helper for VMs with NAT
networking.

It does roughly what concierge does on ChromeOS, for the images in this
repository.

## Install

```sh
cargo install --locked --path .        # maitred, maitre and maitre-net to ~/.cargo/bin
# maitre-net has to be setuid root, so it can't stay in ~/.cargo/bin
sudo install -o root -g root -m 4755 ~/.cargo/bin/maitre-net /usr/local/bin/
rm ~/.cargo/bin/maitre-net
install -Dm644 data/maitred.service ~/.config/systemd/user/maitred.service
install -Dm644 data/org.garnish.Maitre.service ~/.local/share/dbus-1/services/org.garnish.Maitre.service
systemctl --user daemon-reload
# dbus-broker only reads service files when it starts or reloads
busctl --user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus ReloadConfig
```

On Arch, `makepkg -si` in `host/garnish` installs the same files to `/usr`
instead, with the shell completions and `10-garnish.network` for
systemd-networkd, as the `garnish-host` package (with carte) and the `salle`
package; then reload the bus as above.

Shell completions for `maitre` complete subcommands and VM names (from the
configuration files) and, after `maitre ssh <name>`, a command. Without the
package:

```sh
install -Dm644 completions/maitre.bash ~/.local/share/bash-completion/completions/maitre
# zsh: a directory in $fpath, added before compinit in ~/.zshrc, e.g.
# fpath=(~/.local/share/zsh/site-functions $fpath)
install -Dm644 completions/_maitre ~/.local/share/zsh/site-functions/_maitre
```

Nothing has to be started: the first call over D-Bus starts `maitred.service`
(D-Bus activation). It logs to the journal (`journalctl --user -u maitred`).
You can also run `maitred` in a terminal instead; it then logs to stderr.

The unit puts `~/.cargo/bin` and `~/.local/bin` in `PATH`, where it looks for
`crosvm`, `vhost-device-sound`, `carte` and `sonnette`. If yours are elsewhere, change
`Environment=PATH=` in the unit. `passt` and `maitre-net` are looked up in
`PATH` too; a copy of maitre-net that isn't setuid root is skipped.

Requires Rust 1.88 or newer. Dependencies: `zbus`, `tokio`, `serde`,
`futures-util`, `libc`.

## Usage

```sh
mkdir -p ~/.config/garnish/vms
cp examples/debian.conf ~/.config/garnish/vms/   # and set Image=

maitre start debian          # prints the CID and the address
maitre list
maitre ssh debian            # ssh <user>@vsock/<cid>
maitre stop debian           # shut down, and wait until it's off
maitre stop --force debian   # stop crosvm without waiting for the guest
```

There is no serial console to type into. The guest's console output goes to
`console.log` in the VM's log directory (see [Logs](#logs)).

## VM configuration

One file per VM, `~/.config/garnish/vms/<name>.conf`. The name is also the
guest's hostname, so it may contain letters, digits and `-` (not at the start
or end), at most 32 characters. The format is the
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
| `[Machine] Title=` | | a name for people, e.g. `Debian (work)`; Salle shows it instead of the VM's name |
| `[Machine] CPUs=` | `4` | |
| `[Machine] Memory=` | `4G` | with a `K`, `M`, `G` or `T` suffix; a plain number is bytes |
| `[Machine] SharedHome=` | | host directory mounted over the guest user's home |
| `[Machine] Audio=` | `no` | virtio-snd through vhost-device-sound |
| `[Machine] Microphone=` | `no` | lets the guest record from your default input |
| `[Machine] Wayland=` | `yes` | GPU with `GpuContextTypes` and the host's Wayland socket, for sommelier |
| `[Machine] GpuContextTypes=` | `cross-domain` | list; add `virgl2` only if crosvm has virglrenderer |
| `[Machine] ExtraArguments=` | | list, passed to `crosvm run` |
| `[Network] Mode=` | `passt` | `passt`: through passt, as you; `nat`: a network device of its own on the host; `none`: no network. See [Network](#network) |
| `[Guest] User=` | your user name | the image's `GUEST_USER`: home mount and `maitre ssh` |
| `[Guest] UID=`, `GID=` | `1000`, the UID | the guest user's IDs, for the shared home |
| `[Guest] AppMenu=` | `yes` | run `carte sync` while the VM is up; with `Wayland=yes` and `AppMenu=no`, `carte sync --no-menu`, which only keeps the list of applications (for Salle) |
| `[Guest] Notifications=` | `yes` | run `sonnette relay` while the VM is up: the guest's notifications show on the host |

The default kernel command line is the image's (`KernelCommandLine=` in
`mkosi/mkosi.conf`): `root=PARTLABEL=root rw console=ttyS0
systemd.firstboot=off video=Virtual-1:d`. maitred adds to it:

- `systemd.hostname=<name>`: the guest is named after its configuration file.
  sommelier, carte-agent and sonnette-agent use the hostname as the VM's name
- with a network, `ip=<guest>::<gateway>:<netmask>::eth0:off:<dns>` and
  `net.ifnames=0`: the guest's address, gateway and DNS servers
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
| `[Pool] DNS=` | `1.1.1.1 9.9.9.9` | list, at most two |
| `[Manager] StopTimeoutSec=` | `30` | how long a guest gets to shut down |

A VM gets the lowest free CID. It is given back when the VM stops, so a VM's
CID can change between runs; `maitre list` shows the current one.

## Network

`[Network] Mode=` picks one of three:

| Mode | The guest | Needs |
|---|---|---|
| `passt` (default) | reaches the outside and the host's other addresses; the host can't reach it over the network | `passt` |
| `nat` | has an address the host can reach, and reaches the outside through NAT | `maitre-net`, setuid root |
| `none` | has no network device | |

In every mode the host reaches the guest over vsock: `maitre ssh`, the
application menu and Wayland don't need a network.

### passt

With `Mode=passt`, maitred starts [passt](https://passt.top) as you, next to
crosvm, and crosvm connects to it as a vhost-user network device
(`--vhost-user type=net,socket=passt.sock`). passt has no network device on
the host: it turns the guest's connections into ordinary sockets of its own,
as if the guest's programs ran on the host as you. Nothing needs root or
setup, and VMs can't reach each other, as each has a passt of its own.

Every guest is 10.0.2.15/24 with gateway 10.0.2.2. These addresses only exist
between the guest and its passt. passt runs with `--no-map-gw`: by default,
connections to the gateway address would go to the host's loopback, where
services that trust local users listen. The guest can still reach the host's
other addresses, like any program on the LAN. No ports are forwarded to the
guest. passt stays up across guest reboots and stops with the VM; it logs
to `passt.log`.

### NAT

With `[Network] Mode=nat`, maitred runs `maitre-net nat` before it starts the
VM. The helper creates tap `garnish<n>` for the lowest free /30 of the
subnet in `/etc/garnish/network.conf` (default 192.168.10.0/24), puts the
host's address on it, makes sure IPv4 forwarding and the NAT rules are in
place, and passes the open tap back over a Unix socket. crosvm gets it as
`--net tap-fd=N`. In network `n` the host is `.4n+1` and the guest is
`.4n+2`: 192.168.10.2 for the first VM, 192.168.10.6 for the second, and so
on, across all users' VMs. A /30 per VM and a firewall rule keep guests from
reaching each other; they reach the outside through the host's address.

The tap lives as long as its descriptors: maitred keeps one for crosvm's
restarts after a guest reboot, and when the VM stops, the kernel removes the
tap. So a VM's address can change between runs.

maitre-net is setuid root but keeps only `CAP_NET_ADMIN`, as your user, and
seccomp-confines itself before it does anything; its caller can't choose the
tap's name or addresses. [docs/host-setup.md](../../docs/host-setup.md#network)
has the details, the firewall rules and how to change the subnet.

## Starting and stopping

To start a VM, maitred

1. reads and checks its configuration,
2. with `Mode=nat`, gets a tap from `maitre-net`, and takes a CID,
3. starts `passt` (with `Mode=passt`), `vhost-device-sound` (with
   `Audio=yes`), `carte sync --cid <cid>` (with `AppMenu=yes` or
   `Wayland=yes`, adding `--no-menu` without `AppMenu`, if `carte` is
   installed) and `sonnette relay --cid <cid>` (with `Notifications=yes`,
   if `sonnette` is installed),
4. starts `crosvm run` and waits for its control socket.

vhost-device-sound gets `PIPEWIRE_PROPS` naming its playback stream after
the VM: `node.name=garnish.<name>`, `application.name` the title (or the
name) and `application.id=org.garnish.vm.<name>`. The host's sound settings
then show each VM by name, WirePlumber remembers each one's volume, and Salle
finds the stream to set its volume.

`StartVm` returns after step 4, with the VM `booting`. If crosvm exits
before that, the call fails with the end of crosvm's log.

The VM is `running` once the guest says it has booted: `maitre-ready.service`
in the image (see `mkosi/`) runs `systemd-notify --ready` after
`multi-user.target`, with `NOTIFY_SOCKET=vsock-stream:2:8891`. maitred listens
on vsock port 8891 and finds the VM by the guest's CID. Only messages from
root in the guest count (systemd-notify sends from port 1023 as root). An image
without the service shows as `running` after 120 seconds. vsock ports are
shared by all users of the host: if maitred can't listen on 8891 (another
user's maitred has it), it says so at startup and VMs are `running` as soon as
crosvm runs.

When the guest reboots, crosvm exits with status 32 (`--extended-status`) and
maitred starts it again with the same resources; the VM is `booting` again. A guest that reboots more
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
| `passt.log` | with `Mode=passt` |
| `vhost-device-sound.log` | with `Audio=yes` |
| `carte.log` | with `AppMenu=yes` or `Wayland=yes` |
| `sonnette.log` | with `Notifications=yes` |

They are emptied on every start. After a guest reboot crosvm writes to the
end of them. Sockets are in `$XDG_RUNTIME_DIR/garnish/<name>/`.

## D-Bus API

Bus name `org.garnish.Maitre` on the session bus, object
`/org/garnish/Maitre`, interface `org.garnish.Maitre1`:

| Member | Signature | |
|---|---|---|
| `StartVm(s name) → VmInfo` | `s` → `(ssusssus)` | starts the VM, returns once crosvm runs (`booting`) |
| `StopVm(s name)` | `s` | power button, then `crosvm stop`; returns right away |
| `KillVm(s name)` | `s` | `crosvm stop`; returns right away |
| `ListVms() → a(VmInfo)` | → `a(ssusssus)` | configured VMs, running or not |
| signal `VmStarted(VmInfo)` | `(ssusssus)` | |
| signal `VmStopped(s name, s reason)` | `ss` | `reason` is for people, e.g. "the guest shut down" |
| signal `VmStateChanged(VmInfo)` | `(ssusssus)` | the state changed; also the new `pid` after a guest reboot |

`VmInfo` is `(name, state, cid, tap, address, user, pid, log_dir)`. `state` is
`stopped`, `starting`, `booting`, `running` or `stopping`; for a stopped VM the
other fields are empty or 0.

`VmStateChanged` follows a VM through every state: `starting` when a start
begins, `booting` once crosvm runs, `running` once the guest says it has
booted (see [Starting and stopping](#starting-and-stopping)), `stopping`
after `StopVm` or `KillVm`
(or a shutdown of maitred), and `stopped` when it's gone, also after a start
that failed. It comes before `VmStarted` and `VmStopped`, which are still
sent.

Errors are `org.garnish.Maitre1.Error.<name>` with a message: `NoSuchVm`,
`InvalidConfig`, `AlreadyRunning`, `NotRunning`, `NoResources` (out of CIDs),
`StartFailed` (also when there's no network device), `ShuttingDown`.

```sh
busctl --user call org.garnish.Maitre /org/garnish/Maitre org.garnish.Maitre1 StartVm s debian
busctl --user call org.garnish.Maitre /org/garnish/Maitre org.garnish.Maitre1 ListVms
```

Anything running as your user can call these, just as it could run crosvm.

## Not done

- an interactive serial console
- fixed CIDs or addresses for a VM
- more than two DNS servers
- a bridge to the host's network, and port forwarding to passt guests
- IPv6 settings for the guests: maitred gives them only IPv4 addresses
- a limit on how many taps one user can hold: anyone can run maitre-net, and
  each tap takes one of the subnet's /30s until it is closed
- noticing a CID taken by a crosvm that maitred didn't start (the start then
  fails with crosvm's "cid in use" error)

## Code

| File | Contents |
|---|---|
| `src/ini.rs` | the configuration file format |
| `src/config.rs` | VM and daemon settings |
| `src/pool.rs` | CIDs |
| `src/net.rs` | the network settings, and how maitred and maitre-net talk |
| `src/vm.rs` | crosvm's command line, kernel command line, starting processes |
| `src/manager.rs` | the daemon's state, starting, supervising and stopping VMs, the D-Bus object |
| `src/ready.rs` | the vsock listener for the guests' boot notifications |
| `src/dbus.rs` | names, `VmInfo`, errors and the client proxy |
| `src/bin/maitred.rs` | the daemon |
| `src/bin/maitre.rs` | the client |
| `src/bin/maitre-net/` | the setuid helper: dropping privileges, the seccomp filter, netlink and nftables |
| `data/` | systemd user unit, D-Bus service file, networkd file for the taps |
| `completions/` | bash and zsh completions for `maitre` |
| `examples/` | a VM configuration, the pool defaults and `/etc/garnish/network.conf` |

`cargo test` runs the unit tests.

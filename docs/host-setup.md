# Host setup

Details on the host side that the component READMEs don't cover: crosvm, the
network, the shared home directory and audio. For installing gerantd and
writing VM files, see the [gerant README](../vm_tools/gerant/README.md); for
the application menu, the [carte README](../vm_tools/carte/README.md).

## crosvm

On x86_64, crosvm's GPU sandbox kills the device when a guest app pastes
something copied on the host, and the VM stops with status 33: the clipboard
code calls `pipe`, which the GPU's seccomp policy doesn't allow.
`host/crosvm` builds the AUR's `crosvm-git` with that one call allowed; it
replaces `crosvm-git`. `host/crosvm/upstream-bug.md` has the details.

```sh
(cd host/crosvm && makepkg -si)                # Arch package, patched
```

## Network

Each VM has one setting, `[Network] Mode=` in its file:

- `passt` (the default): internet access through
  [passt](https://passt.top), which gerantd runs as you. Nothing to set up
- `nat`: a network device of its own on the host, with an address the host
  can reach, and internet access through the host's (below)
- `none`: no network device

The [gerant README](../vm_tools/gerant/README.md#network) compares them and
describes passt mode. The rest of this section is about `nat`.

### NAT

Only root can create network devices, so gerantd runs `gerant-net`, a small
helper installed setuid root next to it, every time a VM with `Mode=nat`
starts. The helper

1. creates tap `garnish<n>` for the lowest free /30 in the subnet: the host
   gets 192.168.10.(4n+1) on it and the guest 192.168.10.(4n+2),
2. turns IPv4 forwarding on (`net.ipv4.ip_forward=1`; it stays on),
3. loads the nftables table below, replacing any older version of it,
4. passes the open tap to gerantd over a socket and exits.

gerantd hands the tap to crosvm as `--net tap-fd=N`. The tap isn't
persistent: the kernel removes it when the VM stops and the last descriptor
is closed, even if gerantd crashes. Nothing has to be set up after a reboot.

```
table ip garnish {
    chain postrouting {
        type nat hook postrouting priority srcnat;
        ip saddr 192.168.10.0/24 oifname != "garnish*" masquerade
    }
    chain forward {
        type filter hook forward priority filter;
        iifname "garnish*" oifname "garnish*" drop
        iifname "garnish*" ip saddr != 192.168.10.0/24 drop
    }
}
```

Guests reach the outside through the host's address, but not each other,
and they can't send from addresses that aren't theirs. They can reach the
host itself, at their gateway address or any other. If the host firewall
drops forwarded traffic (a `forward` chain with `policy drop`, as firewalld
or ufw set up), the NAT rule alone isn't enough: that firewall also has to
accept forwarding from and to the `garnish*` interfaces. To remove the table:
`sudo nft delete table ip garnish`.

The subnet is the same for every user of the host, so it is set by root, in
`/etc/garnish/network.conf` (optional; `vm_tools/gerant/examples/network.conf`
has the default). Pick one that doesn't overlap a network the host is on:

```ini
[NAT]
Subnet=192.168.10.0/24
```

The file has to belong to root and not be writable by anyone else, or
gerant-net refuses to run. gerant-net has no other options: nothing the
caller passes picks the tap's name or addresses, and it can't remove or
change a tap.

#### The helper's privileges

gerant-net runs with root's privileges only long enough to give most of them
up. Before it reads its configuration or touches the network, it

- becomes the user who ran it, keeping only `CAP_NET_ADMIN`: no other
  capability, none in the bounding set, ambient or inheritable sets, and
  `no_new_privs`, so nothing it could run would get any back
- is marked not dumpable, so the user can't attach a debugger to it
- opens everything it needs: `/dev/net/tun`,
  `/proc/sys/net/ipv4/ip_forward`, a netlink socket for addresses and one for
  nftables
- installs a seccomp filter. After that it can't open files, create sockets,
  run programs or send signals. `ioctl` works only for creating the tap and
  looking up its index, `sendmsg`, `recv` and `write` only on the descriptors
  meant for them, and any other call kills it

Its environment is ignored, and it accepts exactly one argument, `nat`.
The code is in `vm_tools/gerant/src/bin/gerant-net/`.

#### systemd-networkd

A catch-all `.network` file (e.g. one matching `Type=ether`) would make
systemd-networkd take over the taps and replace their addresses. The package
installs `/usr/lib/systemd/network/10-garnish.network`, which tells networkd
to leave `garnish*` alone. NetworkManager leaves taps it didn't create alone
by default.

## Shared home directory

`SharedHome=<dir>` in the VM's `[Machine]` section mounts a host directory
over `/home/$GUEST_USER` with virtio-fs. crosvm's built-in device serves it,
running as your user, so you don't need virtiofsd or sudo. Nothing in the
image depends on the home directory: ssh keys, the sommelier unit and the
Wayland variables live in `/etc`. You can start with an empty directory.

The device runs in a user namespace that has only one uid and one gid. They
map the guest user (`[Guest] UID=`/`GID=`, default 1000) to your host user.
This means:

- files the guest user creates are owned by you on the host
- files with other owners on the host show up as `nobody` in the guest
- guest root can't create files there or `chown` them
- the guest user can't `chown` to other users either

If you changed `GUEST_USER` at build time, set the same `[Guest] User=`. If
the mount fails (`systemctl status home-<user>.mount`), the guest boots
anyway with the home from the image.

## Audio

`Audio=yes` adds a virtio-snd card. crosvm's own sound device has no PipeWire
backend, so gerantd starts
[vhost-device-sound](https://github.com/rust-vmm/vhost-device) as your user and
connects crosvm to it over vhost-user. It plays into your PipeWire and stops
when the VM stops. In the guest, PipeWire, WirePlumber and pipewire-pulse run
in the user session, so apps see an ordinary sound card.

The guest gets **no microphone** by default. `host/vhost-device-sound` builds
the package with a patch that adds `--no-capture`, and gerantd passes it
unless the VM has `Microphone=yes`. Then the guest can record from your
default input. This is enforced on the host, so guest root can't turn it back
on.

The package also patches bugs in vhost-device-sound 0.3.0 that stopped all
of a VM's sound until it was restarted: the PipeWire backend could deadlock
when the guest opened or closed its microphone during playback, and the
thread that serves the guest panicked on a buffer sent as a stream stopped,
which apps like Discord trigger often. It also fills capture buffers one
guest period at a time; they came in bursts as big as the host's quantum,
so small buffers overran. Rebuild with `makepkg -sfi` if you installed an
older build.

```sh
./check-host-audio.sh                          # host prerequisites (read-only)
(cd host/vhost-device-sound && makepkg -si)    # Arch package, patched
# Audio=yes in ~/.config/garnish/vms/debian.conf, then:
gerant start debian
gerant ssh debian speaker-test -c 2 -t wav -l 1
```

The host needs PipeWire with a session manager (WirePlumber) running.
`check-host-audio.sh` checks that, and that crosvm accepts the device.
The socket is `$XDG_RUNTIME_DIR/garnish/<name>/snd.sock`.

Each VM's sound is its own stream on the host, named after the VM, so its
volume is set from the host: the slider on the VM's page in Salle, or your
desktop's sound settings, where it shows by its title. WirePlumber remembers
it per VM. The host can only turn the guest's sound down, so the guest's own
output should be at full volume. The `garnish-guest` package starts it
there, but a guest that saved another volume before keeps it. Fix that once
with `gerant ssh debian wpctl set-volume @DEFAULT_AUDIO_SINK@ 1.0`.

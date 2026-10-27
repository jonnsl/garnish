# Host setup

Details on the host side that the component READMEs don't cover: the tap
network, the shared home directory and audio. For installing maitred and
writing VM files, see the [maitre README](../vm_tools/maitre/README.md); for
the application menu, the [carte README](../vm_tools/carte/README.md).

## Network

Only root can create taps, so `setup-host-net.sh` does it once per boot, for
each of the `TAPS` taps (4 by default; `TAPS=8 ./setup-host-net.sh up` for
more VMs at once). The taps belong to your user, so crosvm doesn't need root.
Tap `n` gets its own /30: the host is 192.168.10.(4n+1) and the guest
192.168.10.(4n+2).

```sh
./setup-host-net.sh up --nat    # crosvm_tap0-3; --nat adds internet access
./setup-host-net.sh down        # removes the taps and the NAT rule
```

It runs these commands through sudo:

```sh
sudo ip tuntap add mode tap user "$USER" vnet_hdr crosvm_tap0
sudo ip addr add 192.168.10.1/30 dev crosvm_tap0
sudo ip link set crosvm_tap0 up
# --nat:
sudo sysctl -w net.ipv4.ip_forward=1
sudo iptables -t nat -A POSTROUTING -s 192.168.10.0/24 -j MASQUERADE
```

`down` does not turn `ip_forward` off again. If the host firewall drops
forwarded traffic, the NAT rule alone isn't enough: you also need to allow
forwarding for 192.168.10.0/24.

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
backend, so maitred starts
[vhost-device-sound](https://github.com/rust-vmm/vhost-device) as your user and
connects crosvm to it over vhost-user. It plays into your PipeWire and stops
when the VM stops. In the guest, PipeWire, WirePlumber and pipewire-pulse run
in the user session, so apps see an ordinary sound card.

The guest gets **no microphone** by default. `host/vhost-device-sound` builds
the package with a patch that adds `--no-capture`, and maitred passes it
unless the VM has `Microphone=yes`. Then the guest can record from your
default input. This is enforced on the host, so guest root can't turn it back
on.

```sh
./check-host-audio.sh                          # host prerequisites (read-only)
(cd host/vhost-device-sound && makepkg -si)    # Arch package, patched
# Audio=yes in ~/.config/garnish/vms/debian.conf, then:
maitre start debian
maitre ssh debian speaker-test -c 2 -t wav -l 1
```

The host needs PipeWire with a session manager (WirePlumber) running.
`check-host-audio.sh` checks that, and that crosvm accepts the device.
The socket is `$XDG_RUNTIME_DIR/garnish/<name>/snd.sock`.

# mkosi images for crosvm

Builds Arch Linux, Debian (trixie), Ubuntu (26.04 "resolute") and Fedora (44)
disk images for crosvm. Every image comes with:

- sshd on AF_VSOCK port 22 (`sshd-vsock.socket`); public-key login only, root login disabled
- sommelier built from ChromiumOS platform2, installed to `/usr/local/bin/sommelier`
- a user (`jonnathan` by default) in `video`, `render` and `sudo`/`wheel`
- passwordless sudo, which you can turn off
- `~/.config/systemd/user/sommelier.service`, enabled, with linger turned on
- the Wayland environment variables in `~/.profile` (Wayland only, no X11)
- `video=Virtual-1:d` on the kernel command line
- a static address (192.168.10.2/24) on a host tap device, using systemd-networkd and systemd-resolved

## Non-goals

These are out of scope on purpose. Don't add support for them:

- **Secure Boot.** No signing keys, shim, signed UKIs or `SecureBoot=`.
- **LUKS / disk encryption.** The root partition is plain ext4.
- **Containers.** The images are meant for crosvm only. Running them with
  `systemd-nspawn`, `mkosi boot` or similar is not supported.
- **X11 / Xwayland.** Wayland only.
- **Architectures other than x86-64.**

## Build

Requires mkosi v26 or newer. Tested with mkosi v27.1 on an Arch Linux host
(rootless builds). All four images build and work under crosvm: ssh over
vsock and the tap network, the distro package manager, and GTK apps through
sommelier.

```sh
# required for ssh: password and root logins are disabled
cp ~/.ssh/id_ed25519.pub authorized_keys

mkosi -d arch   build
mkosi -d debian build
mkosi -d ubuntu build
mkosi -d fedora build
```

Output in `mkosi.output/`: `<name>.raw`, `<name>.vmlinuz`, `<name>.initrd`
where `<name>` is `archlinux`, `debian`, `ubuntu` or `fedora`.

Add `-f` to rebuild an existing image (`-ff` also drops the cached image).

### Build options

These are set with `Environment=` in `mkosi.conf`. Override them per build with
`-E`, or permanently in a `mkosi.local.conf` (ignored by git):

| Variable            | Default          | Meaning                                          |
|---------------------|------------------|--------------------------------------------------|
| `PASSWORDLESS_SUDO` | `yes`            | `no` makes sudo ask for the password             |
| `GUEST_USER`        | `jonnathan`      | name of the user that gets created               |
| `GUEST_PASSWORD`    | `changeme`       | password for sudo and the serial console (not ssh) |
| `SOMMELIER_REF`     | `main`           | platform2 branch, tag or commit to build         |
| `SOMMELIER_REPO`    | platform2 URL    | git repository to clone sommelier from           |
| `GUEST_ADDRESS`     | `192.168.10.2/24`| guest address on the tap network                 |
| `GUEST_GATEWAY`     | `192.168.10.1`   | default gateway (the host end of the tap)        |
| `GUEST_DNS`         | `1.1.1.1,9.9.9.9`| comma-separated DNS servers                      |

```sh
mkosi -d debian -E PASSWORDLESS_SUDO=no -E GUEST_PASSWORD=hunter2 build
```

```ini
# mkosi.local.conf
[Build]
Environment=PASSWORDLESS_SUDO=no GUEST_PASSWORD=hunter2
```

To keep the platform2 checkout and meson build between builds, run `mkdir mkosi.builddir`.

## Run

```sh
./setup-host-net.sh up --nat    # tap crosvm_tap at 192.168.10.1; --nat adds internet access
./run-crosvm.sh debian          # CID=3, 4 CPUs, 4 GiB, tap crosvm_tap by default
CID=4 MEM=8192 ./run-crosvm.sh fedora
TAP= ./run-crosvm.sh ubuntu     # no network
GPU_CONTEXTS=cross-domain:virgl2 ./run-crosvm.sh debian  # only if crosvm has virglrenderer
ssh jonnathan@vsock/3           # host systemd >= 256; otherwise:
ssh -o ProxyCommand='socat - VSOCK-CONNECT:3:22' jonnathan@localhost
ssh jonnathan@192.168.10.2      # over the tap also works
./setup-host-net.sh down        # removes the tap and the NAT rule
```

`setup-host-net.sh` runs the commands below through sudo. The tap belongs to your
user, so crosvm doesn't need root.

```sh
sudo ip tuntap add mode tap user "$USER" vnet_hdr crosvm_tap
sudo ip addr add 192.168.10.1/24 dev crosvm_tap
sudo ip link set crosvm_tap up
# --nat:
sudo sysctl -w net.ipv4.ip_forward=1
sudo iptables -t nat -A POSTROUTING -s 192.168.10.0/24 -j MASQUERADE
```

`down` does not turn `ip_forward` off again. If the host firewall drops
forwarded traffic, the NAT rule alone isn't enough: you also need to allow
forwarding for 192.168.10.0/24.

crosvm boots `vmlinuz`/`initrd` directly, so the kernel command line comes from
`run-crosvm.sh`, not from `mkosi.conf`. If you change one, change the other.
The image also has an ESP with a UKI, so UEFI boot (`--bios OVMF.fd`) works too.
That path uses `KernelCommandLine=` from `mkosi.conf`.

The root filesystem is 16 GiB but the file is sparse. To make it bigger, run
`truncate -s +10G mkosi.output/debian.raw`. systemd-growfs then grows the
partition at the next boot.

## Layout

| Path                                   | Purpose                                              |
|----------------------------------------|------------------------------------------------------|
| `mkosi.conf`                           | shared settings, build options, kernel command line  |
| `mkosi.conf.d/10-<distro>.conf`        | release, packages and build packages for each distro |
| `mkosi.conf.d/20-debian-ubuntu-packages.conf` | packages Debian and Ubuntu have in common     |
| `mkosi.repart/`                        | ESP plus an ext4 root labelled `root`                |
| `mkosi.build.chroot`                   | clones and builds sommelier inside the target distro |
| `mkosi.postinst.chroot`                | sets up the user, sudo, ssh, network, sommelier unit, linger and `.profile` |
| `mkosi.finalize`                       | points `/etc/resolv.conf` at the systemd-resolved stub |
| `mkosi.extra/`                         | `sshd-vsock.socket`, `sshd-vsock@.service`, preset, sshd drop-in |
| `run-crosvm.sh`                        | boots an image with crosvm                           |
| `setup-host-net.sh`                    | creates/removes the host tap and the optional NAT    |

## Notes

- The sshd-vsock units have the same names as the ones `systemd-ssh-generator`
  makes, so these take precedence. They also work without the generator.
  Host keys are deleted at build time and created on the first connection,
  so two images never share host keys.
- In Arch and Fedora, `/etc/skel/.bash_profile` stops bash from reading `~/.profile`.
  The postinst script makes `~/.bash_profile` source `~/.profile`.
- Debian and Ubuntu need `dbus-user-session` to get a user D-Bus session.
  Fedora and Arch have one by default.
- `pam_systemd` sets `XDG_RUNTIME_DIR`. Without it, `user@.service` fails
  ("$XDG_RUNTIME_DIR is not set") and sommelier never starts. Fedora ships it
  as `systemd-pam`, and Debian and Ubuntu as `libpam-systemd`. On Arch it's
  part of `systemd`.
- These images are Wayland only: no Xwayland, and sommelier runs without `-X`.
  The xcb libraries are still installed because sommelier's `meson.build`
  needs them unconditionally.
- mkosi builds rootless, so the build can't make files owned by the guest
  user (chown fails with `Invalid argument`). `guest-home-owner.service` runs
  `chown -R` on the home directory once at first boot, before user sessions
  and lingering start. It then writes `/var/lib/guest-home-owner/done` so it
  doesn't run again.
- In the Arch image, the postinst script writes `/etc/pacman.d/mirrorlist`,
  because the packaged one has every server commented out.
  `pacman-init.service` creates the pacman keyring at first boot.
  Creating it at build time would give every image the same local signing key.
- The guest network config is written to
  `/etc/systemd/network/80-crosvm-tap.network`. It matches any Ethernet
  interface, which in practice is the virtio-net device.

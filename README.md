# garnish

Experimental crosvm guest images built with mkosi. This is an experiment, not
production-ready: expect breakage.

The name continues ChromeOS's food names: this is roughly Crostini (crosvm,
sommelier) on a regular Linux desktop.

Builds Arch Linux, Debian (trixie), Ubuntu (26.04 "resolute") and Fedora (44)
disk images for crosvm. Every image comes with:

- sshd on AF_VSOCK port 22 (`sshd-vsock.socket`); public-key login only, root login disabled
- sommelier built from ChromiumOS platform2, installed to `/usr/local/bin/sommelier`
- a user (`jonnathan` by default) in `video`, `render` and `sudo`/`wheel`
- passwordless sudo, which you can turn off
- `/etc/systemd/user/sommelier.service`, enabled, with linger turned on
- `carte-agent`, which puts the guest's applications in the host's application menu
- the Wayland environment variables in `/etc/profile.d/crosvm-wayland.sh` (Wayland only, no X11)
- optionally, a host directory as the user's home over virtio-fs (no root needed)
- PipeWire for an optional virtio-snd sound card that plays into the host's PipeWire
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

The mkosi configuration is in `mkosi/`. mkosi finds `mkosi/mkosi.conf` by
itself, so run it from the repository root:

```sh
# required for ssh: password and root logins are disabled
cp ~/.ssh/id_ed25519.pub mkosi/authorized_keys

mkosi -d arch   build
mkosi -d debian build
mkosi -d ubuntu build
mkosi -d fedora build
```

Output in `mkosi/mkosi.output/`: `<name>.raw`, `<name>.vmlinuz`, `<name>.initrd`
where `<name>` is `archlinux`, `debian`, `ubuntu` or `fedora`.

Add `-f` to rebuild an existing image (`-ff` also drops the cached image).

### Build options

These are set with `Environment=` in `mkosi/mkosi.conf`. Override them per build with
`-E`, or permanently in `mkosi/mkosi.local.conf` (ignored by git):

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
| `VM_NAME`           | the distribution | name in the host's app menu and window app_ids   |

```sh
mkosi -d debian -E PASSWORDLESS_SUDO=no -E GUEST_PASSWORD=hunter2 build
```

```ini
# mkosi/mkosi.local.conf
[Build]
Environment=PASSWORDLESS_SUDO=no GUEST_PASSWORD=hunter2
```

To keep the platform2 checkout and meson build between builds, run `mkdir mkosi/mkosi.builddir`.

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
SHARE_HOME=~/vm/home ./run-crosvm.sh debian  # host directory as the guest home
AUDIO=1 ./run-crosvm.sh debian  # sound, playback only (see Audio below)
CARTE=0 ./run-crosvm.sh debian    # don't put the guest's apps in the app menu
./setup-host-net.sh down        # removes the tap and the NAT rule
```

The terminal only shows the serial console. crosvm's own log goes to
`crosvm-<CID>.log` in the current directory, and with `AUDIO=1`
vhost-device-sound's goes to `vhost-device-sound-<CID>.log` next to it, and
`carte sync`'s to `carte-<CID>.log`. They are overwritten on every run. If crosvm exits with an error, the last lines of
its log are printed. Set `LOG_DIR=<dir>` to put them somewhere else, or
`LOG_DIR=` to keep them on the terminal.

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

### Shared home directory

`SHARE_HOME=<dir>` mounts a host directory over `/home/$GUEST_USER` with
virtio-fs. crosvm's built-in device serves it, running as your user, so you
don't need virtiofsd or sudo. Nothing in the image depends on the home
directory: ssh keys, the sommelier unit and the Wayland variables live in
`/etc`. You can start with an empty directory.

The device runs in a user namespace that has only one uid and one gid. They
map the guest user (`GUEST_UID`/`GUEST_GID`, default 1000) to your host user.
This means:

- files the guest user creates are owned by you on the host
- files with other owners on the host show up as `nobody` in the guest
- guest root can't create files there or `chown` them
- the guest user can't `chown` to other users either

If you changed `GUEST_USER` at build time, pass the same value to
`run-crosvm.sh`. If the mount fails (`systemctl status home-<user>.mount`),
the guest boots anyway with the home from the image.

### Application menu

[`vm_tools/carte`](vm_tools/carte/README.md) is a small Rust take on
ChromeOS's garcon. `carte-agent` runs in the guest's user session
(`carte-agent.service`) and sends the guest's applications and their icons to
the host over vsock. On the host, `carte sync` turns them into launchers in
`~/.local/share/applications`, which start the application in the guest.

```sh
cargo install --locked --path vm_tools/carte --bin carte  # once, to ~/.cargo/bin
./run-crosvm.sh debian      # runs `carte sync --cid 3` while the VM is up
carte remove debian         # removes debian's launchers and icons
```

The VM name is `VM_NAME`, by default the distribution (`arch`, `debian`,
`ubuntu`, `fedora`). It's appended to the application names, and it's also
sommelier's VM identifier, so GNOME can match windows to their launcher. The
applications start with the user manager's environment, which is why the
Wayland variables are also in `/etc/environment.d/50-crosvm-wayland.conf`.

### Audio

`AUDIO=1` adds a virtio-snd card. crosvm's own sound device has no PipeWire
backend, so `run-crosvm.sh` starts
[vhost-device-sound](https://github.com/rust-vmm/vhost-device) as your user and
connects crosvm to it over vhost-user. It plays into your PipeWire and stops
when crosvm exits. In the guest, PipeWire, WirePlumber and pipewire-pulse run
in the user session, so apps see an ordinary sound card.

The guest gets **no microphone** by default. `host/vhost-device-sound` builds
the package with a patch that adds `--no-capture`, and `run-crosvm.sh` passes
it unless you set `AUDIO_CAPTURE=1`. Then the guest can record from your default
input. This is enforced on the host, so guest root can't turn it back on.

```sh
./check-host-audio.sh                          # host prerequisites (read-only)
(cd host/vhost-device-sound && makepkg -si)    # Arch package, patched
AUDIO=1 ./run-crosvm.sh debian
ssh jonnathan@vsock/3 speaker-test -c 2 -t wav -l 1
AUDIO=1 AUDIO_CAPTURE=1 ./run-crosvm.sh debian # with microphone
```

The host needs PipeWire with a session manager (WirePlumber) running.
`check-host-audio.sh` checks that, and that crosvm accepts the device.
The socket is `$XDG_RUNTIME_DIR/crosvm-snd-$CID.sock`, so VMs with different
CIDs each get their own.

crosvm boots `vmlinuz`/`initrd` directly, so the kernel command line comes from
`run-crosvm.sh`, not from `mkosi/mkosi.conf`. If you change one, change the other.
The image also has an ESP with a UKI, so UEFI boot (`--bios OVMF.fd`) works too.
That path uses `KernelCommandLine=` from `mkosi/mkosi.conf`.

The root filesystem is 16 GiB but the file is sparse. To make it bigger, run
`truncate -s +10G mkosi/mkosi.output/debian.raw`. systemd-growfs then grows the
partition at the next boot.

## Layout

| Path                                   | Purpose                                              |
|----------------------------------------|------------------------------------------------------|
| `mkosi/mkosi.conf`                     | shared settings, build options, kernel command line  |
| `mkosi/mkosi.conf.d/10-<distro>.conf`  | release, packages and build packages for each distro |
| `mkosi/mkosi.conf.d/20-debian-ubuntu-packages.conf` | packages Debian and Ubuntu have in common |
| `mkosi/mkosi.repart/`                  | ESP plus an ext4 root labelled `root`                |
| `mkosi/mkosi.build.chroot`             | builds sommelier and carte-agent inside the target distro |
| `mkosi/mkosi.postinst.chroot`          | sets up the user, sudo, ssh, network, sommelier and carte-agent units, linger and Wayland variables |
| `mkosi/mkosi.finalize`                 | points `/etc/resolv.conf` at the systemd-resolved stub |
| `mkosi/mkosi.extra/`                   | `sshd-vsock.socket`, `sshd-vsock@.service`, preset, sshd drop-in |
| `mkosi/authorized_keys`                | your ssh public key (not in git)                     |
| `run-crosvm.sh`                        | boots an image with crosvm                           |
| `check-host-audio.sh`                  | checks the host prerequisites for `AUDIO=1`          |
| `host/vhost-device-sound/`             | Arch PKGBUILD for vhost-device-sound, plus the `--no-capture` patch |
| `setup-host-net.sh`                    | creates/removes the host tap and the optional NAT    |
| `vm_tools/carte/`                      | `carte-agent` (guest) and `carte` (host): guest applications in the host's app menu |

The build mounts the whole repository at `$SRCDIR` (`BuildSources=..`), so
the build scripts can reach `vm_tools/`.

## Notes

- The sshd-vsock units have the same names as the ones `systemd-ssh-generator`
  makes, so these take precedence. They also work without the generator.
  Host keys are deleted at build time and created on the first connection,
  so two images never share host keys.
- Debian and Ubuntu need `dbus-user-session` to get a user D-Bus session.
  Fedora and Arch have one by default.
- `pam_systemd` sets `XDG_RUNTIME_DIR`. Without it, `user@.service` fails
  ("$XDG_RUNTIME_DIR is not set") and sommelier never starts. Fedora ships it
  as `systemd-pam`, and Debian and Ubuntu as `libpam-systemd`. On Arch it's
  part of `systemd`.
- These images are Wayland only: no Xwayland, and sommelier runs without `-X`.
  The xcb libraries are still installed because sommelier's `meson.build`
  needs them unconditionally.
- Electron and Chromium apps such as VS Code choose X11 or Wayland from
  `XDG_SESSION_TYPE`. logind sets it to `tty` for ssh logins, so they try
  X11 and exit right away (`code` still returns 0). The profile script sets
  it to `wayland`. `ELECTRON_OZONE_PLATFORM_HINT` is ignored by current
  Electron.
- mkosi builds rootless, so the build can't make files owned by the guest
  user (chown fails with `Invalid argument`). `guest-home-owner.service` runs
  `chown -R` on the home directory once at first boot, before user sessions
  and lingering start. It then writes `/var/lib/guest-home-owner/done` so it
  doesn't run again. It's skipped while a shared home is mounted there.
- In the Arch image, the postinst script writes `/etc/pacman.d/mirrorlist`,
  because the packaged one has every server commented out.
  `pacman-init.service` creates the pacman keyring at first boot.
  Creating it at build time would give every image the same local signing key.
- The guest network config is written to
  `/etc/systemd/network/80-crosvm-tap.network`. It matches any Ethernet
  interface, which in practice is the virtio-net device.
- `KernelInitrdModules=` in `mkosi/mkosi.conf` limits the initrd to the modules
  needed to mount the root filesystem: `virtio_pci`, `virtio_blk`, `ext4` and
  `crc32c*`, plus their dependencies. Kernels older than 6.14 (Debian) load
  crc32c for ext4 as a soft dependency, which mkosi doesn't add by itself. Without it, mkosi puts every module in the initrd,
  120 to 400 MiB depending on the distribution. If the root partition isn't
  found at boot (the initrd drops into an emergency shell), a module is missing
  from this list.

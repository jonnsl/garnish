# Guest images

mkosi configuration for the garnish guest images: Arch Linux, Debian (trixie),
Ubuntu (26.04 "resolute") and Fedora (44), built for crosvm on x86-64.

Every image comes with:

- sshd on AF_VSOCK port 22 (`sshd-vsock.socket`); public-key login only, root login disabled
- sommelier built from ChromiumOS platform2, installed to `/usr/local/bin/sommelier`
- a user (`user` by default) in `video`, `render` and `sudo`/`wheel`
- passwordless sudo, which you can turn off
- `/etc/systemd/user/sommelier.service`, enabled, with linger turned on
- `carte-agent`, which puts the guest's applications in the host's application menu
- the Wayland environment variables in `/etc/profile.d/crosvm-wayland.sh` (Wayland only, no X11)
- PipeWire for an optional virtio-snd sound card that plays into the host's PipeWire
- `video=Virtual-1:d` on the kernel command line
- its address on a host tap device from the kernel command line (`ip=`), or a
  static 192.168.10.2/24 without one, using systemd-networkd and systemd-resolved

## Build

Requires mkosi v26 or newer. Tested with mkosi v27.1 on an Arch Linux host
(rootless builds).

mkosi finds `mkosi/mkosi.conf` by itself, so run it from the repository root:

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
To keep the platform2 checkout and meson build between builds, run
`mkdir mkosi/mkosi.builddir`.

## Build options

These are set with `Environment=` in `mkosi/mkosi.conf`. Override them per build with
`-E`, or permanently in `mkosi/mkosi.local.conf` (ignored by git):

| Variable            | Default          | Meaning                                          |
|---------------------|------------------|--------------------------------------------------|
| `PASSWORDLESS_SUDO` | `yes`            | `no` makes sudo ask for the password             |
| `GUEST_USER`        | `user`           | name of the user that gets created               |
| `GUEST_PASSWORD`    | `changeme`       | password for sudo and the serial console (not ssh) |
| `SOMMELIER_REF`     | `main`           | platform2 branch, tag or commit to build         |
| `SOMMELIER_REPO`    | platform2 URL    | git repository to clone sommelier from           |
| `GUEST_ADDRESS`     | `192.168.10.2/24`| guest address when the kernel command line has no `ip=` |
| `GUEST_GATEWAY`     | `192.168.10.1`   | default gateway, likewise                        |
| `GUEST_DNS`         | `1.1.1.1,9.9.9.9`| comma-separated DNS servers, likewise            |
| `VM_NAME`           | the distribution | name in the host's app menu and window app_ids   |

```sh
mkosi -d debian -E PASSWORDLESS_SUDO=no -E GUEST_PASSWORD=hunter2 build
```

```ini
# mkosi/mkosi.local.conf
[Build]
Environment=PASSWORDLESS_SUDO=no GUEST_PASSWORD=hunter2
```

If you change `GUEST_USER`, set the same `[Guest] User=` in the VM's maitre
configuration.

## Booting

crosvm boots `vmlinuz`/`initrd` directly, so the kernel command line comes from
maitred (`KernelCommandLine=` in the VM's file, by default the same as in
`mkosi/mkosi.conf`), not from `mkosi/mkosi.conf`. If you change one, change
the other. The image also has an ESP with a UKI, so UEFI boot (`--bios
OVMF.fd` in `ExtraArguments=`) works too. That path uses `KernelCommandLine=`
from `mkosi/mkosi.conf`.

The root filesystem is 16 GiB but the file is sparse. To make it bigger, run
`truncate -s +10G mkosi/mkosi.output/debian.raw`. systemd-growfs then grows the
partition at the next boot.

## Layout

| Path                                   | Purpose                                              |
|----------------------------------------|------------------------------------------------------|
| `mkosi.conf`                           | shared settings, build options, kernel command line  |
| `mkosi.conf.d/10-<distro>.conf`        | release, packages and build packages for each distro |
| `mkosi.conf.d/20-debian-ubuntu-packages.conf` | packages Debian and Ubuntu have in common     |
| `mkosi.repart/`                        | ESP plus an ext4 root labelled `root`                |
| `mkosi.build.chroot`                   | builds sommelier and carte-agent inside the target distro |
| `mkosi.postinst.chroot`                | sets up the user, sudo, ssh, network, sommelier and carte-agent units, linger and Wayland variables |
| `mkosi.finalize`                       | points `/etc/resolv.conf` at the systemd-resolved stub |
| `mkosi.extra/`                         | `sshd-vsock.socket`, `sshd-vsock@.service`, preset, sshd drop-in |
| `authorized_keys`                      | your ssh public key (not in git)                     |

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
- Applications started from the host's menu run with the user manager's
  environment, which is why the Wayland variables are also in
  `/etc/environment.d/50-crosvm-wayland.conf`.
- `VM_NAME` is appended to the application names in the host's menu, and it's
  also sommelier's VM identifier, so GNOME can match windows to their launcher.
  It is not the name of the maitre configuration.
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
  interface, which in practice is the virtio-net device. maitred passes the
  guest its address on the kernel command line (`ip=`), and
  `systemd-network-generator` in the guest configures it.
- `KernelInitrdModules=` in `mkosi.conf` limits the initrd to the modules
  needed to mount the root filesystem: `virtio_pci`, `virtio_blk`, `ext4` and
  `crc32c*`, plus their dependencies. Kernels older than 6.14 (Debian) load
  crc32c for ext4 as a soft dependency, which mkosi doesn't add by itself.
  Without it, mkosi puts every module in the initrd, 120 to 400 MiB depending
  on the distribution. If the root partition isn't found at boot (the initrd
  drops into an emergency shell), a module is missing from this list.

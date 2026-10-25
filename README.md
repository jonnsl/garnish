# mkosi images for crosvm

Builds Arch Linux, Debian (trixie), Ubuntu (26.04 "resolute") and Fedora (44)
disk images for crosvm. Every image comes with:

- sshd on AF_VSOCK port 22 (`sshd-vsock.socket`)
- sommelier built from ChromiumOS platform2, installed to `/usr/local/bin/sommelier`
- a user (`jonnathan` by default) in `video`, `render` and `sudo`/`wheel`
- passwordless sudo, which you can turn off
- `~/.config/systemd/user/sommelier.service`, enabled, with linger turned on
- the Wayland environment variables in `~/.profile` (Wayland only, no X11)
- `video=Virtual-1:d` on the kernel command line

## Build

Requires mkosi v26 or newer (tested config parsing with v27.1).

```sh
# optional: your public key for ssh login
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
| `GUEST_PASSWORD`    | `changeme`       | the user's password; empty means key-only login  |
| `SOMMELIER_REF`     | `main`           | platform2 branch, tag or commit to build         |
| `SOMMELIER_REPO`    | platform2 URL    | git repository to clone sommelier from           |

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
./run-crosvm.sh debian          # CID=3, 4 CPUs, 4 GiB by default
CID=4 MEM=8192 ./run-crosvm.sh fedora --net tap-name=tap0
ssh jonnathan@vsock/3           # host systemd >= 256; otherwise:
ssh -o ProxyCommand='socat - VSOCK-CONNECT:3:22' jonnathan@localhost
```

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
| `mkosi.postinst.chroot`                | sets up the user, sudo, ssh, sommelier unit, linger and `.profile` |
| `mkosi.extra/`                         | `sshd-vsock.socket`, `sshd-vsock@.service`, preset   |
| `run-crosvm.sh`                        | boots an image with crosvm                           |

## Notes

- The sshd-vsock units have the same names as the ones `systemd-ssh-generator`
  makes, so these take precedence. They also work without the generator.
  Host keys are deleted at build time and created on the first connection,
  so two images never share host keys.
- In Arch and Fedora, `/etc/skel/.bash_profile` stops bash from reading `~/.profile`.
  The postinst script makes `~/.bash_profile` source `~/.profile`.
- Debian and Ubuntu need `dbus-user-session` to get a user D-Bus session.
  Fedora and Arch have one by default.
- These images are Wayland only: no Xwayland, and sommelier runs without `-X`.
  The xcb libraries are still installed because sommelier's `meson.build`
  needs them unconditionally.
- There is no guest networking out of the box. Add a crosvm `--net` option and a
  systemd-networkd `.network` file under `mkosi.extra/` if you need it.

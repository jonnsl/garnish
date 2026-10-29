# Guest packages

The parts of garnish that run inside a guest, packaged for each guest
distribution (Arch, Debian, Ubuntu, Fedora):

- **`sommelier`**: ChromiumOS's Wayland proxy, from the platform2 commit in
  `versions`, with the patches in `sommelier/patches/` applied. It ships
  `sommelier.service`, enabled for every user.
- **`garnish-guest`**: `carte-agent` (from `vm_tools/carte`) and its user
  unit, `maitre-ready.service`, and the Wayland environment
  (`/etc/profile.d/garnish-wayland.sh`,
  `/usr/lib/environment.d/50-garnish-wayland.conf`). It depends on
  `sommelier`.

The units are enabled by symlinks inside the packages, not by install
scripts, so they're on as soon as the package is installed. To turn one
off, mask it.

What stays in the image build (`mkosi/`): the user, sudo, ssh over vsock,
the network, linger, and `/etc/environment.d/60-carte.conf` with the VM
name. Those differ per image, or (the `sshd-vsock` units) have to be in
`/etc` to take precedence over systemd's ssh generator.

## Building

The image build makes the packages: `mkosi/mkosi.build.chroot` runs
`build-packages.sh` inside the target distribution, and mkosi installs the
result. The packages are also left in
`mkosi/mkosi.output/packages-<distribution>/`. To build just the packages:

```sh
mkosi -d fedora -t none build
```

Debian and Ubuntu get separate builds, linked against their own libraries.

`build-packages.sh` makes the source tarballs (sommelier with the patches
applied; carte plus `garnish-guest/`) and runs the distribution's own tool on
the recipe:

| Distribution  | Recipe                                      | Tool                 |
|---------------|---------------------------------------------|----------------------|
| Arch          | `sommelier/PKGBUILD`, `garnish-guest/PKGBUILD` | `makepkg`         |
| Debian/Ubuntu | `sommelier/debian/`, `garnish-guest/debian/` | `dpkg-buildpackage` |
| Fedora        | `sommelier/sommelier.spec`, `garnish-guest/garnish-guest.spec` | `rpmbuild` |

The recipes have `@VERSION@` and `@RELEASE@` in place of the version, and
Debian's `changelog` is generated, so they're only usable through
`build-packages.sh`. Files a package adds besides what it builds are
installed by one script that every format runs
(`sommelier/install-extras.sh`, `garnish-guest/install.sh`), so the file
lists live in one place.

## Versions

`versions` pins sommelier to a platform2 commit. Its package version is
`0.<commit date>.git<short hash>`. To move to a newer sommelier, change
`SOMMELIER_COMMIT` and set `SOMMELIER_RELEASE` back to 1; to try another
commit or branch once, `mkosi -E SOMMELIER_COMMIT=main build`. When only
the patches or the packaging change, bump `SOMMELIER_RELEASE`.

`GARNISH_GUEST_VERSION` and `GARNISH_GUEST_RELEASE` are the same for
`garnish-guest`.

## Patches

`sommelier/patches/*.patch` are applied in order with `git apply` from the
root of platform2 (paths like `vm_tools/sommelier/...`), so they can be
sent upstream as they are.

## Updating a running guest

```sh
mkosi -d debian -t none build
guest/update-guest.sh debian-vm
```

`update-guest.sh VM [DIR]` asks the guest for its distribution, copies the
packages from `mkosi/mkosi.output/packages-<distribution>/` (or `DIR`) over
`maitre ssh`, and runs `install-packages.sh` with sudo in the guest. That
installs them with pacman, apt or rpm (reinstalling when the version is the
same), and removes what older images put in `/usr/local/bin` and `/etc`
before these packages existed, since the copies in `/etc` would hide the
packaged ones.

The running sommelier keeps the old binary until it restarts. Reboot the VM,
or run `systemctl --user restart sommelier` in it, which closes every guest
window.

#!/bin/bash
# Builds the guest packages, sommelier and garnish-guest, for the
# distribution it runs on (Arch, Debian, Ubuntu or Fedora), with that
# distribution's own tools: makepkg, dpkg-buildpackage or rpmbuild.
#
#   build-packages.sh OUTDIR...
#
# Copies the finished packages into every OUTDIR. The image build runs it
# from mkosi/mkosi.build.chroot, inside the target distribution, with the
# build dependencies from mkosi.conf.d installed. It needs network access
# to fetch sommelier and the crates of carte and sonnette.
#
# Versions come from ./versions. The sommelier tarball is upstream at
# SOMMELIER_COMMIT with sommelier/patches/*.patch applied.
set -euo pipefail

guest=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$guest")
# shellcheck source=versions
source "$guest/versions"

if (( $# == 0 )); then
    echo "usage: $0 OUTDIR..." >&2
    exit 2
fi

. /etc/os-release
case "$ID" in
    arch|debian|ubuntu|fedora) ;;
    *) echo "build-packages.sh: unsupported distribution '$ID'" >&2; exit 1 ;;
esac

# With mkosi.builddir, the platform2 checkout and the crates are kept
# between builds.
work="${BUILDDIR:-$(mktemp -d)}/guest-packages"
stage="$work/stage"
out="$work/out"
rm -rf "$stage" "$out"
mkdir -p "$stage" "$out"

subst() {  # FILE VERSION RELEASE
    sed -e "s/@VERSION@/$2/g" -e "s/@RELEASE@/$3/g" "$1"
}

# --- sommelier source --------------------------------------------------------
# platform2 is huge: fetch only vm_tools/sommelier (cone mode also brings the
# files at the top, LICENSE among them).
src="$work/platform2"
if [[ ! -d "$src/.git" ]]; then
    git init -q "$src"
    git -C "$src" remote add origin "$SOMMELIER_REPO"
    git -C "$src" sparse-checkout set vm_tools/sommelier
fi
git -C "$src" remote set-url origin "$SOMMELIER_REPO"
git -C "$src" fetch -q --depth=1 --filter=blob:none origin "$SOMMELIER_COMMIT"
git -C "$src" checkout -q --force FETCH_HEAD
git -C "$src" clean -q -fdx

commit=$(git -C "$src" rev-parse HEAD)
date=$(git -C "$src" log -1 --format=%cd --date=format:%Y%m%d)
sommelier_version="0.$date.git${commit:0:7}"
echo "sommelier: $commit, version $sommelier_version-$SOMMELIER_RELEASE"

for p in "$guest"/sommelier/patches/*.patch; do
    [[ -e "$p" ]] || continue
    echo "sommelier: applying $(basename "$p")"
    git -C "$src" apply "$p"
done

sommelier_tree="$stage/sommelier-$sommelier_version"
cp -a "$src/vm_tools/sommelier" "$sommelier_tree"
install -d "$sommelier_tree/garnish"
cp "$guest/sommelier/install-extras.sh" "$guest/sommelier/sommelier.service" \
    "$sommelier_tree/garnish/"
cp "$src/LICENSE" "$sommelier_tree/garnish/LICENSE"
tar -C "$stage" -czf "$stage/sommelier-$sommelier_version.tar.gz" "sommelier-$sommelier_version"

# --- garnish-guest source ----------------------------------------------------
guest_version=$GARNISH_GUEST_VERSION
guest_tree="$stage/garnish-guest-$guest_version"
# Side by side, as in vm_tools/: sonnette uses carte through ../carte.
for crate in carte sonnette; do
    install -d "$guest_tree/$crate"
    tar -C "$repo/vm_tools/$crate" --exclude=./target -c . | tar -C "$guest_tree/$crate" -x
done
cp -a "$guest/garnish-guest/install.sh" "$guest/garnish-guest/files" "$guest_tree/"
cp "$repo/LICENSE" "$guest_tree/LICENSE"
tar -C "$stage" -czf "$stage/garnish-guest-$guest_version.tar.gz" "garnish-guest-$guest_version"

# The package builds run cargo with --frozen, from these.
export CARGO_HOME="$work/cargo"
for crate in carte sonnette; do
    cargo fetch --locked --manifest-path "$guest_tree/$crate/Cargo.toml"
done

# --- build -------------------------------------------------------------------
# name version release
packages=(
    "sommelier $sommelier_version $SOMMELIER_RELEASE"
    "garnish-guest $guest_version $GARNISH_GUEST_RELEASE"
)

build_arch() {  # name version release
    local dir="$work/arch/$1"
    rm -rf "$dir"
    mkdir -p "$dir"
    subst "$guest/$1/PKGBUILD" "$2" "$3" > "$dir/PKGBUILD"
    ln "$stage/$1-$2.tar.gz" "$dir/"
    # makepkg refuses to run as root, and the image build is root (in a
    # user namespace that maps only that one user). In a nested user
    # namespace the same user is uid 1000, and still owns every file.
    local -a as_user=()
    if (( EUID == 0 )); then
        as_user=(unshare --user --map-user=1000 --map-group=1000 --)
    fi
    # makepkg would also build in $BUILDDIR, which mkosi sets.
    (cd "$dir" && env -u BUILDDIR PKGDEST="$out" "${as_user[@]}" makepkg --nodeps --noconfirm --force)
}

build_deb() {  # name version release
    local tree="$stage/$1-$2"
    cp -a "$guest/$1/debian" "$tree/debian"
    cat > "$tree/debian/changelog" <<EOF
$1 ($2-$3) unstable; urgency=medium

  * Built by garnish's guest/build-packages.sh.

 -- Jonnathan Soares <jonnsl@hotmail.com>  $(date -R)
EOF
    ln "$stage/$1-$2.tar.gz" "$stage/$1_$2.orig.tar.gz"
    (cd "$tree" && dpkg-buildpackage -b -uc -us)
    mv "$stage/$1_$2-$3"_*.deb "$out/"
}

build_rpm() {  # name version release
    local top="$work/rpmbuild"
    mkdir -p "$top/SOURCES" "$top/SPECS"
    ln -f "$stage/$1-$2.tar.gz" "$top/SOURCES/"
    subst "$guest/$1/$1.spec" "$2" "$3" > "$top/SPECS/$1.spec"
    rpmbuild --define "_topdir $top" --define "_rpmdir $out" \
        --define "_build_name_fmt %%{NAME}-%%{VERSION}-%%{RELEASE}.%%{ARCH}.rpm" \
        -bb "$top/SPECS/$1.spec"
}

rm -rf "$work/rpmbuild"
for p in "${packages[@]}"; do
    read -r name version release <<<"$p"
    echo "building $name $version-$release for $ID"
    case "$ID" in
        arch) build_arch "$name" "$version" "$release" ;;
        debian|ubuntu) build_deb "$name" "$version" "$release" ;;
        fedora) build_rpm "$name" "$version" "$release" ;;
    esac
done

for dir in "$@"; do
    mkdir -p "$dir"
    cp "$out"/* "$dir/"
done
echo "packages:"
ls -l "$out"

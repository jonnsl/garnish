#!/bin/bash
# Runs the checks of .github/workflows/ci.yml locally: for each Rust crate
# rustfmt, clippy, the tests and (where CI does) a build with its
# rust-version; for Salle uncrustify, the build and the tests. Keep the two
# in step.
#
#   ./ci-checks.sh          check the working tree
#   ./ci-checks.sh <rev>    check a commit, in a temporary worktree, so
#                           uncommitted changes don't count (the pre-push
#                           hook does this)
#
# Needs rustup with the 1.85 toolchain, meson, vala, blueprint-compiler and
# uncrustify. Exits non-zero if any check fails.
set -u

repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel) || exit 1

# The crates CI checks, with the rust-version CI also builds with ("" for
# none). carte and sonnette are built inside Debian trixie guests.
crates="carte:1.85 gerant: sonnette:1.85 videur:"

tree=$repo
logs=$(mktemp -d "${TMPDIR:-/tmp}/garnish-ci.XXXXXX") || exit 1
cleanup() {
    if [ "$tree" != "$repo" ]; then
        git -C "$repo" worktree remove --force "$tree" 2>/dev/null
    fi
    rm -rf "$logs"
}
trap cleanup EXIT

if [ $# -gt 0 ]; then
    rev=$(git -C "$repo" rev-parse --verify "$1^{commit}") || exit 1
    tree="$logs/tree"
    git -C "$repo" worktree add --quiet --detach "$tree" "$rev" || exit 1
    echo "Checking $(git -C "$repo" log -1 --format='%h %s' "$rev")"
fi

failed=""

# step <name> <dir> <command...>: runs the command in dir, quietly unless it
# fails.
step() {
    local name=$1 dir=$2 log
    shift 2
    log="$logs/$(echo "$name" | tr ' /' '__').log"
    printf '%-40s ' "$name"
    if (cd "$dir" && "$@") >"$log" 2>&1; then
        echo ok
    else
        echo FAILED
        sed 's/^/    /' "$log" | tail -40
        failed="$failed
  $name"
    fi
}

for entry in $crates; do
    crate=${entry%%:*}
    msrv=${entry#*:}
    dir="$tree/vm_tools/$crate"
    # Reuse the working tree's build cache, also for a worktree.
    export CARGO_TARGET_DIR="$repo/vm_tools/$crate/target"
    step "$crate: rustfmt" "$dir" cargo +stable fmt --check
    step "$crate: clippy" "$dir" cargo +stable clippy --locked --all-targets -- -D warnings
    step "$crate: test" "$dir" cargo +stable test --locked
    if [ -n "$msrv" ]; then
        step "$crate: build with $msrv" "$dir" cargo "+$msrv" build --locked --all-targets
    fi
done
unset CARGO_TARGET_DIR

salle="$tree/vm_tools/salle"
step "salle: uncrustify" "$salle" sh -c \
    "git ls-files -z '*.vala' | xargs -0 uncrustify -c uncrustify.cfg -l VALA --check -q"
# A build directory of its own, so the working tree's build/ isn't touched.
build="$logs/salle-build"
step "salle: build" "$salle" sh -c "meson setup '$build' && meson compile -C '$build'"
step "salle: test" "$salle" meson test -C "$build" --print-errorlogs

if [ -n "$failed" ]; then
    echo
    echo "Failed:$failed"
    exit 1
fi
echo
echo "All checks passed."

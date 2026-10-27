#!/bin/sh
# Check Vala formatting (uncrustify) and style (vala-lint).
# Pass --fix to reformat files in place and apply vala-lint's automatic fixes.
set -eu

cd "$(dirname "$0")/.."

fix=false
if [ "${1:-}" = "--fix" ]; then
    fix=true
fi

status=0

if $fix; then
    git ls-files -z '*.vala' | xargs -0 uncrustify -c uncrustify.cfg -l VALA --replace --no-backup -q
    git ls-files -z '*.vala' | xargs -0 io.elementary.vala-lint --fix || status=1
else
    if ! git ls-files -z '*.vala' | xargs -0 uncrustify -c uncrustify.cfg -l VALA --check -q; then
        echo "Formatting is off; run build-aux/check-style.sh --fix" >&2
        status=1
    fi
    git ls-files -z '*.vala' | xargs -0 io.elementary.vala-lint || status=1
fi

exit $status

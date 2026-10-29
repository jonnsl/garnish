# The Wayland variables, for login shells (ssh). The user manager reads the
# same file from environment.d, so the applications carte-agent starts get
# them too.
set -a
. /usr/lib/environment.d/50-garnish-wayland.conf
set +a

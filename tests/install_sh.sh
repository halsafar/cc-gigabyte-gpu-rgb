#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Tests install.sh in DESTDIR staging trees: manifest-directory choice, idempotency,
# uninstall scope, and refusals. Needs no root and never touches the real system.
# Usage: sh tests/install_sh.sh   (from the repository root)

set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT INT TERM
FAKEBIN=$WORK/fake-binary
printf '#!/bin/sh\necho fake\n' > "$FAKEBIN"
chmod +x "$FAKEBIN"

PASS=0
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
ok() { PASS=$((PASS + 1)); }
run() { BINARY=$FAKEBIN "$ROOT/install.sh" "$@" > "$WORK/out" 2>&1 || { cat "$WORK/out"; return 1; }; }
has() { [ -e "$D$1" ] || fail "$CASE: missing $1"; ok; }
hasnt() { [ ! -e "$D$1" ] || fail "$CASE: unexpected $1"; ok; }
said() { grep -q "$1" "$WORK/out" || { cat "$WORK/out"; fail "$CASE: output lacks '$1'"; }; ok; }

SOCK=/var/lib/coolercontrol/plugins/gigabyte-gpu-rgb
COMMON="/usr/local/bin/cc-gigabyte-gpu-rgb /etc/systemd/system/cc-gigabyte-gpu-rgb.service /etc/modules-load.d/cc-gigabyte-gpu-rgb.conf"

newtree() {
    CASE=$1
    D=$WORK/$1
    mkdir -p "$D"
    export DESTDIR="$D"
}

common_installed() {
    for f in $COMMON; do has "$f"; done
    has $SOCK
}

everything_gone() {
    for f in $COMMON; do hasnt "$f"; done
    hasnt $SOCK
    # Through an absolute symlink this would test the real system; skip it there.
    if [ ! -L "$D/etc/coolercontrol/plugins" ]; then
        hasnt /etc/coolercontrol/plugins/gigabyte-gpu-rgb
    fi
    hasnt /var/lib/cc-gigabyte-gpu-rgb
}

# 1. Nothing from CoolerControl yet: native default.
newtree native-default
run
said "does not exist"
run # idempotent
common_installed
has $SOCK/manifest.toml
hasnt /etc/coolercontrol/plugins
mkdir -p "$D/var/lib/cc-gigabyte-gpu-rgb" "$D/var/lib/coolercontrol/plugins/other"
run --uninstall
everything_gone
has /var/lib/coolercontrol/plugins/other

# 2. Container daemon: /etc/coolercontrol/plugins is a real directory, and an old
#    manifest from an earlier default install sits in the socket directory.
newtree container
mkdir -p "$D/etc/coolercontrol/plugins/other" "$D$SOCK"
echo old > "$D$SOCK/manifest.toml"
run
said "real directory"
said "Removed the old manifest"
run
common_installed
has /etc/coolercontrol/plugins/gigabyte-gpu-rgb/manifest.toml
hasnt $SOCK/manifest.toml
run --uninstall
everything_gone
has /etc/coolercontrol/plugins/other

# 3. Native daemon: /etc/coolercontrol/plugins is an absolute symlink. The target must
#    be resolved inside DESTDIR, never on the real system.
newtree native-symlink
mkdir -p "$D/var/lib/coolercontrol/plugins" "$D/etc/coolercontrol"
ln -s /var/lib/coolercontrol/plugins "$D/etc/coolercontrol/plugins"
run
common_installed
has $SOCK/manifest.toml
said "is a symlink"
[ -L "$D/etc/coolercontrol/plugins" ] || fail "$CASE: symlink replaced"
ok
run --uninstall
everything_gone
[ -L "$D/etc/coolercontrol/plugins" ] || fail "$CASE: symlink removed"
ok

# 4. Override wins over detection; uninstall with the same override.
newtree override
mkdir -p "$D/etc/coolercontrol/plugins"
run --manifest-dir /opt/cc/plugins
has /opt/cc/plugins/gigabyte-gpu-rgb/manifest.toml
hasnt /etc/coolercontrol/plugins/gigabyte-gpu-rgb
said "given with --manifest-dir"
MANIFEST_PLUGINS_DIR=/opt/cc/plugins run --uninstall
hasnt /opt/cc/plugins/gigabyte-gpu-rgb
has /opt/cc/plugins
everything_gone

# 5. Refusals.
CASE=refusals
if BINARY=$FAKEBIN DESTDIR=$WORK/r "$ROOT/install.sh" --manifest-dir relative/dir > "$WORK/out" 2>&1; then
    fail "$CASE: relative manifest dir accepted"
fi
ok
if BINARY=$WORK/missing DESTDIR=$WORK/r "$ROOT/install.sh" > "$WORK/out" 2>&1; then
    fail "$CASE: missing binary accepted"
fi
ok
if [ "$(id -u)" -ne 0 ]; then
    if env -u DESTDIR BINARY="$FAKEBIN" "$ROOT/install.sh" > "$WORK/out" 2>&1; then
        fail "$CASE: ran without root"
    fi
    said "must run as root"
fi
[ ! -e "$WORK/r/usr" ] || fail "$CASE: refused run wrote files"
ok

printf 'install.sh tests: %d checks passed\n' "$PASS"

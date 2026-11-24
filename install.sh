#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Installs (or with --uninstall removes) cc-gigabyte-gpu-rgb: the binary, the systemd
# unit, the i2c-dev modules-load file and the CoolerControl plugin manifest.
# Run it from an unpacked release archive, or via `make install` from a checkout.
#
#   sudo ./install.sh                     install, or update in place
#   sudo ./install.sh --uninstall         remove everything this script installs
#   sudo ./install.sh --manifest-dir DIR  put the manifest in DIR/gigabyte-gpu-rgb
#
# Environment (used by the Makefile): DESTDIR stages into a directory without root
# and without touching systemd; BINARY is the binary to install; MANIFEST_PLUGINS_DIR
# is the same as --manifest-dir.
#
# It only ever touches its own files and the gigabyte-gpu-rgb subdirectories of the
# CoolerControl plugins directories, never anything else in them.

set -eu

NAME=cc-gigabyte-gpu-rgb
PLUGIN_ID=gigabyte-gpu-rgb

BINDIR=/usr/local/bin
UNITDIR=/etc/systemd/system
MODLOADDIR=/etc/modules-load.d
# Fixed: also in dist/manifest.toml (address), the unit (--socket, ReadWritePaths=)
# and src/lib.rs (DEFAULT_SOCKET).
SOCKET_DIR=/var/lib/coolercontrol/plugins/$PLUGIN_ID
# Fixed: StateDirectory= in the unit, DEFAULT_STATE_DIR in src/autosave.rs.
STATE_DIR=/var/lib/$NAME
# Where CoolerControl looks for manifests (see choose_manifest_dir).
NATIVE_PLUGINS_DIR=/var/lib/coolercontrol/plugins
LEGACY_PLUGINS_DIR=/etc/coolercontrol/plugins

SRC=$(cd "$(dirname "$0")" && pwd)
DESTDIR=${DESTDIR:-}
BINARY=${BINARY:-$SRC/$NAME}
MANIFEST_PLUGINS_DIR=${MANIFEST_PLUGINS_DIR:-}
ACTION=install

say() { printf '%s\n' "$*"; }
die() { printf '%s: %s\n' "$NAME" "$*" >&2; exit 1; }

usage() {
    cat <<EOF
Usage: sudo ./install.sh [--manifest-dir DIR]   install, or update in place
       sudo ./install.sh --uninstall            remove everything it installs

  --manifest-dir DIR  CoolerControl plugins directory to put the manifest in
                      (default: chosen automatically, and printed)
Environment: DESTDIR (stage without root/systemd), BINARY (binary to install),
             MANIFEST_PLUGINS_DIR (same as --manifest-dir)
EOF
    exit "${1:-0}"
}

while [ $# -gt 0 ]; do
    case $1 in
        --uninstall) ACTION=uninstall ;;
        --manifest-dir)
            [ $# -ge 2 ] || die "--manifest-dir needs a directory"
            MANIFEST_PLUGINS_DIR=$2
            shift
            ;;
        --manifest-dir=*) MANIFEST_PLUGINS_DIR=${1#--manifest-dir=} ;;
        -h | --help) usage 0 ;;
        *) printf 'unknown option: %s\n' "$1" >&2; usage 1 ;;
    esac
    shift
done

if [ -z "$DESTDIR" ] && [ "$(id -u)" -ne 0 ]; then
    die "must run as root (sudo $0)"
fi
case $DESTDIR in
    "" | /?*) ;;
    *) die "DESTDIR must be an absolute path" ;;
esac
case $MANIFEST_PLUGINS_DIR in
    "" | /?*) ;;
    *) die "the manifest directory must be an absolute path" ;;
esac

# Resolves a symlinked plugins directory one level, keeping the result inside
# DESTDIR (an absolute link target must not escape the staging tree).
resolve_dir() {
    _d=$1
    if [ -L "$DESTDIR$_d" ]; then
        _t=$(readlink "$DESTDIR$_d")
        case $_t in
            /*) _d=$_t ;;
            *) _d=$(dirname "$_d")/$_t ;;
        esac
    fi
    printf '%s\n' "$_d"
}

# Picks the plugins directory the CoolerControl daemon scans, and says why.
#
# The daemon scans <data dir>/plugins (or $CC_PLUGINS_DIR). The data dir is
# /var/lib/coolercontrol natively; the official container image sets
# CC_DATA_DIR=/etc/coolercontrol. A native daemon also replaces
# /etc/coolercontrol/plugins with a compatibility symlink to its real directory,
# while in the container /etc/coolercontrol/plugins *is* the real directory. So:
#   /etc/coolercontrol/plugins is a symlink -> use its target (native daemon)
#   /etc/coolercontrol/plugins is a directory -> use it (container daemon)
#   neither -> /var/lib/coolercontrol/plugins (native default)
choose_manifest_dir() {
    if [ -n "$MANIFEST_PLUGINS_DIR" ]; then
        MANIFEST_BASE=$MANIFEST_PLUGINS_DIR
        WHY="given with --manifest-dir / MANIFEST_PLUGINS_DIR"
    elif [ -L "$DESTDIR$LEGACY_PLUGINS_DIR" ]; then
        MANIFEST_BASE=$(resolve_dir "$LEGACY_PLUGINS_DIR")
        WHY="$LEGACY_PLUGINS_DIR is a symlink to it (made by a native CoolerControl daemon)"
    elif [ -d "$DESTDIR$LEGACY_PLUGINS_DIR" ]; then
        MANIFEST_BASE=$LEGACY_PLUGINS_DIR
        WHY="it is a real directory, as in the official CoolerControl container image"
        if [ -z "$DESTDIR" ] && command -v coolercontrold >/dev/null 2>&1; then
            say "warning: a native coolercontrold is installed; it reads $NATIVE_PLUGINS_DIR." >&2
            say "         If that is the daemon you use, re-run with --manifest-dir $NATIVE_PLUGINS_DIR" >&2
        fi
    else
        MANIFEST_BASE=$NATIVE_PLUGINS_DIR
        WHY="default for a native CoolerControl install ($LEGACY_PLUGINS_DIR does not exist)"
    fi
}

do_install() {
    [ -x "$BINARY" ] || die "binary not found or not executable: $BINARY"
    for f in "$NAME.service" modules-load.conf manifest.toml; do
        [ -f "$SRC/dist/$f" ] || die "missing $SRC/dist/$f"
    done
    choose_manifest_dir
    MANIFEST_DIR=$MANIFEST_BASE/$PLUGIN_ID

    install -D -m 0755 "$BINARY" "$DESTDIR$BINDIR/$NAME"
    install -D -m 0644 "$SRC/dist/$NAME.service" "$DESTDIR$UNITDIR/$NAME.service"
    install -D -m 0644 "$SRC/dist/modules-load.conf" "$DESTDIR$MODLOADDIR/$NAME.conf"
    install -d -m 0755 "$DESTDIR$SOCKET_DIR"
    install -d -m 0755 "$DESTDIR$MANIFEST_DIR"
    install -m 0644 "$SRC/dist/manifest.toml" "$DESTDIR$MANIFEST_DIR/manifest.toml"
    # A manifest left in the socket directory by an earlier install with another
    # choice is ours and now unused; remove it so only one copy exists.
    if [ "$(resolve_dir "$MANIFEST_BASE")/$PLUGIN_ID" != "$SOCKET_DIR" ] &&
        [ -f "$DESTDIR$SOCKET_DIR/manifest.toml" ]; then
        rm -f "$DESTDIR$SOCKET_DIR/manifest.toml"
        say "Removed the old manifest from $SOCKET_DIR"
    fi

    say "Installed $BINDIR/$NAME and $UNITDIR/$NAME.service"
    say "Manifest:  $MANIFEST_DIR/manifest.toml"
    say "           ($WHY)"
    say "Socket:    $SOCKET_DIR/$PLUGIN_ID.sock"

    [ -z "$DESTDIR" ] || return 0
    if command -v restorecon >/dev/null 2>&1; then
        restorecon -F "$BINDIR/$NAME" "$UNITDIR/$NAME.service" "$MODLOADDIR/$NAME.conf" || true
        restorecon -RF "$SOCKET_DIR" "$MANIFEST_DIR" || true
    fi
    modprobe i2c-dev 2>/dev/null || true
    systemctl daemon-reload
    systemctl enable "$NAME.service"
    systemctl restart "$NAME.service"
    say "Service $NAME started. Now restart CoolerControl (coolercontrold, or its container)."
}

do_uninstall() {
    if [ -z "$DESTDIR" ]; then
        systemctl disable --now "$NAME.service" 2>/dev/null || true
    fi
    rm -f "$DESTDIR$UNITDIR/$NAME.service" "$DESTDIR$BINDIR/$NAME" "$DESTDIR$MODLOADDIR/$NAME.conf"
    # The manifest may be in any of these; remove only our own subdirectory.
    for base in "$MANIFEST_PLUGINS_DIR" "$LEGACY_PLUGINS_DIR" "$NATIVE_PLUGINS_DIR"; do
        [ -n "$base" ] || continue
        dir=$(resolve_dir "$base")/$PLUGIN_ID
        if [ -d "$DESTDIR$dir" ] && [ ! -L "$DESTDIR$dir" ]; then
            rm -rf "${DESTDIR}${dir:?}"
            say "Removed $dir"
        fi
    done
    rm -rf "${DESTDIR}${SOCKET_DIR:?}" "${DESTDIR}${STATE_DIR:?}"
    if [ -z "$DESTDIR" ]; then
        systemctl daemon-reload
    fi
    say "Uninstalled $NAME. Restart CoolerControl so it forgets the plugin."
}

case $ACTION in
    install) do_install ;;
    uninstall) do_uninstall ;;
esac

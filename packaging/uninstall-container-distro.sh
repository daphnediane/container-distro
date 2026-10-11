#!/bin/sh
# Remove the container-distro .pkg — modeled on Apple's
# uninstall-container.sh: the file/dir list comes from the pkg receipt
# (never a hardcoded list, so an upgraded package's layout can't strand
# files), commands get sudo only when the path's parent isn't writable
# by this user, and -d additionally deletes per-user state.
#
#   uninstall-container-distro.sh -k        keep distro state
#   uninstall-container-distro.sh -d        delete it too (destroys distros)
#   uninstall-container-distro.sh -d|-k -n  dry run — show, don't do
#
# One flag is required — an explicit choice about the data. -d deletes
# every distro (containers via `delete --all`, when `container` services
# are already running — the check never starts them) and removes
# ~/Library/Application Support/container-distro for the invoking user
# (under sudo, SUDO_USER's). Plain `container` machines are `container`'s
# own data and are never touched.
set -u

PKG=io.github.daphnediane.container-distro
DELETE_DATA=
DRY=
OPTS=0

usage() {
    echo "Usage: $0 {-d | -k} [-n]    (-n alone previews -d)"
    echo "Uninstall container-distro"
    echo
    echo "Options:"
    echo "  -d  Delete per-user distro state (destroys all distros)."
    echo "  -k  Keep per-user distro state."
    echo "  -n  Dry run — show what would be done (alone: previews -d)."
    exit 1
}

while getopts ":dkn" arg; do
    case "$arg" in
        d) DELETE_DATA=true; OPTS=$((OPTS + 1)) ;;
        k) DELETE_DATA=false; OPTS=$((OPTS + 1)) ;;
        n) DRY=1 ;;
        *) echo "Invalid option: -$OPTARG"; usage ;;
    esac
done
if ! pkgutil --pkg-info "$PKG" >/dev/null 2>&1; then
    echo "$PKG is not installed" >&2
    exit 1
fi
if [ "$OPTS" != 1 ]; then
    if [ "$OPTS" = 0 ] && [ -n "$DRY" ]; then
        # -n alone previews the full -d cleanup — rerun with -d to do it.
        echo "Dry run — previewing as if -d were provided:"
        DELETE_DATA=true
    else
        echo "Provide either -d or -k"
        usage
    fi
fi
if [ "$(id -u)" -ne 0 ]; then
    echo "Removing root-owned files may require an administrator password."
fi

# run always executes (read-only queries must still run under -n);
# x executes or, in a dry run, just shows the command — including the
# sudo/user prefixes sudo_if and as_user would have chosen.
run() { "$@"; }
x() {
    if [ -n "$DRY" ]; then
        printf '  %s\n' "$(printf '%q ' "$@")"
    elif [ -n "$QUIET" ]; then
        "$@" >/dev/null
    else
        "$@"
    fi
}

# sudo only when needed: already root, or the path's parent dir is
# user-writable, means the command runs directly.
sudo_if() {
    # sudo_if <path affected> <command...>
    t=$1; shift
    if [ "$(id -u)" -eq 0 ] || [ -w "${t%/*}" ]; then
        x "$@"
    else
        x sudo "$@"
    fi
}

# The receipt records where the payload root landed ("usr/local" for
# current packages; "" meant / on the earliest builds). Normalize to a
# canonical absolute path — trailing slash stripped first so a bare "/"
# survives — and receipt paths join under it.
LOC=$(pkgutil --pkg-info "$PKG" | sed -n 's/^location: *//p' | head -n1)
LOC=${LOC%/}
LOC=/${LOC#/}

# Deletion must never escape the install root. TOP is LOC canonicalized,
# with /usr/local as the fallback when the receipt claims / (the old
# build's paths were usr/local/... under it). Every removal checks its
# parent dir resolves inside TOP — that blocks `..` traversal and a
# symlinked parent pointing elsewhere. Removing a symlink itself is
# safe: unlink doesn't follow it.
TOP=$LOC
[ "$TOP" = / ] && TOP=/usr/local
[ -d "$TOP" ] && TOP=$(cd -P "$TOP" && pwd -P)

in_top() {
    # in_top <path> — its parent dir must resolve inside TOP
    parent=${1%/*}
    [ -d "$parent" ] || return 1
    real=$(cd -P "$parent" 2>/dev/null && pwd -P) || return 1
    case "$real" in
        "$TOP" | "$TOP"/*) return 0 ;;
    esac
    return 1
}

if [ "$DELETE_DATA" = true ]; then
    # Under sudo, $HOME is root's — resolve the invoking user's home.
    if [ -n "${SUDO_USER:-}" ]; then
        UHOME=$(eval "echo ~$SUDO_USER")
    else
        UHOME=$HOME
    fi
    # Run as the invoking user: root's `container` sees a different
    # appRoot — the distros aren't there. First arg is the executor:
    # `run` for the read-only status check, `x` for `delete --all`.
    as_user() {
        f=$1; shift
        if [ -n "${SUDO_USER:-}" ]; then
            "$f" sudo -u "$SUDO_USER" "$@"
        else
            "$f" "$@"
        fi
    }
    # Delete our distro containers through the binary while it's still
    # installed — but only when `container` services are already up:
    # `system status` never starts them, and when they're down the
    # containers can't be reached at all. Whatever remains is
    # `container`'s storage (like plain machines) — left for it.
    CD="$TOP/bin/container-distro"
    CCLI=${CONTAINER_CLI:-container}
    if [ -x "$CD" ] && command -v "$CCLI" >/dev/null 2>&1 &&
        as_user run "$CCLI" system status --format json 2>/dev/null |
            grep -Eq '"status"[[:space:]]*:[[:space:]]*"running"'; then
        as_user x "$CD" delete --all -f ||
            echo "warning: distro cleanup failed — 'container list' shows the rest" >&2
    else
        echo "container services not running — distro containers (if any)"
        echo "are left in container's storage, alongside its machines"
    fi
fi

# Files first (receipt paths are relative to LOC; the ._ AppleDouble
# entries never exist on disk — the -f/-L test skips them).
pkgutil --only-files --files "$PKG" | while IFS= read -r f; do
    p="${LOC%/}/${f#./}"
    if [ -f "$p" ] || [ -L "$p" ]; then
        if in_top "$p"; then
            sudo_if "$p" rm -f "$p"
        else
            echo "skipping $p — parent resolves outside $TOP" >&2
        fi
    fi
done
# Then dirs, deepest first, only when empty — nothing foreign is ever
# deleted. TOP itself never matches in_top (its parent is outside), so
# the install root is never rmdir'd.
pkgutil --only-dirs --files "$PKG" | sort -r | while IFS= read -r d; do
    p="${LOC%/}/${d#./}"
    if in_top "$p"; then
        sudo_if "$p" rmdir "$p" 2>/dev/null
    elif [ -d "$p" ] || [ -L "$p" ]; then
        echo "skipping $p — parent resolves outside $TOP" >&2
    fi
done
QUIET=1 sudo_if /var/db/receipts pkgutil --forget "$PKG"

if [ "$DELETE_DATA" = true ]; then
    echo "Removing per-user distro state:"
    sudo_if "$UHOME/Library" rm -rf \
        "$UHOME/Library/Application Support/container-distro"
fi

if [ -n "$DRY" ]; then
    echo "Dry run — nothing was removed."
    if [ "$OPTS" = 0 ]; then
        echo "Rerun with -d or -k in place of -n to actually remove."
    else
        echo "Rerun without -n to actually remove."
    fi
else
    echo "Removed container-distro."
fi

#!/bin/sh
# Remove every container-distro install from this machine — for dev
# machines cycling between pkg / cargo / manual installs, when a new
# build must be the only copy. Removes:
#   - the .pkg payload (delegated to packaging/uninstall-container-distro.sh -k)
#   - plugin registration, generated man pages + completions, the `wsl` alias
#     (via the binaries' own uninstall-* commands — they only remove files
#     that look like our generated output, so probing share roots is safe)
#   - $CARGO_HOME/bin/{cm,container-distro} via `cargo uninstall`, and
#     $CARGO_HOME/libexec/container-distro/distrofs-server-* (build-distrofs.sh)
# Binaries for the uninstall-* calls resolve from PATH, $CARGO_HOME/bin,
# then target/release|debug — a dev build fingerprints just as well.
#
# Never touches user data: ~/Library/Application Support/container-distro
# and distro containers stay (`container distro rm --all --force` is the
# deliberate way to remove those). `-n` previews instead of doing.
set -u
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$*"; }

DRY=
for arg in "$@"; do
    case "$arg" in
        -n) DRY=1 ;;
        *) echo "usage: $0 [-n]" >&2; exit 2 ;;
    esac
done

x() {
    if [ -n "$DRY" ]; then
        printf '  %s\n' "$(printf '%q ' "$@")"
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

PKG=io.github.daphnediane.container-distro
CH=${CARGO_HOME:-$HOME/.cargo}
TGT=${CARGO_TARGET_DIR:-target}

# First resolvable copy of a binary: PATH, $CARGO_HOME/bin, then the
# newer dev build — anything that runs works as an uninstall/
# fingerprint tool, and debug is rebuilt far more often than release.
find_bin() {
    p=$(command -v "$1" 2>/dev/null || true)
    [ -n "$p" ] || [ ! -x "$CH/bin/$1" ] || p="$CH/bin/$1"
    if [ -z "$p" ]; then
        for c in "$TGT/debug/$1" "$TGT/release/$1"; do
            [ -x "$c" ] || continue
            [ -z "$p" ] || [ "$c" -nt "$p" ] || continue
            p=$c
        done
    fi
    printf %s "$p"
}

step "pkg payload"
if pkgutil --pkg-info "$PKG" >/dev/null 2>&1; then
    # -k keeps per-user state; -k is a normal uninstall minus the
    # data-path bits this script deliberately never does.
    if [ -n "$DRY" ]; then
        sh packaging/uninstall-container-distro.sh -kn
    else
        sh packaging/uninstall-container-distro.sh -k
    fi
else
    echo "  (no receipt for $PKG)"
fi

# After the pkg removal these resolve to a cargo/manual copy if one is
# on PATH; find_bin falls back to $CARGO_HOME/bin and dev builds.
CM=$(find_bin cm)
CD=$(find_bin container-distro)

step "man pages + completions"
if [ -z "$CD" ] && [ -z "$CM" ]; then
    echo "  (no cm/container-distro binaries found to fingerprint with)"
fi
# Share roots to probe: cargo and ~/.local defaults, /usr/local for
# manual sudo installs the pkg path didn't cover, plus each resolved
# binary's own adjacent share/ (its install-* default target).
SHARES="$CH/share $HOME/.local/share /usr/local/share"
for b in $CM $CD; do
    [ -n "$b" ] || continue
    s=$(dirname "$(dirname "$b")")/share
    case " $SHARES " in *" $s "*) ;; *) SHARES="$SHARES $s" ;; esac
done
for share in $SHARES; do
    man="$share/man/man1"
    [ -d "$share" ] || continue
    if [ -n "$CD" ]; then
        x "$CD" uninstall-man --dir "$man" || echo "  (uninstall-man $man failed)"
        x "$CD" uninstall-completions --dir "$share" ||
            echo "  (uninstall-completions $share failed)"
    fi
    if [ -n "$CM" ]; then
        x "$CM" --uninstall-man="$man" || echo "  (uninstall-man $man failed)"
        x "$CM" --uninstall-completions="$share" ||
            echo "  (uninstall-completions $share failed)"
    fi
done

step "plugin registration + alias"
# uninstall-plugin verifies the config.toml fingerprint, removes only
# the files install wrote, and self-elevates when the dir needs root.
# Its default dir follows the `container` binary's prefix.
PDIRS="/usr/local/libexec/container-plugins"
CB=$(command -v container 2>/dev/null || true)
if [ -n "$CB" ]; then
    pdir="$(dirname "$(dirname "$CB")")/libexec/container-plugins"
    case " $PDIRS " in *" $pdir "*) ;; *) PDIRS="$PDIRS $pdir" ;; esac
fi
if [ -n "$CD" ]; then
    for pdir in $PDIRS; do
        [ -d "$pdir/distro" ] || continue
        x "$CD" uninstall-plugin --plugin-dir "$pdir" ||
            echo "  (uninstall-plugin $pdir failed)"
    done
else
    echo "  (no container-distro binary — trying direct removal)"
fi
# Last resort for a broken install: a leftover distro/ subtree is ours
# alone — remove exactly it, never the plugins root.
for pdir in $PDIRS; do
    if [ -d "$pdir/distro" ]; then
        echo "  leftover $pdir/distro — force-removing it alone:"
        sudo_if "$pdir" rm -rf "$pdir/distro"
    fi
done
[ -n "$CM" ] && { x "$CM" --uninstall-alias wsl 2>/dev/null || true; }

step "cargo uninstall"
if command -v cargo >/dev/null 2>&1 &&
    { [ -e "$CH/bin/cm" ] || [ -e "$CH/bin/container-distro" ]; }; then
    x cargo uninstall container-distro ||
        echo "  (cargo uninstall failed — remove $CH/bin/{cm,container-distro} by hand)"
else
    echo "  (no binaries under $CH/bin)"
fi
# scripts/build-distrofs.sh --install's guest file server (current
# name plus the earlier one).
removed=
for f in distrofs-server-aarch64 distrofs; do
    if [ -e "$CH/libexec/container-distro/$f" ]; then
        x rm -f "$CH/libexec/container-distro/$f"
        removed=1
    fi
done
if [ -n "$removed" ]; then
    x rmdir "$CH/libexec/container-distro" 2>/dev/null
    x rmdir "$CH/libexec" 2>/dev/null
fi

if [ -z "$DRY" ]; then
    left=
    for b in cm container-distro; do
        p=$(command -v "$b" 2>/dev/null || true)
        [ -n "$p" ] && left="$left $p"
    done
    [ -n "$left" ] && echo "  still on PATH:$left"
fi

if [ -n "$DRY" ]; then
    echo "Dry run — nothing was removed."
else
    echo "Done. Distro state and containers untouched —"
    echo "  'container distro rm --all --force' removes those if wanted."
fi

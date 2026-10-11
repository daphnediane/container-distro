#!/bin/sh
# Remove everything the container-distro .pkg installed — same role as
# Apple's own uninstall-container.sh. The file list comes from the pkg
# receipt, never a hardcoded list, so an upgraded package's layout
# can't strand files. Must run as root (the files are root-owned);
# the pkg receipt tells us the paths.
#
#   sudo /usr/local/bin/uninstall-container-distro.sh
#
# Per-user state (~/Library/Application Support/container-distro) and
# any distros/machines are untouched — `cm -l` lists them,
# `cm --unregister <name>` deletes them.
set -eu

PKG=io.github.daphnediane.container-distro

if [ "$(id -u)" != 0 ]; then
    echo "re-run as root: sudo $0" >&2
    exit 1
fi
if ! pkgutil --pkg-info "$PKG" >/dev/null 2>&1; then
    echo "$PKG is not installed" >&2
    exit 1
fi

# Only ever remove under /usr/local — the receipt is our whole list of
# what we own, and directories go last (deepest first) and only when
# empty, so nothing foreign is ever deleted.
pkgutil --files "$PKG" | while IFS= read -r f; do
    f=${f#./}
    case "/$f" in
        /usr/local/*)
            if [ -f "/$f" ] || [ -L "/$f" ]; then
                rm -f "/$f"
            fi
            ;;
    esac
done
pkgutil --files "$PKG" | sort -r | while IFS= read -r f; do
    f=${f#./}
    case "/$f" in
        /usr/local/*) rmdir "/$f" 2>/dev/null || true ;;
    esac
done
pkgutil --forget "$PKG"
echo "container-distro removed."
echo "Per-user state under ~/Library/Application Support/container-distro is untouched;"
echo "delete distros first with \`cm --unregister <name>\` if you want a clean slate."

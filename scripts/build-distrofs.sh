#!/bin/sh
# Build the guest-side file server from the sibling distrofs checkout
# (https://github.com/daphnediane/distrofs) as a static
# aarch64-linux-musl binary — linked by rust-lld against Rust's
# self-contained musl, so no cross C toolchain or container is needed.
# The VM kernel is arm64 even for amd64 (Rosetta) distros, so this one
# binary serves every distro.
#
#   scripts/build-distrofs.sh                  # build only
#   scripts/build-distrofs.sh --install PREFIX # also copy to
#                                              # PREFIX/libexec/container-distro/
#
# DISTROFS_DIR overrides where the distrofs checkout lives (default:
# ../distrofs next to this repo). Host binaries in <prefix>/bin look
# for it in <prefix>/libexec/container-distro/distrofs-server-aarch64
# (so `--install ~/.cargo` pairs with `cargo install`); `--dev` prefers
# the sibling worktree's target/aarch64-unknown-linux-musl builds.
# `install-plugin` copies whichever it finds beside the plugin copy.
set -eu

DISTROFS="${DISTROFS_DIR:-$(dirname "$0")/../../distrofs}"
if [ ! -f "$DISTROFS/Cargo.toml" ]; then
    echo "no distrofs checkout at $DISTROFS — clone daphnediane/distrofs" \
        "beside this repo or set DISTROFS_DIR" >&2
    exit 1
fi

TARGET=aarch64-unknown-linux-musl
rustup target list --installed | grep -qx "$TARGET" || rustup target add "$TARGET"
cargo build --release --target "$TARGET" --manifest-path "$DISTROFS/Cargo.toml"
BIN="$DISTROFS/target/$TARGET/release/distrofs"
echo "built $BIN"

if [ "${1:-}" = "--install" ]; then
    PREFIX="${2:?usage: $0 --install PREFIX}"
    DEST="$PREFIX/libexec/container-distro"
    mkdir -p "$DEST"
    install -m 0755 "$BIN" "$DEST/distrofs-server-aarch64"
    echo "installed $DEST/distrofs-server-aarch64"
fi

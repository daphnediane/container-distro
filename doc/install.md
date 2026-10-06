# Building and installing

## Prerequisites

- macOS with Apple's [`container`](https://github.com/apple/container)
  installed and working (`container system start`, `container machine
  list`). Development targets `container` 1.5.x; a code path that reads
  `appRoot` internals warns once per process on unverified releases —
  see [container-internals.md](container-internals.md).
- A current stable Rust toolchain (edition 2024; `rustup` recommended).

## Build

```bash
cargo build --workspace      # debug binaries in target/debug/
cargo build --release        # optimized
```

Checks before committing (CI-equivalent):

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The workspace has three crates:

| Crate                        | Contents                                                        |
| ---------------------------- | --------------------------------------------------------------- |
| `crates/cm`                  | the `cm` binary (WSL-compatible CLI)                              |
| `crates/container-distro`    | the `container-distro` binary + the `container_distro` library    |
| `crates/cm-core`             | shared `container` CLI plumbing (naming, OCI, forwarder, tables)  |

## Install

`cargo install` only copies binaries, so installing is cargo plus two
small post-steps (plugin registration, man pages) — each binary
self-installs what it owns.

```bash
cargo install --path crates/cm
cargo install --path crates/container-distro   # also provides cm's distro support

# man pages (self-generated from the CLI definitions — never stale)
cm --install-man
container-distro install-man

# optional: symlink so WSL-shaped tooling finds `wsl`
ln -sf "$(which cm)" "$(dirname "$(which cm)")/wsl"

# optional: register the `container distro` subcommand (cm doesn't need it)
sudo container-distro install-plugin    # -> /usr/local/libexec/container-plugins/distro
```

`install-plugin` resolves `container` (`CONTAINER_CLI`, then
`/usr/local/bin/container`, then `PATH`), derives
`<prefix>/libexec/container-plugins/` from it, and **copies** the binary
there — a symlink would let your user account replace what other users'
`container distro` invocations exec. `uninstall-plugin` removes the
registration.

The plugin is optional: `cm` links the `container_distro` library
directly, and the standalone `container-distro` binary exposes the same
interface as `container distro`.

### Man pages

Man pages are generated at runtime by `clap_mangen` from the clap
definitions — they cannot drift from `--help`, and re-running the
install command after an upgrade rewrites them.

The default target directory is `share/man/man1` *next to the binary's
`bin` directory*, so pages follow the binary wherever it was installed:

- `cargo install` → `$CARGO_HOME/bin/cm` → `$CARGO_HOME/share/man/man1`
- `cargo install --root /usr/local` → `/usr/local/share/man/man1`
- dev build in `target/debug` → falls back to `$CARGO_HOME/share/man/man1`

macOS `man` (and Linux man-db) add `<dir>/../share/man` to the search
path for every `bin` directory on `PATH`, so no manpath configuration is
needed as long as the install `bin` dir is on `PATH`. (Third-party `man`
ports, e.g. MacPorts mandoc, may not do this mapping — the system
`/usr/bin/man` does.)

```bash
cm --install-man              # install cm(1) to the default dir
cm --install-man=/path/man1   # explicit dir (= is required)
container-distro install-man --dir /path/man1
```

Run `install-man` on the `cargo install`ed copy — invoked through the
plugin (`container distro install-man`), the binary lives under
`libexec/container-plugins/` and the exe-relative default would put the
pages inside the plugin tree where `man` won't look.

If `cm` is symlinked to `wsl`, the man page stays `cm(1)` — symlink
`wsl.1` next to it if you want `man wsl` too.

## Upgrade

Same commands again — `cargo install` replaces the binaries, the
`install-man` commands rewrite the pages (they're rendered from the new
binary, so they're always current), and `install-plugin` refreshes the
registered copy (or `container-distro install-plugin --from
<installed>` from a newer build).

## Uninstall

```bash
container-distro uninstall-plugin   # only removes our registration
cargo uninstall cm container-distro
rm "$CARGO_HOME/share/man/man1/cm.1" "$CARGO_HOME/share/man/man1/"container-distro*.1
```

Distro state (`~/Library/Application Support/container-distro/`) and any
distros/machines you created are left behind; `cm -l` lists them and
`cm --unregister <name>` deletes them first if you want a clean removal.

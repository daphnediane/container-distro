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

| Crate                     | Contents                                                         |
| ------------------------- | ---------------------------------------------------------------- |
| `crates/cm`               | the `cm` binary (WSL-compatible CLI)                             |
| `crates/container-distro` | the `container-distro` binary + the `container_distro` library   |
| `crates/cm-core`          | shared `container` CLI plumbing (naming, OCI, forwarder, tables) |

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

# optional: a `wsl` alias symlink (bin + man page) so WSL-shaped
# tooling finds it — a bare name lands next to cm, or pass a path:
cm --install-alias wsl
cm --install-alias ~/.local/bin/wsl

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

The default target directory is `share/man/man1` of the prefix the
binary is installed under — derived from the binary's own path, so it
follows the install wherever it went:

- `cargo install` → `$CARGO_HOME/bin/cm` → `$CARGO_HOME/share/man/man1`
- `cargo install --root /usr/local` → `/usr/local/share/man/man1`

The binary must live in a `bin` directory for the default to apply; a
dev build (`target/debug/cm`) has no install prefix and errors —
pass an explicit dir in that case:

```bash
cm --install-man              # install cm(1) to the default dir
cm --install-man=/path/man1   # explicit dir (= is required)
cm --uninstall-man            # remove our pages again (foreign or
                              # edited files are left alone)
container-distro install-man [--dir /path/man1]   # + uninstall-man
```

macOS `man` (and Linux man-db) add `<dir>/../share/man` to the search
path for every `bin` directory on `PATH`, so no manpath configuration is
needed as long as the install `bin` dir is on `PATH`. (Third-party `man`
ports, e.g. MacPorts mandoc, may not do this mapping — the system
`/usr/bin/man` does.)

Run `install-man` on the `cargo install`ed copy — invoked through the
plugin (`container distro install-man`), the binary lives under
`libexec/container-plugins/` and the pages would land inside the plugin
tree where `man` won't look.

### `wsl` aliases

`cm --install-alias NAME|PATH` creates a symlink to `cm` — a bare `NAME`
(e.g. `wsl`) goes next to the binary itself, a `PATH` (like
`~/.local/bin/wsl`) is used as given — plus a `NAME(1)` symlink to
`cm.1` in the man dir when the man page is installed (skipped otherwise;
re-run `--install-alias` after `--install-man` to add just the link).
`--uninstall-alias` removes both, and only when they're provably ours:
the alias must be a symlink resolving to this `cm` binary and the man
page a symlink to `cm.1` — anything else is refused.

## Upgrade

Same commands again — `cargo install` replaces the binaries, the
`install-man` commands rewrite the pages (they're rendered from the new
binary, so they're always current), and `install-plugin` refreshes the
registered copy (or `container-distro install-plugin --from
<installed>` from a newer build).

## Uninstall

```bash
container-distro uninstall-plugin   # only removes our registration
cm --uninstall-alias wsl            # if you installed one
cm --uninstall-man && container-distro uninstall-man
cargo uninstall cm container-distro
```

Distro state (`~/Library/Application Support/container-distro/`) and any
distros/machines you created are left behind; `cm -l` lists them and
`cm --unregister <name>` deletes them first if you want a clean removal.

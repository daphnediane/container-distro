# Building and installing

The easiest path needs no toolchain at all: each [release] ships a
signed, notarized macOS installer package -- download
`container-distro-<version>.pkg` and double-click it, or:

```bash
sudo installer -pkg container-distro-<version>.pkg -target /
```

A published release normally only carries the signed build. CI and
draft prereleases may still have
`container-distro-<version>-unsigned.pkg`, which Gatekeeper installs
with right-click → Open or the same `installer` command; [packaging]
covers the signing story. It lands under `/usr/local`: `bin/cm` and
`bin/container-distro`, the `container distro` plugin registration
(`libexec/container-plugins/distro/`), man pages, bash/zsh/fish
completions, `bin/uninstall-container-distro.sh`,
`bin/update-container-distro.sh` (a thin wrapper over
`container distro update`), and
`share/doc/container-distro/` (LICENSE + THIRD-PARTY-NOTICES). When
`container` isn't installed yet, the first `cm` run offers Apple's
signed installer.

The release also carries a `cargo-binstall`-compatible tarball, so
`cargo binstall container-distro` fetches prebuilt binaries instead of
compiling (then run the same post-steps under [Install](#install)
below -- binstall only drops the binaries into `$CARGO_HOME/bin`).

## Verifying the download

Signed builds sign the pkg, the binaries, and the tarball -- the
tarball gets a detached code signature (`<file>.tar.gz.sig`) and a GPG
signature (`<file>.tar.gz.asc`):

```bash
pkgutil --check-signature container-distro-<version>.pkg
codesign --verify --detached=<file>.tar.gz.sig <file>.tar.gz
gpg --verify <file>.tar.gz.asc <file>.tar.gz
codesign -dv /usr/local/bin/container-distro   # after install
```

Expected signers:

- `.pkg`: `Developer ID Installer: Daphne Pfister (YN9EG6U9PY)`;
  detached `.sig` + binaries: `Developer ID Application: Daphne
  Pfister (YN9EG6U9PY)`. The certificates chain to the Apple Root CA
  (`codesign -dvvv --detached=` lists them under `Authority=`); the
  signer name + team ID `YN9EG6U9PY` are the stable check.
- `.asc`: GPG `29F8 7CBD 7DF6 A40E 2A44 7111 6F4D 7952 6D9B AEDF`
  (Daphne Pfister <daphnediane@mac.com>). Fetch the public key with
  `curl https://github.com/daphnediane.gpg | gpg --import` or `gpg
  --keyserver keys.openpgp.org --recv-keys
  29F87CBD7DF6A40E2A4471116F4D79526D9BAEDF` -- or import the copy in
  [packaging/release-pubkey.asc](../packaging/release-pubkey.asc).

## Prerequisites

- macOS with Apple's [`container`] installed and working
  (`container system start`, `container machine
  list`). Development targets `container` 1.5.x; a code path that reads
  `appRoot` internals warns once per process on unverified releases --
  see design/container-internals.md. When `container` is missing, an
  interactive `cm`/`container-distro` offers to download the signed
  installer for the verified release (SHA-256 pinned in the binary) and
  open it in Installer.app -- `CONTAINER_DISTRO_NO_INSTALL_OFFER=1`
  disables the offer.
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

The repository is a single crate:

| Path                    | Contents                                                           |
| ----------------------- | ------------------------------------------------------------------ |
| `src/`                  | `container_distro` library: `naming` plus the modules below        |
| `src/container/`        | `container` CLI wrappers, its JSON types, the install offer        |
| `src/catalog/`          | installable catalog, `.wsl` packages, verified downloads           |
| `src/setup/`            | layout, plugin registration, man/completions, self-update, sudo    |
| `src/util/`             | table rendering, OCI layout writer, TCP forwarder                  |
| `src/distro/`           | distro operations, one submodule each; `spec/` and `terminal`      |
| `src/cli/commands/`     | `container-distro` subcommands, one file per larger subcommand     |
| `src/cli/commands/cli/` | the `cli` subcommand: `cm`'s WSL-compatible interface              |
| `src/bin/`              | the `container-distro` binary's one-line `main` (`cm` is an alias) |

## Install

`cargo install` only copies the one binary, `container-distro`, so
installing is cargo plus `install-all`, which sets up everything
around it. The release [.pkg](#building-and-installing) skips this by
installing the finished layout directly ([#14]) -- its payload is
produced by the same `install-all`.

```bash
cargo install --locked container-distro
# or from a checkout:
cargo install --path .

# the `cm` alias, man pages + shell completions for both interfaces
# (self-generated from the CLI definitions -- never stale), and the
# `container distro` plugin registration:
container-distro install-all
container-distro install-all --alias wsl   # ...plus a `wsl` alias
container-distro install-all --no-plugin   # ...without the plugin
```

`cm` is not a binary: it is an alias script running
`container-distro cli` ([wsl aliases](#wsl-aliases)). `install-all`
writes it beside the binary (`$CARGO_HOME/bin/cm`) and replaces a `cm`
binary from an older release there. The individual steps remain
available for partial installs: `install-man`, `install-completions`,
`install-plugin`, `install-alias` (and `cli --install-man` etc. for
the `cm` pages). Packaging stages a payload with `install-all --prefix
/usr/local --stage DIR`: files are written under `DIR`, while aliases
run `/usr/local/bin/container-distro`.

`install-plugin` resolves `container` (`CONTAINER_CLI` under `--dev`,
then `/usr/local/bin/container`, then `PATH`) as the invoking user --
offering to install `container` itself first when it's missing and the
session is interactive. It derives `<prefix>/libexec/container-plugins/`
from the resolved binary and **copies** itself there -- a symlink would
let your user account replace what other users' `container distro`
invocations exec. When the plugin directory isn't user-writable (the
normal `/usr/local` case), it re-execs itself as
`sudo <self> install-plugin --plugin-dir <root>` so only the file ops
run as root -- the `sudo` password prompt comes from `sudo` itself, and
prefixing `sudo` by hand still works. It also installs the distro init
assets (`sbin.distro*/`) alongside the binary: outside the shared home
and root-owned for a standard install, so guests can't rewrite what
runs as PID 1 (T6). `cm`-only installs use per-user copies kept
immutable with `uchg`. `uninstall-plugin` removes the registration and
the assets, self-elevating the same way.

The plugin is optional: `cm` links the `container_distro` library
directly, and the standalone `container-distro` binary exposes the same
interface as `container distro`.

### Man pages

Man pages are generated at runtime by `clap_mangen` from the clap
definitions -- they cannot drift from `--help`, and re-running the
install command after an upgrade rewrites them. Pages for subcommands
that no longer exist are pruned at the same time (only files that
still look like our generated pages are touched).

The default target directory is `share/man/man1` of the prefix the
binary is installed under -- derived from the binary's own path, so it
follows the install wherever it went:

- `cargo install` → `$CARGO_HOME/bin/container-distro` → `$CARGO_HOME/share/man/man1`
- `cargo install --root /usr/local` → `/usr/local/share/man/man1`

The binary must live in a `bin` directory for the default to apply; a
dev build (`target/debug/container-distro`) has no install prefix and errors -- pass
an explicit dir in that case:

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
ports, e.g. MacPorts mandoc, may not do this mapping -- the system
`/usr/bin/man` does.)

Run `install-man` on the `cargo install`ed copy -- invoked through the
plugin (`container distro install-man`), the binary lives under
`libexec/container-plugins/` and the pages would land inside the plugin
tree where `man` won't look.

### Shell completions

`cm --install-completions` / `container-distro install-completions`
write bash, zsh, and fish scripts -- generated at runtime by
`clap_complete` from the same clap definitions as the man pages -- to
the conventional vendor locations under the share root:

- `share/bash-completion/completions/<bin>`
- `share/zsh/site-functions/_<bin>`
- `share/fish/vendor_completions.d/<bin>.fish`

The share root defaults to the binary's install prefix (`<bin-dir>/..` →
`share`), so `cargo install --root /usr/local` lands under
`/usr/local/share` -- prefixes whose vendor dirs the shells already
scan. For `$CARGO_HOME/share` or a dev build you'll want an explicit
dir on the shell's search path:

```bash
cm --install-completions=~/.local/share        # fish + bash-completion
                                               # pick ~/.local/share up
container-distro install-completions --dir ~/.local/share
cm --uninstall-completions                      # remove ours again
                                                # (foreign/edited files
                                                # are left alone)
```

`--uninstall-completions` removes only scripts that still carry the
generated signature for that binary -- a foreign or hand-replaced file
of the same name is left in place with a warning.

### `wsl` aliases

`cm --install-alias NAME|PATH` writes a small alias script that runs
`cm` -- a bare `NAME` (e.g. `wsl`) goes next to the alias you ran it
through, else next to the binary; a `PATH` (like `~/.local/bin/wsl`) is
used as given -- plus a `NAME(1)` symlink to `cm.1` in the man dir when
the man page is installed (skipped otherwise; re-run `--install-alias`
after `--install-man` to add just the link). `container-distro
install-alias` does the same for `container-distro` itself, and
`container-distro cli --install-alias` makes a `cm`-style alias from a
plugin-only install. `--uninstall-alias` removes both, and only when
they're provably ours: the alias must be one of our alias scripts (or a
legacy symlink to this binary) and the man page a symlink to one of our
pages -- anything else is refused.

## Upgrade

For a `.pkg` install, `cm --update` (equivalently `container distro
update` or `/usr/local/bin/update-container-distro.sh`) fetches a
release from GitHub -- the latest, or `cm --update 0.5.0` for a
specific one -- prefers the signed `container-distro-<ver>.pkg` (the
unsigned CI build needs an explicit yes), verifies the SHA-256 the
release reports for the asset plus a `pkgutil --check-signature`
Developer ID and a signing-team match against the installed copy, then
`exec`s `open` on the result so Installer.app owns the privileged
install. Downgrades, same-version reinstalls, and non-pkg installs ask
first (`--force` skips the asking); as root it's refused -- run it
without `sudo`. `--dry-run` resolves and verifies the package, then
reports instead of installing. Nothing needs stopping first: the
binaries are per-invocation, and running distros mount their own
asset copies.

For a `cargo install`/`cargo binstall` copy the same commands again --
`cargo install` replaces the binary, and `container-distro install-all`
rewrites the pages and completions (rendered from the new binary, so
always current), refreshes the aliases, and re-registers the plugin
copy. Upgrading from a release that still shipped a `cm` binary,
`cargo install` removes that binary, so `cm` is missing until
`install-all` writes the alias in its place; an old `wsl` symlink to
it is upgraded to an alias script by `install-all --alias wsl` (or
`cm --install-alias wsl`). `cm --update` on such an install warns and
asks before laying a pkg down next to it. From a checkout,
`scripts/install-local.sh` bundles the maintainer version of this:
`cargo install --path .` then `container-distro install-all`.

Upgrading from ≤0.4.1 needs `--force` once: the `cm` binary was owned
by a separate `cm` package then, and cargo won't overwrite a binary
owned by another package (`cargo uninstall cm` first works too).

## Uninstall

```bash
container-distro uninstall-all [--alias wsl]   # aliases, pages, completions, plugin
cargo uninstall container-distro               # the binary
```

`uninstall-all` removes only what is provably ours: our alias scripts
(or legacy symlinks to our binary), our generated pages and
completions, and our plugin registration. Run it before `cargo
uninstall` -- cargo doesn't know about the `cm` alias script.

For a `.pkg` install, `sudo /usr/local/bin/uninstall-container-distro.sh -k`
(add `-n` to preview every command first)
removes exactly what the receipt owns and forgets it (the manual
equivalent: `pkgutil --files io.github.daphnediane.container-distro`
lists the files — all under `/usr/local` — then `sudo pkgutil
--forget`).

With `-k` (keep), distro state (`~/Library/Application Support/container-distro/`)
and any distros/machines you created are left behind; `cm -l` lists
them and `cm --unregister <name>` deletes them first if you want a
clean removal. Pass `-d` instead: when `container` services are running
it first deletes every distro container via `container-distro delete
--all -f` (plain `container` machines are `container`'s own and are
never touched), then removes the whole state directory.

For dev machines that mix pkg, `cargo install`, and manual `install-*`
output, `scripts/dev-clean.sh` removes all installed artifacts at once
(distro state and containers are kept; `-n` previews).

[#14]: https://github.com/daphnediane/container-distro/issues/14
[`container`]: https://github.com/apple/container
[packaging]: design/packaging.md
[release]: https://github.com/daphnediane/container-distro/releases

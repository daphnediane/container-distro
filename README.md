# container-distro (`cm`)

A [WSL]-compatible command line for Apple's [`container`], in two
parts:

- **`cm`** -- `wsl.exe`-style commands over [container machines]
  *and* distros, as one namespace.
- **`container distro`** -- a `container` plugin (and library) that
  creates **distros**: machine-like containers (your account
  provisioned inside, home shared at the same path, the image's init
  as PID 1) plus what machines lack -- host directories outside
  `$HOME`, published ports, export/import, and `set` for changing
  settings after creation.

> **A machine or distro is not a sandbox.** It is a second login
> session for your account: `$HOME` is shared read-write by default,
> your SSH agent is forwarded in, and the provisioned user has
> passwordless sudo. Treat guest code as code running on the host as
> you -- see [Security](docs/security.md).

## Requirements

- **macOS 26 or later** on Apple silicon -- the floor for Apple's
  `container` itself.
- Apple's [`container`] CLI. Built and verified against **1.5.x–1.6.x**; if
  it's missing, an interactive `cm`/`container-distro` offers to
  download the signed installer (SHA-256-pinned;
  `CONTAINER_DISTRO_NO_INSTALL_OFFER=1` opts out). Newer releases run
  but may warn on internals we verify per release.
- A current Rust toolchain (`rustup` recommended) -- only for building
  from source. Releases also ship a macOS `.pkg`
  (`sudo installer -pkg container-distro-<version>.pkg -target /`) and a
  `cargo-binstall` tarball (`cargo binstall container-distro`).

## Quick start

The release `.pkg` is the easiest install -- signed, no toolchain,
and it lays down everything: the binary, the `cm` alias, the plugin registration,
man pages, completions, and the update/uninstall scripts. Download
`container-distro-<version>.pkg` from the [latest release] and
double-click it (`sudo installer -pkg` from a shell does the same --
[docs/install.md](docs/install.md) has the details). Prereleases might
have an `-unsigned` build, either wait for the signed release or you
might need to right-click → Open to install.

`cargo install` and `cargo binstall` work too, followed by
`install-all` for everything around the binary:

```bash
cargo install --locked container-distro
container-distro install-all   # cm alias, man pages, completions, plugin
```

Then:

```bash
cm --install        # install the catalog default (Ubuntu), drop into a shell
cm -l -v            # list machines and distros, WSL-style
cm -e uname -a      # run a command
cm                  # login shell in the default
```

`cm` is a small alias script for `container-distro cli` — the pkg
and `install-all` both install it ([docs/install.md](docs/install.md)
has the details). Optional either way:

```bash
cm --install-alias wsl          # alias `cm` as `wsl`
```

If `container` services aren't running, `cm` runs
`container system start` first -- matching WSL's transparent start.

## Verifying downloads

Signed release builds sign the pkg, the binaries, and the release
tarball -- the tarball gets a detached code signature
(`.tar.gz.sig`) and a `gpg --detach-sign` signature (`.tar.gz.asc`):

| Artifact                     | Check                                            |
| ---------------------------- | ------------------------------------------------ |
| `.pkg`                       | `pkgutil --check-signature <file>.pkg`           |
| `*.tar.gz` (code signature)  | `codesign --verify --detached=<file>.sig <file>` |
| `*.tar.gz` (GPG)             | `gpg --verify <file>.asc <file>`                 |
| extracted `container-distro` | `codesign -dv <path>`                            |

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
  [packaging/release-pubkey.asc](packaging/release-pubkey.asc).

`cm --update` runs the pkg checks itself (digest, Developer ID, same
team) before installing.

## Upgrading

A `.pkg` install self-updates -- it downloads the latest (or a named)
release package, verifies it (release digest, Developer ID, same
signing team), and hands it to Installer.app:

```bash
cm --update                    # or: container distro update [VERSION]
/usr/local/bin/update-container-distro.sh [VERSION]   # same thing
```

Cargo installs update with `cargo install --locked container-distro`
(or `cargo binstall`) followed by `container-distro install-all` --
[docs/install.md](docs/install.md#upgrade). Nothing needs stopping
first. Apple's `container` itself updates separately via its own
`update-container.sh`.

## Uninstalling

```bash
/usr/local/bin/uninstall-container-distro.sh -k   # keep distro state
/usr/local/bin/uninstall-container-distro.sh -d   # delete it too (destroys distros)
```

Add `-n` to preview every removal first. Cargo installs run
`container-distro uninstall-all` before `cargo uninstall
container-distro` -- [docs/install.md](docs/install.md#uninstall).

## `cm` -- the WSL command line

`cm` maps `wsl.exe` argument semantics onto `container`: `-d` picks a
machine or distro, `-e` runs a command with exact argv, `--` goes
through the guest shell, `-l`/`-s`/`-t`/`--shutdown`/`--unregister`
manage them, and `--install`/`--export`/`--import`/`--rename`/
`--forward` cover the rest of the WSL surface. Full reference:
[docs/cm.md](docs/cm.md) (`man cm` after `--install-man`).

```bash
cm -d alpine                      # shell in the alpine machine/distro
cm -e uname -a                    # run a command (exact argv)
cm -- ls -la                      # via the guest shell
cm --install ubuntu:24.04 --name work --share /Volumes/Code:/mnt/code --publish 3000
echo hi | cm -- cat               # stdin is piped through
cm -t alpine                      # stop it
```

Where `cm` deliberately differs from real `wsl.exe`: [docs/cm-vs-wsl.md].

## `container distro` -- what machines lack

`container machine` can't share anything outside `$HOME`, has no
port-publishing API, can't export a filesystem directly, and fixes
nearly all settings at creation. A **distro** is a regular `container`
container configured the way the machine plugin configures a machine --
then extended:

```bash
container distro create -n dev -v /Volumes/Code:/mnt/code -p 3000 alpine:latest
container distro run -n dev -- uname -a
container distro set dev --memory 8G --add-volume /opt/x:/mnt/x
container distro export dev -o dev.tar
container distro rm -f dev
```

`cm` sees distros and machines as one set of WSL distributions; the
plugin install is optional since `cm` links the distro library
directly. Choosing between the two backends:
[docs/machine-vs-distro.md]. Full subcommand reference:
[docs/container-distro.md](docs/container-distro.md) (`man
container-distro`).

## Notes

- `container machine run` (and so `cm`) requires a TTY for interactive
  shells.
- `container` is resolved from `/usr/local/bin/container`, then
  `PATH`; `CONTAINER_CLI` overrides that under `--dev` (values naming
  the default resolution never need it) -- a dev-mode seam, see
  [DEV.md](DEV.md).
- Some features reach into `container` internals that may change on
  newer releases -- a warning appears on unverified versions.
- Distro names are lowercase DNS-style (`[a-z0-9-]`).
- Open work and planned features: [docs/work/](docs/work/). The
  rest of the docs live in [docs/](docs/README.md).

## Documentation

| Document                                               | Contents                                                       |
| ------------------------------------------------------ | -------------------------------------------------------------- |
| [docs/install.md](docs/install.md)                     | Build/install/uninstall in full detail                         |
| [DEV.md](DEV.md)                                       | Working in this repo: build, test, scripts, packaging, release |
| [docs/cm.md](docs/cm.md)                               | `cm` command reference                                         |
| [docs/container-distro.md](docs/container-distro.md)   | `container distro` subcommand reference                        |
| [docs/cm-vs-wsl.md](docs/cm-vs-wsl.md)                 | Behavioral differences from `wsl.exe`                          |
| [docs/machine-vs-distro.md](docs/machine-vs-distro.md) | `container machine` vs `container distro` -- which to use      |
| [docs/security.md](docs/security.md)                   | Security model: not a sandbox, `--restricted`, scoping down    |
| [SECURITY.md](SECURITY.md)                             | Reporting vulnerabilities                                      |
| [docs/work/](docs/work/)                               | Roadmap and open items                                         |
| [docs/design/](docs/design/)                           | Architecture and design documents                              |

## License

[BSD-2-Clause License].

Dependencies are permissively licensed (MIT / Apache-2.0 / BSD /
Unicode-3.0; policy enforced by `deny.toml`). Their copyright and
license texts are collected in THIRD-PARTY-NOTICES.yaml -- ship that
file with any prebuilt binaries.

The built-in distro catalog incorporates Microsoft's
`DistributionInfo.json` from the [WSL] repository (MIT License,
(c) Microsoft Corporation). The vendored copy and its license live in
`vendor/microsoft/`; installers ship the license text as
`LICENSE-microsoft-wsl`.

## AI Coding Declaration

Development of this project has been assisted by AI coding tools:

- [Devin]
- [Claude Code]

Most of the code and documentation here was AI-generated and manually
reviewed. Take the documentation as a slightly out-of-date roadmap, no
matter the polish of the AI verbiage. Here be dragons.

[`container`]: https://github.com/apple/container
[BSD-2-Clause License]: LICENSE
[Claude Code]: https://claude.ai/code
[container machines]: https://github.com/apple/container/blob/main/docs/container-machine.md
[Devin]: https://www.devin.ai/
[docs/cm-vs-wsl.md]: docs/cm-vs-wsl.md
[docs/machine-vs-distro.md]: docs/machine-vs-distro.md
[latest release]: https://github.com/daphnediane/container-distro/releases/latest
[WSL]: https://github.com/microsoft/WSL

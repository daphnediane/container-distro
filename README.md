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
- Apple's [`container`] CLI. Built and verified against **1.5.0**; if
  it's missing, an interactive `cm`/`container-distro` offers to
  download the signed installer (SHA-256-pinned;
  `CONTAINER_DISTRO_NO_INSTALL_OFFER=1` opts out). Newer releases run
  but may warn on internals we verify per release.
- A current Rust toolchain (`rustup` recommended) -- `cargo install`
  is the only packaged install path today.

## Quick start

```bash
cargo install --locked container-distro   # one crate, both binaries

cm --install        # install the catalog default (Ubuntu), drop into a shell
cm -l -v            # list machines and distros, WSL-style
cm -e uname -a      # run a command
cm                  # login shell in the default
```

Optional extras:

```bash
cm --install-man && container-distro install-man   # man pages
cm --install-completions                            # bash/zsh/fish completions
cm --install-alias wsl                              # alias `cm` as `wsl`
container-distro install-plugin                     # the `container distro` subcommand
```

If `container` services aren't running, `cm` runs
`container system start` first -- matching WSL's transparent start.

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
  `PATH`; set `CONTAINER_CLI` to override.
- Some features reach into `container` internals that may change on
  newer releases -- a warning appears on unverified versions.
- Distro names are lowercase DNS-style (`[a-z0-9-]`).
- Open work and planned features: [docs/TODO.md](docs/TODO.md). The
  rest of the docs live in [docs/](docs/README.md).

## Documentation

| Document                                               | Contents                                                    |
| ------------------------------------------------------ | ----------------------------------------------------------- |
| [docs/install.md](docs/install.md)                     | Build/install/uninstall in full detail                      |
| [docs/cm.md](docs/cm.md)                               | `cm` command reference                                      |
| [docs/container-distro.md](docs/container-distro.md)   | `container distro` subcommand reference                     |
| [docs/cm-vs-wsl.md](docs/cm-vs-wsl.md)                 | Behavioral differences from `wsl.exe`                       |
| [docs/machine-vs-distro.md](docs/machine-vs-distro.md) | `container machine` vs `container distro` -- which to use   |
| [docs/security.md](docs/security.md)                   | Security model: not a sandbox, `--restricted`, scoping down |
| [SECURITY.md](SECURITY.md)                             | Reporting vulnerabilities (draft, pre-1.0)                  |
| [docs/TODO.md](docs/TODO.md)                           | Roadmap and open items                                      |
| [docs/design/](docs/design/)                           | Architecture and design documents                           |

## License

[BSD-2-Clause License].

Dependencies are permissively licensed (MIT / Apache-2.0 / BSD /
Unicode-3.0; policy enforced by `deny.toml`). Their copyright and
license texts are collected in THIRD-PARTY-NOTICES.yaml -- ship that
file with any prebuilt binaries.

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
[WSL]: https://github.com/microsoft/WSL

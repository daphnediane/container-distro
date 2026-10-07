# container-distro docs

Guides first, then design notes and research documents for `cm`. The
design docs are working documents -- they describe options and
tradeoffs, not commitments.

## Guides

| Document           | Contents                                                             |
| ------------------ | -------------------------------------------------------------------- |
| [install]          | Building, `cargo install`, plugin registration, man pages, uninstall |
| [cm usage]         | `cm` command-line reference -- the WSL-compatible front end          |
| [container distro] | `container distro`/`container-distro` subcommand reference           |
| [TODO]             | Central work list -- open gaps, planned features, pre-1.0 checklist  |
| [release]          | Release process -- versioning, `vX.Y.Z` tags, `release/X.Y` branches |

## Design notes

| Document                  | Contents                                                                                                                                                                                                    |
| ------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [security]                | Threat model, accepted trust trade-offs (each tagged for re-evaluation), the pre-1.0 finding list, and risks each gap-closing feature adds                                                                  |
| [lower-level-integration] | Can `cm` bypass the `container` CLI -- the XPC/Swift-library/Containerization/Virtualization.framework layers, Rust↔Swift interop options, and whether a Swift rewrite is worthwhile                        |
| [remote-wsl-interop]      | What Remote-WSL-style editor extensions (`ms-vscode-remote.remote-wsl`, `codeium.windsurf-remote-wsl`, `open-remote-wsl`) need from `wsl.exe`, and the fake-shim / forked-resolver / sshd paths to interop  |
| [container-internals]     | Inventory of every `container`/`container machine` internal we rely on -- CLI/JSON surfaces, observed behaviors, the `appRoot` on-disk layout, plugin discovery -- and what breaks if upstream changes them |

## Feature gaps vs WSL

One document per gap, each noting the integration level (see the rung
definitions in [lower-level-integration][lower-level-integration-2])
that would fix it:

| Gap                                        | Cheapest fix                            |
| ------------------------------------------ | --------------------------------------- |
| [Mounts outside `$HOME`]                   | distros (L2.5, done); machines upstream |
| [Localhost port forwarding]                | `cm --forward`, distro `-p` (done)      |
| [`--export` / `--import`]                  | done (L0 workaround for machines)       |
| [Configuration files]                      | L0 host config; guest `wsl.conf` harder |
| [`-v` verbose list]                        | done (L0)                               |
| [Subprocess overhead / JSON scraping]      | L2 if it ever hurts                     |
| [Service startup / `--shutdown` semantics] | L0 -- "stop what we started"            |
| [`machine run` exec stdio loss]            | L0 `-i` + quoting done; race upstream   |
| [GUI apps (X11/Wayland, WSLg)]             | L0 -- host X server + env plumbing      |

Upstream references:

- [apple/container] -- the `container` CLI and its `container-apiserver`
  XPC services (`container machine` lives here)
- [apple/containerization] -- the lower-level Swift package for OCI
  images, lightweight VMs, and `vminitd`

[`--export` / `--import`]: gaps/export-import.md
[`-v` verbose list]: gaps/verbose-list.md
[`machine run` exec stdio loss]: gaps/exec-stdio.md
[apple/container]: https://github.com/apple/container
[apple/containerization]: https://github.com/apple/containerization
[cm usage]: cm.md
[Configuration files]: gaps/configuration-files.md
[container distro]: container-distro.md
[container-internals]: container-internals.md
[GUI apps (X11/Wayland, WSLg)]: gaps/gui-apps.md
[install]: install.md
[Localhost port forwarding]: gaps/port-forwarding.md
[lower-level-integration]: lower-level-integration.md
[lower-level-integration-2]: lower-level-integration.md#rung-by-rung-how-low-can-cm-go
[Mounts outside `$HOME`]: gaps/mounts-outside-home.md
[release]: release.md
[remote-wsl-interop]: remote-wsl-interop.md
[security]: security.md
[Service startup / `--shutdown` semantics]: gaps/service-startup.md
[Subprocess overhead / JSON scraping]: gaps/subprocess-overhead.md
[TODO]: TODO.md

# container-distro docs

Guides first, then design notes and research documents for `cm`. The
design docs are working documents — they describe options and tradeoffs,
not commitments.

## Guides

| Document                                | Contents                                                             |
| --------------------------------------- | -------------------------------------------------------------------- |
| [install](install.md)                   | Building, `cargo install`, plugin registration, man pages, uninstall |
| [cm usage](cm.md)                       | `cm` command-line reference — the WSL-compatible front end           |
| [container distro](container-distro.md) | `container distro`/`container-distro` subcommand reference           |
| [TODO](TODO.md)                         | Central work list — open gaps, planned features, pre-1.0 checklist   |

## Design notes

| Document                                              | Contents                                                                                                                                                                                                   |
| ----------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [security](security.md)                               | Threat model, accepted trust trade-offs (each tagged for re-evaluation), the pre-1.0 finding list, and risks each gap-closing feature adds                                                                 |
| [lower-level-integration](lower-level-integration.md) | Can `cm` bypass the `container` CLI — the XPC/Swift-library/Containerization/Virtualization.framework layers, Rust↔Swift interop options, and whether a Swift rewrite is worthwhile                        |
| [remote-wsl-interop](remote-wsl-interop.md)           | What Remote-WSL-style editor extensions (`ms-vscode-remote.remote-wsl`, `codeium.windsurf-remote-wsl`, `open-remote-wsl`) need from `wsl.exe`, and the fake-shim / forked-resolver / sshd paths to interop |
| [container-internals](container-internals.md)         | Inventory of every `container`/`container machine` internal we rely on — CLI/JSON surfaces, observed behaviors, the `appRoot` on-disk layout, plugin discovery — and what breaks if upstream changes them  |

## Feature gaps vs WSL

One document per gap, each noting the integration level (see the rung
definitions in [lower-level-integration](lower-level-integration.md#rung-by-rung-how-low-can-cm-go))
that would fix it:

| Gap                                                                 | Cheapest fix                             |
| ------------------------------------------------------------------- | ---------------------------------------- |
| [Mounts outside `$HOME`](gaps/mounts-outside-home.md)               | distros (L2.5, done); machines upstream  |
| [Localhost port forwarding](gaps/port-forwarding.md)                | `cm --forward`, distro `-p` (done)       |
| [`--export` / `--import`](gaps/export-import.md)                    | distros done; machines punted (upstream) |
| [Configuration files](gaps/configuration-files.md)                  | L0 host config; guest `wsl.conf` harder  |
| [`-v` verbose list](gaps/verbose-list.md)                           | done (L0)                                |
| [Subprocess overhead / JSON scraping](gaps/subprocess-overhead.md)  | L2 if it ever hurts                      |
| [Service startup / `--shutdown` semantics](gaps/service-startup.md) | L0 — "stop what we started"              |
| [`machine run` exec stdio loss](gaps/exec-stdio.md)                 | L0 `-i` + quoting done; race upstream    |
| [GUI apps (X11/Wayland, WSLg)](gaps/gui-apps.md)                    | L0 — host X server + env plumbing        |

Upstream references:

- [apple/container](https://github.com/apple/container) — the `container` CLI
  and its `container-apiserver` XPC services (`container machine` lives here)
- [apple/containerization](https://github.com/apple/containerization) — the
  lower-level Swift package for OCI images, lightweight VMs, and `vminitd`

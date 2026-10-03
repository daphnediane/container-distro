# wsl-compat docs

Design notes and research documents for `cm`. These are working documents —
they describe options and tradeoffs, not commitments.

| Document                                              | Contents                                                                                                                                                                                                   |
| ----------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [lower-level-integration](lower-level-integration.md) | Can `cm` bypass the `container` CLI — the XPC/Swift-library/Containerization/Virtualization.framework layers, Rust↔Swift interop options, and whether a Swift rewrite is worthwhile                        |
| [remote-wsl-interop](remote-wsl-interop.md)           | What Remote-WSL-style editor extensions (`ms-vscode-remote.remote-wsl`, `codeium.windsurf-remote-wsl`, `open-remote-wsl`) need from `wsl.exe`, and the fake-shim / forked-resolver / sshd paths to interop |

## Feature gaps vs WSL

One document per gap, each noting the integration level (see the rung
definitions in [lower-level-integration](lower-level-integration.md#rung-by-rung-how-low-can-cm-go))
that would fix it:

| Gap                                                                 | Cheapest fix                            |
| ------------------------------------------------------------------- | --------------------------------------- |
| [Mounts outside `$HOME`](gaps/mounts-outside-home.md)               | upstream PR, else L2.5 plugin           |
| [Localhost port forwarding](gaps/port-forwarding.md)                | L0 forwarder subcommand                 |
| [`--export` / `--import`](gaps/export-import.md)                    | punted — upstream export bug            |
| [Configuration files](gaps/configuration-files.md)                  | L0 host config; guest `wsl.conf` harder |
| [`-v` verbose list](gaps/verbose-list.md)                           | L0 — `machine inspect` JSON             |
| [Subprocess overhead / JSON scraping](gaps/subprocess-overhead.md)  | L2 if it ever hurts                     |
| [Service startup / `--shutdown` semantics](gaps/service-startup.md) | L0 — "stop what we started"             |
| [`machine run` exec stdio loss](gaps/exec-stdio.md)                 | upstream fix; L0 `-i` + handshake       |

Upstream references:

- [apple/container](https://github.com/apple/container) — the `container` CLI
  and its `container-apiserver` XPC services (`container machine` lives here)
- [apple/containerization](https://github.com/apple/containerization) — the
  lower-level Swift package for OCI images, lightweight VMs, and `vminitd`

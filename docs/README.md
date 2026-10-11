# container-distro docs

User guides first; design and architecture docs live under [design/].

## User guides

| Document            | Contents                                                                          |
| ------------------- | --------------------------------------------------------------------------------- |
| [install]           | Building, `cargo install`, plugin registration, man pages, completions, uninstall |
| [cm]                | `cm` command-line reference -- the WSL-compatible front end                       |
| [container distro]  | `container distro`/`container-distro` subcommand reference                        |
| [cm vs wsl]         | Where `cm`'s behavior deliberately differs from `wsl.exe`                         |
| [machine vs distro] | Choosing between the `container machine` and `container distro` backends          |
| [security]          | Not a sandbox -- what's exposed by default, `--restricted`, and how to scope down |
| [TODO]              | Central work list -- open items, planned features, pre-1.0 checklist              |

## Design & architecture

Working documents under [design/] -- they describe decisions and
trade-offs, not commitments:

| Document                  | Contents                                                                                                                                                  |
| ------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [security register]       | Threat model register: accepted trade-offs (T1–T9) with re-evaluation triggers, review findings (C1–C12), risks of planned features, pre-1.0 checklist    |
| [upstream-limitations]    | Where `container`/`container machine` falls short, the workaround shipped for each, and the upstream fix that would retire it                             |
| [lower-level-integration] | The L0–L4 integration rungs below the CLI, why the architecture stays at L0 + a CLI plugin, and what would trigger a revisit                              |
| [container-internals]     | Every `container`/`container machine` internal we rely on -- CLI/JSON surfaces, `appRoot` layout, plugin discovery -- and what breaks if upstream changes |
| [remote-wsl-interop]      | What Remote-WSL-style editor extensions need from `wsl.exe`, and the fake-shim / forked-resolver / sshd paths to interop                                  |
| [naming]                  | Every persistent name, label, and path -- the rename checklist and migration strategy                                                                     |
| [release]                 | Release process -- versioning, `vX.Y.Z` tags, `release/X.Y` branches                                                                                      |
| [packaging]               | What releases ship (.pkg, binstall tarball), options considered and rejected, signing/notarization setup                                                  |
| [design/notes/]           | Per-item analyses for open work: configuration files, GUI apps, service-startup semantics, subprocess overhead                                            |

Upstream references:

- [apple/container] -- the `container` CLI and its `container-apiserver`
  XPC services (`container machine` lives here)
- [apple/containerization] -- the lower-level Swift package for OCI
  images, lightweight VMs, and `vminitd`

[apple/container]: https://github.com/apple/container
[apple/containerization]: https://github.com/apple/containerization
[cm vs wsl]: cm-vs-wsl.md
[cm]: cm.md
[container distro]: container-distro.md
[container-internals]: design/container-internals.md
[design/]: design/
[design/notes/]: design/notes/
[install]: install.md
[lower-level-integration]: design/lower-level-integration.md
[machine vs distro]: machine-vs-distro.md
[naming]: design/naming.md
[packaging]: design/packaging.md
[release]: design/release.md
[remote-wsl-interop]: design/remote-wsl-interop.md
[security register]: design/security.md
[security]: security.md
[TODO]: TODO.md
[upstream-limitations]: design/upstream-limitations.md

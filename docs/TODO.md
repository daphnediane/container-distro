# TODO

The working roadmap -- open work only. Each item links to the document
(or upstream issue) with the analysis; the linked docs are the source
of truth for details. "Rung" references are the integration levels in
[lower-level-integration]. `For 1.0`/`Post-1.0` splits mirror the
`release-1` milestone on the linked issues.

Items are plain bullets -- no `[ ]`/`[x]` checkboxes at the top level
(nested subitems may keep them for partial progress). When an item is
completed it leaves this file: the user-facing change is recorded in
the `X.Y.Z-dev` section of [CHANGELOG.md](../CHANGELOG.md), which is
the permanent record of what shipped.

## Before 0.8.0

- **Decide how `distrofs` ships** -- the guest file server behind
  `container distro fs` ([#27]) is a separate static
  aarch64-linux-musl build that `cargo install`/binstall users don't
  get today. Choose among: the `.pkg` payload (Phase 4 of
  [host-file-access]), a SHA-256-verified download of the matching
  release asset on first use (the self-update trust model — the pieces
  already exist: `distrofs fetch-servers` installs verified builds
  into `<prefix>/libexec/distrofs/`, which `fs` discovers), and/or
  a standalone GitHub release artifact. Whatever's chosen,
  `release.yml` and `build-pkg.sh` must produce it
  (`scripts/build-distrofs.sh`). distrofs's own packaging as a
  product — the crates.io decision, a stable `proto`/handshake
  contract, shipping its own `.pkg` — is tracked in distrofs's TODO
  against its 1.0 milestone.

## Machines (WSL parity, mostly upstream-blocked)

### Post-1.0 — machines

- **Configuration files** -- host-side `cm` config (L0) still
  open; the guest `wsl.conf` subset is honored for distros (init
  reads the live file at boot, mode-gated; snapshots in the journal)
  with deferred keys tracked separately ([#29], [#33];
  [configuration-files], design/security.md)
- **`cm --export` / `--import` via upstream primitives** -- once
  `container export` honors a machine's rootfs mount source, drop the
  scratch-container workaround; once a `--no-boot` machine can be
  booted by `machine run`, use it and drop the create-boot probe. Both
  bugs still need filing upstream ([#15]; [export-import])
- **Mounts outside `$HOME` for machines** -- upstream
  ([apple/container#1805], [apple/container#2278]); distros cover it
  today ([#3]; [mounts-outside-home])
- **`machine run` first-write drop** -- still open upstream; file an
  issue (closest: [apple/container#1148]) ([#4]; [exec-stdio])
- **Automatic localhost forwarding** -- WSL-style dynamic forwarding
  would supersede the experimental `cm --forward` ([#5];
  [port-forwarding])
- **GUI apps (WSLg-style)** -- open; the X11 cookie path needs care
  ([#7]; [gui-apps], security notes there)

## Interop

### Post-1.0 — interop

- **Remote-WSL editor extensions** -- decide between the
  fake-`wsl.exe` shim, a forked `open-remote-wsl` resolver, and in-guest
  `sshd`; open questions are in [remote-wsl-interop] ([#8])
- **Host access to distro filesystems (`\\wsl$`)** -- guest `distrofs`
  server over `--publish-socket`, FSKit `Distro Files.app` front-end;
  phased plan in [host-file-access] ([#27])
  - [x] Phase 1: protocol, guest server, host client, `files` spec
    field, `container distro fs` CLI, T13
  - [ ] Phase 2: FSKit spike, app + module + agent, signing
  - [ ] Phase 3: inotify events → `DataCacheHandler` invalidation
  - [ ] Phase 4: `.pkg` payload (build `distrofs` in release.yml /
    `build-pkg.sh`), `cm --mount`, `files` in `list -v`/`info`
  - [ ] Later: machines via exec stdio; `/mnt/wsl`-style
    distro-to-distro mounts

## Plumbing / internals

### Post-1.0 — plumbing

- **Launcher entries** -- `[shortcut]` metadata's Start-menu
  equivalent ([#33]; [configuration-files])
- **`ProfileTemplate` + macOS profile styling** -- map the WT
  fragment to Terminal.app/iTerm2 fields, and/or native
  `[macosterminal]`/`[iterm2]` extension sections ([#34];
  [configuration-files])
- **Locate our own commands through `layout`** -- `terminal.rs`'s
  `cm_path` (sibling `cm`, symlinks unresolved, baked into terminal
  profiles) and `cm`'s alias `our_exe` (canonicalized, hard error)
  each find binaries their own way. Add `layout` helpers for `cm` and
  `container-distro`: `current_exe()` when we *are* that command, the
  install-prefix sibling otherwise, PATH as the last resort — with one
  documented symlink policy (resolve, or deliberately keep the
  user-facing path in generated profiles). Catch: the `container
  distro` plugin can be installed from a user copy (`install-plugin
  --from`) with no `cm` beside it, so the plugin can't count on a
  sibling. Consider folding `cm` into the plugin binary as a
  subcommand (e.g. `container distro cli …`), with `cm` reduced to a
  thin launcher that `exec`s `<path-to-container-distro> cli "$@"`
  (a script or a tiny binary — `cm --install-alias` and terminal
  profiles must keep working, and `wsl`-style aliases need argv[0]
  handling)
- **Subprocess overhead** -- one `container` spawn per op plus
  `system status` checks; only worth fixing (XPC/Swift client libraries,
  L1/L2) if it ever hurts ([#9]; [subprocess-overhead])

[#3]: https://github.com/daphnediane/container-distro/issues/3
[#4]: https://github.com/daphnediane/container-distro/issues/4
[#5]: https://github.com/daphnediane/container-distro/issues/5
[#7]: https://github.com/daphnediane/container-distro/issues/7
[#8]: https://github.com/daphnediane/container-distro/issues/8
[#9]: https://github.com/daphnediane/container-distro/issues/9
[#15]: https://github.com/daphnediane/container-distro/issues/15
[#27]: https://github.com/daphnediane/container-distro/issues/27
[#29]: https://github.com/daphnediane/container-distro/issues/29
[#33]: https://github.com/daphnediane/container-distro/issues/33
[#34]: https://github.com/daphnediane/container-distro/issues/34
[apple/container#1148]: https://github.com/apple/container/issues/1148
[apple/container#1805]: https://github.com/apple/container/issues/1805
[apple/container#2278]: https://github.com/apple/container/issues/2278
[configuration-files]: design/notes/configuration-files.md
[exec-stdio]: design/upstream-limitations.md#machine-run-exec-fidelity-stdin-argv-first-write
[export-import]: design/upstream-limitations.md
[gui-apps]: design/notes/gui-apps.md
[host-file-access]: design/host-file-access.md
[mounts-outside-home]: design/upstream-limitations.md#no-mounts-outside-home-for-machines
[port-forwarding]: design/upstream-limitations.md#no-localhost-port-forwarding-for-machines
[subprocess-overhead]: design/notes/subprocess-overhead.md
[lower-level-integration]: design/lower-level-integration.md#rung-by-rung-how-low-can-cm-go
[remote-wsl-interop]: design/remote-wsl-interop.md

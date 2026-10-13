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

## Plumbing / internals

### Post-1.0 — plumbing

- **Launcher entries** -- `[shortcut]` metadata's Start-menu
  equivalent ([#33]; [configuration-files])
- **`ProfileTemplate` + macOS profile styling** -- map the WT
  fragment to Terminal.app/iTerm2 fields, and/or native
  `[macosterminal]`/`[iterm2]` extension sections ([#34];
  [configuration-files])
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
[mounts-outside-home]: design/upstream-limitations.md#no-mounts-outside-home-for-machines
[port-forwarding]: design/upstream-limitations.md#no-localhost-port-forwarding-for-machines
[subprocess-overhead]: design/notes/subprocess-overhead.md
[lower-level-integration]: design/lower-level-integration.md#rung-by-rung-how-low-can-cm-go
[remote-wsl-interop]: design/remote-wsl-interop.md

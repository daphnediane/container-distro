# TODO

The working roadmap. Each item links to the document (or upstream issue)
with the analysis; statuses here lag reality -- the linked docs are the
source of truth. "Rung" references are the integration levels in
[lower-level-integration]. `For 1.0`/`Post-1.0` splits mirror the
`release-1` milestone on the linked issues.

## Machines (WSL parity, mostly upstream-blocked)

### For 1.0 — machines

- [ ] **Configuration files** -- host-side `cm` config (L0) still
  open; the guest `wsl.conf` subset is honored for distros (init
  reads the live file at boot, mode-gated; snapshots in the journal)
  with deferred keys tracked separately ([#6], [#29], [#30], [#33];
  [configuration-files], design/security.md)

### Post-1.0 — machines

- [ ] **`cm --export` / `--import` via upstream primitives** -- once
  `container export` honors a machine's rootfs mount source, drop the
  scratch-container workaround; once a `--no-boot` machine can be
  booted by `machine run`, use it and drop the create-boot probe. Both
  bugs still need filing upstream ([#15]; [export-import])
- [ ] **Mounts outside `$HOME` for machines** -- upstream
  ([apple/container#1805], [apple/container#2278]); distros cover it
  today ([#3]; [mounts-outside-home])
- [ ] **`machine run` first-write drop** -- still open upstream; file an
  issue (closest: [apple/container#1148]) ([#4]; [exec-stdio])
- [ ] **Automatic localhost forwarding** -- WSL-style dynamic forwarding
  would supersede the experimental `cm --forward` ([#5];
  [port-forwarding])
- [ ] **GUI apps (WSLg-style)** -- open; the X11 cookie path needs care
  ([#7]; [gui-apps], security notes there)

## Catalog

### For 1.0 — catalog

- [x] **Vendor Microsoft's `DistributionInfo.json` as the catalog
  base** -- pinned copy in `vendor/microsoft/` merged with the
  `src/catalog.json` overlay (upstream fields win, overlay fills
  gaps); `Entry::source` prefers `.wsl` and falls back to `Image`;
  `check-catalog-freshness.sh` in the release gate; MIT license
  shipped in the pkg ([#31])
- [x] **Content-keyed image reuse on import** -- shared
  `local/rootfs:<sha256>` dedup tag + per-importer
  `local/{distro,machine}-<name>:imported-<sha>` tags; `rm`/
  `--unregister` untag per-name refs, unused shared tags evicted
  oldest-first past a size budget ([#32])

## Interop

### Post-1.0 — interop

- [ ] **Remote-WSL editor extensions** -- decide between the
  fake-`wsl.exe` shim, a forked `open-remote-wsl` resolver, and in-guest
  `sshd`; open questions are in [remote-wsl-interop] ([#8])

## Plumbing / internals

### For 1.0 — plumbing

- [x] **Service startup semantics** -- `ensure_started` leaves a
  `services-started` marker when it ran `system start`;
  `cm --shutdown` now `system stop`s when the marker exists and no
  machines/containers remain running ("stop what we started");
  `--shutdown --system` stays the explicit big hammer ([#11];
  [service-startup])
- [x] **macOS terminal profiles** -- `windowsterminal.enabled` (default
  true) generates a Terminal.app settings set + iTerm2 dynamic profile
  launching `cm -d NAME`; refreshed on rename, removed on delete/
  `migrate --out`; `ProfileTemplate` styling is #34 ([#30];
  [configuration-files])

### Post-1.0 — plumbing

- [ ] **Launcher entries** -- `[shortcut]` metadata's Start-menu
  equivalent ([#33]; [configuration-files])
- [ ] **`ProfileTemplate` + macOS profile styling** -- map the WT
  fragment to Terminal.app/iTerm2 fields, and/or native
  `[macosterminal]`/`[iterm2]` extension sections ([#34];
  [configuration-files])
- [ ] **Subprocess overhead** -- one `container` spawn per op plus
  `system status` checks; only worth fixing (XPC/Swift client libraries,
  L1/L2) if it ever hurts ([#9]; [subprocess-overhead])

[#3]: https://github.com/daphnediane/container-distro/issues/3
[#4]: https://github.com/daphnediane/container-distro/issues/4
[#5]: https://github.com/daphnediane/container-distro/issues/5
[#6]: https://github.com/daphnediane/container-distro/issues/6
[#7]: https://github.com/daphnediane/container-distro/issues/7
[#8]: https://github.com/daphnediane/container-distro/issues/8
[#9]: https://github.com/daphnediane/container-distro/issues/9
[#11]: https://github.com/daphnediane/container-distro/issues/11
[#15]: https://github.com/daphnediane/container-distro/issues/15
[#29]: https://github.com/daphnediane/container-distro/issues/29
[#30]: https://github.com/daphnediane/container-distro/issues/30
[#31]: https://github.com/daphnediane/container-distro/issues/31
[#32]: https://github.com/daphnediane/container-distro/issues/32
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
[service-startup]: design/notes/service-startup.md
[subprocess-overhead]: design/notes/subprocess-overhead.md
[lower-level-integration]: design/lower-level-integration.md#rung-by-rung-how-low-can-cm-go
[remote-wsl-interop]: design/remote-wsl-interop.md

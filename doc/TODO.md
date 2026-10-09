# TODO

The working roadmap. Each item links to the document (or upstream issue)
with the analysis; statuses here lag reality -- the linked docs are the
source of truth. "Rung" references are the integration levels in
[lower-level-integration]. `For 1.0`/`Post-1.0` splits mirror the
`release-1` milestone on the linked issues.

## Distros (`container distro`)

### For 1.0 — distros

- [ ] **Make distros the `cm` default; add `--machine`** -- flip
  `--install`/create to build a distro unless `--machine` is given
  (today `--distro` opts in). Distro-first is also the safer default:
  only distros support `--restricted`, and it's the path we control.
  Keep `--machine` for WSL-parity testing and upstream comparisons.
  Gated on adopting the machine model's anti-persistence defenses for
  distros (Security & hardening below, [#18]) -- don't make the
  less-regenerated backend the default until it matches ([#21])

## Machines (WSL parity, mostly upstream-blocked)

### For 1.0 — machines

- [ ] **Configuration files** -- host-side `cm` config (L0); guest
  `wsl.conf` subset is harder and has a confused-deputy trap to design
  around ([#6]; [gaps/configuration-files], security.md)

### Post-1.0 — machines

- [ ] **`cm --export` / `--import` via upstream primitives** -- once
  `container export` honors a machine's rootfs mount source, drop the
  scratch-container workaround; once a `--no-boot` machine can be
  booted by `machine run`, use it and drop the create-boot probe. Both
  bugs still need filing upstream ([#15]; [gaps/export-import])
- [ ] **Mounts outside `$HOME` for machines** -- upstream
  ([apple/container#1805], [apple/container#2278]); distros cover it
  today ([#3]; [gaps/mounts-outside-home])
- [ ] **`machine run` first-write drop** -- still open upstream; file an
  issue (closest: [apple/container#1148]) ([#4]; [gaps/exec-stdio])
- [ ] **Automatic localhost forwarding** -- WSL-style dynamic forwarding
  would supersede the experimental `cm --forward` ([#5];
  [gaps/port-forwarding])
- [ ] **GUI apps (WSLg-style)** -- open; the X11 cookie path needs care
  ([#7]; [gaps/gui-apps], security notes there)

## Interop

### Post-1.0 — interop

- [ ] **Remote-WSL editor extensions** -- decide between the
  fake-`wsl.exe` shim, a forked `open-remote-wsl` resolver, and in-guest
  `sshd`; open questions are in [remote-wsl-interop] ([#8])

## Plumbing / internals

### Post-1.0 — plumbing

- [ ] **Subprocess overhead** -- one `container` spawn per op plus
  `system status` checks; only worth fixing (XPC/Swift client libraries,
  L1/L2) if it ever hurts ([#9]; [gaps/subprocess-overhead])
- [ ] **Service startup semantics** -- `cm --shutdown` stops machines
  but leaves `container` services up by design; revisit only if a
  narrower "stop what we started" primitive appears upstream ([#11];
  [gaps/service-startup])

## Security & hardening

From [security.md's checklist]:

### For 1.0 — security

- [ ] Nudge semi-trusted workloads off rw home shares -- an rw-home
  guest can tamper with runtime-managed state that *other* containers
  rely on, not just its own files (security.md T1). Options: a
  create-time hint when registering non-`--restricted` distros, docs
  recommending `home-mount ro` as the default posture for generated or
  imported images, or a scoped-subdir share instead of all of
  `$HOME` ([#19])
- [ ] **Adopt the machine model's anti-persistence defenses for
  distros** -- `container machine` gives each boot a randomized
  backing-container id and regenerates its runtime config, so guest
  edits don't persist; distros use stable names. Consider randomized
  backing-container names + recreate-per-boot, or regenerate/verify
  the backing container's recorded config at `boot`/`start`, so
  guest tampering can't outlive a restart ([#18])
- [ ] **Integrity-check distro-visible state at start** -- beyond the
  uchg-locked assets, consider recording what `cm` last wrote for a
  distro (mounts, resources, ssh/sudo posture) and warning on drift
  at `boot`/`start` -- turns silent tampering into a loud diff
  ([#20])

## Docs & tooling

### For 1.0 — docs & tooling

- [ ] Packages -- no brew/nix/pkg installers; `cargo install` +
  `install-plugin` is the only path ([#14]; [install])

[#3]: https://github.com/daphnediane/container-distro/issues/3
[#4]: https://github.com/daphnediane/container-distro/issues/4
[#5]: https://github.com/daphnediane/container-distro/issues/5
[#6]: https://github.com/daphnediane/container-distro/issues/6
[#7]: https://github.com/daphnediane/container-distro/issues/7
[#8]: https://github.com/daphnediane/container-distro/issues/8
[#9]: https://github.com/daphnediane/container-distro/issues/9
[#11]: https://github.com/daphnediane/container-distro/issues/11
[#14]: https://github.com/daphnediane/container-distro/issues/14
[#15]: https://github.com/daphnediane/container-distro/issues/15
[#18]: https://github.com/daphnediane/container-distro/issues/18
[#19]: https://github.com/daphnediane/container-distro/issues/19
[#20]: https://github.com/daphnediane/container-distro/issues/20
[#21]: https://github.com/daphnediane/container-distro/issues/21
[apple/container#1148]: https://github.com/apple/container/issues/1148
[apple/container#1805]: https://github.com/apple/container/issues/1805
[apple/container#2278]: https://github.com/apple/container/issues/2278
[gaps/configuration-files]: gaps/configuration-files.md
[gaps/exec-stdio]: gaps/exec-stdio.md
[gaps/export-import]: gaps/export-import.md
[gaps/gui-apps]: gaps/gui-apps.md
[gaps/mounts-outside-home]: gaps/mounts-outside-home.md
[gaps/port-forwarding]: gaps/port-forwarding.md
[gaps/service-startup]: gaps/service-startup.md
[gaps/subprocess-overhead]: gaps/subprocess-overhead.md
[install]: install.md
[lower-level-integration]: lower-level-integration.md#rung-by-rung-how-low-can-cm-go
[remote-wsl-interop]: remote-wsl-interop.md
[security.md's checklist]: security.md#pre-10-checklist

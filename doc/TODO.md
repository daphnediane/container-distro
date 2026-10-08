# TODO

The working roadmap. Each item links to the document (or upstream issue)
with the analysis; statuses here lag reality -- the linked docs are the
source of truth. "Rung" references are the integration levels in
[lower-level-integration].

## Distros (`container distro`)

- [x] **Distro → machine migration** -- `migrate --out` clones the
  distro's rootfs into a newly created `container machine`; `--in`
  selects the default direction explicitly ([#1])

## Machines (WSL parity, mostly upstream-blocked)

- [x] **`cm --export` / `--import` workarounds** -- machine export
  clones the rootfs into a scratch regular container for
  `container export`; import OCI-wraps the tar and `machine create`s
  it, retrying the generic first-boot flake. `--distro` imports a
  distro ([#2]; [gaps/export-import])
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
- [ ] **Configuration files** -- host-side `cm` config (L0); guest
  `wsl.conf` subset is harder and has a confused-deputy trap to design
  around ([#6]; [gaps/configuration-files], security.md)
- [ ] **GUI apps (WSLg-style)** -- open; the X11 cookie path needs care
  ([#7]; [gaps/gui-apps], security notes there)

## Interop

- [x] **Distro catalog (`--list --online`)** -- curated catalog in a
  `DistributionInfo.json` superset (Microsoft's manifest parses via
  `--catalog`), `--install` catalog-name/default resolution plus
  `--from-image` and `--from-file` (`.wsl` package import); `.wsl`
  catalog entries download (ureq + platform trust store, SHA-256
  verified) into the OS-managed content-addressed cache with per-hash
  flocking and provenance sidecars, listable via `--list --cache` and
  clearable via `--purge-cache`; covers the `wsl.exe --list --online`
  call surface in [remote-wsl-interop]
- [x] **`cm --list --online --verbose`** -- `-l -o -v` appends `LOCAL`
  (`pulled` image / `cached` `.wsl` / `downloading`) and `INSTANCES`
  (machines and distros installed from the entry, running marked);
  `-v -v` adds `SOURCE`/`REF`. Probes are best-effort and never
  block: cache state is read without flocking (in-flight fetches
  detected via `try_lock` on the `.lock` sidecar) and `container` is
  queried only when already running

- [ ] **Remote-WSL editor extensions** -- decide between the
  fake-`wsl.exe` shim, a forked `open-remote-wsl` resolver, and in-guest
  `sshd`; open questions are in [remote-wsl-interop] ([#8])

## Plumbing / internals

- [ ] **Subprocess overhead** -- one `container` spawn per op plus
  `system status` checks; only worth fixing (XPC/Swift client libraries,
  L1/L2) if it ever hurts ([#9]; [gaps/subprocess-overhead])
- [ ] **Service startup semantics** -- `cm --shutdown` stops machines
  but leaves `container` services up by design; revisit only if a
  narrower "stop what we started" primitive appears upstream ([#11];
  [gaps/service-startup])

## Security & hardening (pre-1.0)

From [security.md's checklist]:

- [ ] `cargo audit` / `cargo deny` in CI; fuzz the `FromStr` parsers
  (`MountSpec`, `PublishSpec`, `PortMapping`) ([#12])
- [x] Decide init-assets location vs. guest-writable shared home (T6)
  -- `install-plugin` writes `sbin.distro*/` next to the plugin binary
  (outside `$HOME`, root-owned for standard installs); `assets_dir`
  prefers that copy when byte-identical to the running build, else
  falls back ([#13])
- [x] `uchg` on guest-reachable state (beyond the init assets) -- the
  per-user fallback `sbin.distro*/` dirs and files, `default-distro`,
  and `preserved/` staged rootfs+journal are immutable-flagged
  (virtiofs exposes no flag ops, so guests get EPERM they can't undo);
  `preserved/` stays locked at rest so nothing can be planted for
  `recover_interrupted` to trust; refreshes write via `O_NOFOLLOW`
  ([#13], security.md T6)

## Docs & tooling

- [ ] Packages -- no brew/nix/pkg installers; `cargo install` +
  `install-plugin` is the only path ([#14]; [install])

[#1]: https://github.com/daphnediane/container-distro/issues/1
[#11]: https://github.com/daphnediane/container-distro/issues/11
[#12]: https://github.com/daphnediane/container-distro/issues/12
[#13]: https://github.com/daphnediane/container-distro/issues/13
[#14]: https://github.com/daphnediane/container-distro/issues/14
[#15]: https://github.com/daphnediane/container-distro/issues/15
[#2]: https://github.com/daphnediane/container-distro/issues/2
[apple/container#2278]: https://github.com/apple/container/issues/2278
[#3]: https://github.com/daphnediane/container-distro/issues/3
[#4]: https://github.com/daphnediane/container-distro/issues/4
[#5]: https://github.com/daphnediane/container-distro/issues/5
[#6]: https://github.com/daphnediane/container-distro/issues/6
[#7]: https://github.com/daphnediane/container-distro/issues/7
[#8]: https://github.com/daphnediane/container-distro/issues/8
[#9]: https://github.com/daphnediane/container-distro/issues/9
[apple/container#1148]: https://github.com/apple/container/issues/1148
[apple/container#1805]: https://github.com/apple/container/issues/1805
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

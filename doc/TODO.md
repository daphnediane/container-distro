# TODO

The working roadmap. Each item links to the document (or upstream issue)
with the analysis; statuses here lag reality — the linked docs are the
source of truth. "Rung" references are the integration levels in
[lower-level-integration](lower-level-integration.md#rung-by-rung-how-low-can-cm-go).

## Distros (`container distro`)

- [ ] **Distro → machine migration** — the reverse of `migrate`; planned
      as a separate subcommand (see `ops.rs::migrate` notes)
- [x] **Non-loopback `--publish` warning** — warns on create/set, and a
      boot-time report lists each published listener
      ([security.md](security.md#risks-introduced-by-gap-closing-work))
- [ ] **Automount refresh** — `--automount` resolves once at `create`;
      an operator command to re-scan `/Volumes` (`set --automount`?)
      would close the "attach volume → `set --add-volume`" chore

## Machines (WSL parity, mostly upstream-blocked)

- [ ] **`cm --export` / `--import`** — machine export is blocked upstream;
      prototype parked on `wip/export-import`
      ([gaps/export-import](gaps/export-import.md))
- [ ] **Mounts outside `$HOME` for machines** — upstream
      ([apple/container#1805](https://github.com/apple/container/issues/1805),
      [#2278](https://github.com/apple/container/issues/2278)); distros
      cover it today ([gaps/mounts-outside-home](gaps/mounts-outside-home.md))
- [ ] **`machine run` first-write drop** — still open upstream; file an
      issue (closest: [apple/container#1148](https://github.com/apple/container/issues/1148))
      ([gaps/exec-stdio](gaps/exec-stdio.md))
- [ ] **Automatic localhost forwarding** — WSL-style dynamic forwarding
      would supersede the experimental `cm --forward`
      ([gaps/port-forwarding](gaps/port-forwarding.md))
- [ ] **Configuration files** — host-side `cm` config (L0); guest
      `wsl.conf` subset is harder and has a confused-deputy trap to
      design around ([gaps/configuration-files](gaps/configuration-files.md),
      [security.md](security.md))
- [ ] **GUI apps (WSLg-style)** — open; the X11 cookie path needs care
      ([gaps/gui-apps](gaps/gui-apps.md), security notes there)

## Interop

- [ ] **Remote-WSL editor extensions** — decide between the fake-`wsl.exe`
      shim, a forked `open-remote-wsl` resolver, and in-guest `sshd`;
      open questions are in [remote-wsl-interop](remote-wsl-interop.md)

## Plumbing / internals

- [ ] **Subprocess overhead** — one `container` spawn per op plus
      `system status` checks; only worth fixing (XPC/Swift client
      libraries, L1/L2) if it ever hurts
      ([gaps/subprocess-overhead](gaps/subprocess-overhead.md))
- [ ] **Upstream-internal assumptions** — we read `appRoot` internals in
      a few paths, gated by `warn_unverified_version`; re-verify on each
      `container` minor release ([container-internals](container-internals.md))
- [ ] **Service startup semantics** — `cm --shutdown` stops machines but
      leaves `container` services up by design; revisit only if a
      narrower "stop what we started" primitive appears upstream
      ([gaps/service-startup](gaps/service-startup.md))

## Security & hardening (pre-1.0)

From [security.md's checklist](security.md#pre-10-checklist):

- [ ] `cargo audit` / `cargo deny` in CI; fuzz the `FromStr` parsers
      (`MountSpec`, `PublishSpec`, `PortMapping`)
- [ ] Decide init-assets location vs. guest-writable shared home (T6)
- [x] C8 — PID 1 zombie reaping in `assets/init` (low)
- [ ] C11 — `uninstall-plugin` TOCTOU / prefix cleanup (low)

## Docs & tooling

- [x] Man pages — `cm --install-man`, `container-distro install-man`
      (generated from the clap definitions, so they can't drift)
- [ ] Shell completions — not yet; `clap_complete` would slot in next to
      `clap_mangen` the same way
- [ ] Packages — no brew/nix/pkg installers; `cargo install` +
      `install-plugin` is the only path ([install](install.md))

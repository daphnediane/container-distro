# Decide how `distrofs` ships

- **Issue:** [#35]
- **Milestone:** `distrofs`

The guest file server behind `container distro fs` ([#27]) is a
separate static aarch64-linux-musl build that `cargo install`/binstall
users don't get today. Choose among: the `.pkg` payload (Phase 4 of
[host-file-access]), a SHA-256-verified download of the matching
release asset on first use (the self-update trust model -- the pieces
already exist: `distrofs fetch-servers` installs verified builds into
`<prefix>/libexec/distrofs/`, which `fs` discovers), and/or a
standalone GitHub release artifact. Whatever's chosen, `release.yml`
and `build-pkg.sh` must produce it (`scripts/build-distrofs.sh`).

distrofs's own packaging as a product -- the crates.io decision, a
stable `proto`/handshake contract, shipping its own `.pkg` -- is
tracked in the distrofs repo against its 1.0 milestone.

[#27]: https://github.com/daphnediane/container-distro/issues/27
[#35]: https://github.com/daphnediane/container-distro/issues/35
[host-file-access]: ../../../design/host-file-access.md

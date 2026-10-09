# Changelog

User-facing changes per release, newest first. Written at release time
from the commit log and the TODO items completed since the previous
tag -- see doc/release.md.

## 0.4.2 -- 2026-10-08

### Changed

- Packaging: the repository is now a single crate at the root —
  `cargo install --path .` installs **both** `cm` and
  `container-distro` (previously `cargo install --path crates/cm` plus
  `cargo install --path crates/container-distro`). The crate now
  carries crates.io publish metadata (repository, homepage,
  `rust-version`); no functional changes to either command.
- **Upgrading:** `cargo` refuses to overwrite a binary owned by
  another package, so a plain `cargo install --path .` will fail on
  the existing `cm` binary from the retired `cm` package. Use
  `--force` or uninstall it first:

  ```bash
  cargo install --force --path .
  # or: cargo uninstall cm && cargo install --path .
  ```

  If the `container distro` plugin was registered, re-run
  `sudo container-distro install-plugin` to refresh its copied binary.

## 0.4.1 -- 2026-10-08

### Fixed

- Release automation: the tag-triggered draft release extracts the
  changelog notes with `awk` instead of `sed | head -n -1` — negative
  `head` counts are GNU-only and the workflow runs on macOS (BSD
  tools). No functional changes to `cm`/`container-distro`.

## 0.4.0 -- 2026-10-08

> Tag exists but no GitHub Release was published — the release
> workflow's notes extraction failed on the GNU-only `head` flag
> (fixed in 0.4.1). Everything listed below is in 0.4.1.

### Added

- Distro catalog: `cm --list --online` shows a curated catalog in a
  `DistributionInfo.json` superset (Microsoft's own manifest parses via
  `--catalog`). `--install` resolves catalog names, and `--from-image`
  / `--from-file` import `.wsl` packages; catalog `.wsl` downloads are
  SHA-256-verified into a content-addressed cache managed via
  `--list --cache` and `--purge-cache`.
- `cm -l -o -v` annotates catalog entries with local state: `LOCAL`
  (`pulled` image / `cached` `.wsl` / `downloading`) and `INSTANCES`
  (machines and distros installed from the entry, running marked);
  `-v -v` adds `SOURCE`/`REF`.
- `cm --export` / `--import` cover `container machine`s: export clones
  the rootfs via a scratch container, import wraps the tar as an OCI
  image and retries the generic first-boot flake. `--import --distro`
  imports a distro.
- `container distro migrate --out` converts a distro into a
  `container machine`; `--in` selects machine-to-distro explicitly
  (still the default direction).

### Fixed

- Guest-reachable state hardened against tampering from a distro on an
  rw home share (security.md T6): init assets now install next to the
  plugin binary outside `$HOME`, and the per-user fallback `sbin.distro`
  dirs, `default-distro`, and `preserved/` staging are `uchg`-locked.

## 0.3.0 -- 2026-10-06

### Added

- Shell completions for `cm` and `container-distro` -- bash, zsh, and
  fish scripts generated from the CLI definitions at install time
  (`cm --install-completions`, `container-distro install-completions`).

### Changed

- `/Volumes` automounts are tracked separately from user mounts and
  reconciled live: the `automount` label is a `rw`/`ro`/`none` tri-state
  (`--automount[=MODE]` -- `ro` mounts read-only, `--no-automount` drops
  them), re-scanned on every `set` and refreshed when a stopped distro
  starts -- attaching or ejecting a volume no longer needs manual
  `set --add-volume`/`--rm-volume` fix-up.
- `import` labels its rootfs images (`…distro=<name>`,
  `…imported-from=<tar>`); image cleanup on `rm`/`set` matches that
  label instead of the `local/distro-<name>:` name prefix.
- `--publish` warns when the host address isn't loopback, and each boot
  lists the distro's published listeners.

### Fixed

- C8: the idle PID-1 loop now reaps orphaned zombie processes.
- C11: `uninstall-plugin` removes only the files `install` wrote and
  leaves foreign files/dirs in place.

## 0.2.0 -- 2026-10-06

First tagged release: `cm`, a WSL-shaped CLI over `container machine`,
plus `container distro` -- machine-like distros the same commands treat
as first-class.

- `cm`: WSL argument semantics (`-d`, `-e`, `--`), `cm -l` listing,
  `--shutdown`, `--terminate`, `--unregister`, `--set-default`,
  `--install` (machines and distros), experimental `--forward`,
  `--install-alias`, self-installing man pages.
- `container distro`: create/run/exec/list/inspect/start/stop/rm,
  crash-safe `set` that keeps the rootfs, `export`/`import`, machine
  `migrate`, mounts outside `$HOME`, `--publish`, `--automount`,
  `--home-mount`, `--restricted` preset, `install-plugin`.

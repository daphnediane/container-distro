# Changelog

User-facing changes per release, newest first. Written at release time
from the commit log and the TODO items completed since the previous
tag -- see doc/release.md.

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

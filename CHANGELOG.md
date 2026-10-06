# Changelog

User-facing changes per release, newest first. Written at release time
from the commit log and the TODO items completed since the previous
tag — see [doc/release.md](doc/release.md).

## 0.2.0 — 2026-10-06

First tagged release: `cm`, a WSL-shaped CLI over `container machine`,
plus `container distro` — machine-like distros the same commands treat
as first-class.

- `cm`: WSL argument semantics (`-d`, `-e`, `--`), `cm -l` listing,
  `--shutdown`, `--terminate`, `--unregister`, `--set-default`,
  `--install` (machines and distros), experimental `--forward`,
  `--install-alias`, self-installing man pages.
- `container distro`: create/run/exec/list/inspect/start/stop/rm,
  crash-safe `set` that keeps the rootfs, `export`/`import`, machine
  `migrate`, mounts outside `$HOME`, `--publish`, `--automount`,
  `--home-mount`, `--restricted` preset, `install-plugin`.

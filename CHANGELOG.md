# Changelog

User-facing changes per release, newest first. Written at release time
from the commit log and the TODO items completed since the previous
tag -- see docs/design/release.md.

## 0.6.0 -- 2026-10-11

### Added

- `cm --update` and `container distro update` — the `wsl --update`
  analog. Fetches a GitHub release pkg (latest or a named `vX.Y.Z`),
  verifies its SHA-256 against the API digest and its Developer ID
  signature and signing team against the installed binary, then hands
  it to Installer.app via `exec`. `--dry-run` resolves and verifies
  without installing; consent prompts cover downgrades, same-version,
  unsigned CI pkgs, team mismatch, and non-pkg installs (`--force`
  skips; non-tty refuses).
- Releases now ship a macOS `.pkg` installer and a `cargo-binstall`
  tarball (`cargo binstall container-distro` resolves the aarch64
  tarball) — installing no longer requires a Rust toolchain. The pkg
  stages binaries, the `container distro` plugin registration, man
  pages, completions, and license notices under `/usr/local`.
- `uninstall-container-distro.sh`, shipped in the pkg on Apple's
  `uninstall-container.sh` model: `-k` keeps per-user state, `-d` also
  deletes every distro, `sudo` only where needed, `-n` previews.
- `container distro delete --all` (alias `rm --all`) deletes the union
  of journaled distros and labeled containers.
- `--trusted` preset — the mirror of `--restricted`: rw home, rw
  automount, SSH agent forwarded.
- Mount options on `MountSpec`: `SRC:DST[:opt,...]` whitelisted to
  `ro`/`rw` plus guest flags (`nosuid`, `nodev`, `noexec`, `noatime`,
  `nodiratime`, `relatime`, `sync`, `dirsync`); `--automount` shares
  get `nosuid,nodev` automatically.
- Draft security policy (`SECURITY.md`).

### Changed

- **Home share and SSH agent are now trust-tiered.** Ad-hoc distros
  (`create` / `import` / `--from-file` / bare image refs) default to no
  home mount and no agent forwarding; only installs resolved through a
  catalog (built-in or `--catalog FILE`) keep the full WSL-style
  exposure, and catalog entries can pin their own via an optional
  `HomeMount` field. `--restricted` beats an explicit `--home-mount
  rw`. `container machine` can't honor agent-off (upstream forwards
  `SSH_AUTH_SOCK` unconditionally), so `cm --install --machine` and
  `cm --migrate --machine` are refused below rw home — `--trusted` is
  the opt-in.
- Test seams are gated behind a per-invocation `--dev` flag on both
  binaries (`CONTAINER_DISTRO_RELEASES_URL` release lookups and a
  non-default `CONTAINER_CLI`), so a planted environment variable can't
  steer a privileged path.

## 0.5.0 -- 2026-10-10

### Added

- Journaled distro state: a stopped distro is now only its journal —
  `spec.json` + `rootfs.ext4`, `uchg`-locked under `~/Library/
  Application Support/container-distro/distros/<name>/`. `start`
  materializes a fresh container from the journaled spec and `stop`
  dismantles it, so configuration a guest or stray `container` command
  writes into runtime state can never survive a restart; `start` also
  diffs a found-running container's reconstructed spec against the
  journal and warns per-field.
- `container distro rename` and `cm --rename OLD NEW`; `--machine` /
  `--distro` selectors disambiguate a name present in both backends.
- `container distro catalog list|info|install|cache|purge` — the same
  installable catalog `cm --list --online` uses; `catalog info`
  reports the full entry plus matching cache/image/machine/distro
  state (`--format json`).
- `cm --migrate NAME` converges a name onto the selected backend: a
  machine migrates in carrying its disk, a pre-journal distro is
  adopted, and `--machine` migrates a distro out.
- Missing-`container` install offer: on a terminal, the first command
  offers to download the pinned, SHA-256-verified signed installer pkg
  and opens it in Installer.app (`CONTAINER_DISTRO_NO_INSTALL_OFFER=1`
  opts out). Non-interactive failures print real install instructions,
  and `cm --version` reports `container: not installed` instead of
  erroring.
- Distro journals record provenance (`origin`: creation time, the
  creating build's version, and source — image / file / `.wsl`
  download / machine / adopted); `list` shows journal creation time
  for stopped distros and prefers the recorded source over generated
  `local/distro-*` image refs.

### Changed

- **Upgrading:** distros created by ≤0.4.2 are adopted automatically
  on first contact — the journal is written `uchg`-locked and the
  disk and live config carry over. Once the one-time `journalized`
  marker lands, journal-less containers are refused until adopted via
  `container distro migrate NAME` or `cm --migrate NAME`. Leftover
  `preserved/` `set`-staging pairs fold into the journal; the
  auto-adoption pass sunsets in the 1.1 line.
- `cm --install` and `cm --import` now create **distros** by default;
  `--machine` opts back into `container machine`. `--machine` /
  `--distro` are backend selectors on every name-resolving action
  (`-d`, `-t`, `-s`, `--unregister`, `--export`, `--forward`,
  `--install`, `--import`, `--migrate`, `--rename`).
- Cross-backend name collisions are refused on every create path —
  `create` / `import` / `catalog install` / `migrate` / `rename` and
  their machine equivalents — unless `--force` opts in (`cm` resolves
  a shared name to the distro, so a collision used to shadow the
  machine silently). `cm -l` and catalog views now list same-named
  pairs as two rows annotated `(machine)` / `(distro)`.
- `install-plugin` / `uninstall-plugin` resolve `container` as the
  invoking user, then self-elevate via `sudo` only when the plugin dir
  isn't user-writable — a no-op reinstall no longer prompts, and the
  docs drop the `sudo` prefix.
- Materialized distro containers get machine-style randomized ids
  (`<name>-<6hex>`); per-distro init assets live inside the journal
  dir and refresh on every materialize.
- `install-man` / `uninstall-man` prune stale generated
  `container-distro-*.1` pages left behind by removed subcommands
  (foreign or hand-written pages warn and stay).
- `export` of a stopped distro uses the same scratch-container +
  clonefile workaround as machine export — `container export` still
  can't read a journaled disk directly.

### Fixed

- `cm --install` names a catalog install after the entry (`ubuntu`,
  matching `wsl --install` registering `Ubuntu`) instead of deriving
  `ubuntu-latest` from the resolved image ref.

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

# Changelog

User-facing changes per release, newest first. The top `## X.Y.Z-dev`
section is the running draft for the in-flight release (matching the
dev version in `Cargo.toml`): entries land there as work completes --
finished `docs/TODO.md` items move here, other user-facing changes get
entries directly. At release time the `-dev` heading is renamed to
`## X.Y.Z -- YYYY-MM-DD` and audited against the commit log -- see
docs/design/release.md.

## 0.8.0-dev

### Added

- **amd64 (x86-64) distros on Apple silicon**: `--arch amd64` on
  `cm --install`/`--import`, `container distro create`/`import`, and
  `catalog install` selects x86-64 sources — catalog entries resolve
  their `Amd64Url` `.wsl` download (amd64-only entries like SLES become
  installable), images are fetched as `linux/amd64`, and imported
  rootfs archives are labeled `linux/amd64`. `container` auto-enables
  Rosetta, so the amd64 userspace runs translated on the arm64 kernel.
  The arch is journaled with the distro, so recreation/`set` keeps it.
  For systemd distros, init masks `systemd-binfmt.service` when the
  Rosetta registration is armed — its boot-time binfmt_misc flush would
  otherwise drop x86-64 handling mid-boot.
- **`--arch` on listings**: `cm -l --arch ARCH` and
  `container distro list --arch ARCH` filter installed machines and
  distros to that arch; `cm -l -o --arch ARCH` and
  `container distro catalog list --arch ARCH` render the catalog for
  that arch (status columns match against the selected arch's source).
- **`cm --terminal` and `container distro terminal NAME`**: open a
  distro in Terminal.app — its `.terminal` profile imports into
  Terminal's settings and a window launches into `cm -d NAME`. On
  `cm --install`/`--import`, `--terminal` replaces the console launch,
  and either command journals an explicit `--terminal-profile` opt-in
  so `sync-profiles` keeps the profile rather than pruning it.
- **`container distro sync-profiles`**: regenerates the
  terminal-profile artifacts for every journaled distro — repairs
  files missing after an upgrade or cleanup, honors recorded opt-outs,
  and prunes artifacts (and marker-bearing imported profiles) for
  distros that no longer exist.
- **Terminal-profile intent flags**: `--terminal-profile` /
  `--no-terminal-profile` on `create`/`import`/`cm --install`/`set`
  pin generation (a pin beats the package's `windowsterminal.enabled`
  source intent); `set --reset terminal-profile` un-pins back to
  derived, re-arming generation when resolution returns to enabled.

### Changed

- **Terminal.app profiles are launcher files.** Instead of writing
  Terminal's plist, each distro gets `terminal-profiles/NAME.terminal`
  plus a `terminal-cmds/NAME.sh` script under `~/Library/Application
  Support/container-distro/`. Opening the file imports the settings
  set through Terminal itself — into its live model, so a running
  Terminal's next prefs flush can no longer silently drop a profile —
  and the file doubles as a double-clickable launcher. Imported
  profiles run the script (`cm --cd ~ -d NAME`), so command updates
  never need a re-import, and `shellExitAction` closes the window on a
  clean exit while a failed launch stays visible. iTerm2 dynamic
  profiles run the same script. Terminal.app's `CommandString` isn't
  shell-parsed — quoting can't save a path containing `Application
  Support` — so profiles exec the script as
  `~/.container-distro/terminal-cmds/NAME.sh`, tilde-expanded through a
  `uchg`-locked symlink to the state dir; the scripts, `.terminal`
  files, iTerm2 profile, and the symlink itself are all locked so a
  guest with a rw-shared home can't rewrite a host launcher. A
  same-named foreign profile is never overwritten — Terminal imports
  ours as `NAME 1` — and a profile you delete in Terminal's prefs
  stays deleted until `set --terminal-profile`, `distro terminal`, or
  `sync-profiles` explicitly re-imports it.
- **`cm --cd ~` follows the effective user.** `~`/`~/…` now expand to
  the `-u` user's guest home — root → `/root`, a named user → its home
  resolved live in the guest via `getent passwd` (NSS-aware, with an
  `/etc/passwd` fallback in the same exec), an unmatched numeric uid →
  `/`; no `-u` keeps the provisioned user. Machine `container exec`
  resolves the same way.

### Fixed

- `cm --unregister`/`distro delete` no longer print a stray "Can't get
  settings set" AppleScript error for distros with no Terminal
  profile — removal only touches settings sets carrying our marker.

## 0.7.0 -- 2026-10-13

### Added

- **Presets are a journaled tier, not a one-time default flip.** `--preset
  default|restricted|trusted` on `create`/`import`/`cm --install`
  (`--restricted`/`--trusted` are aliases) records the tier on the distro,
  and `container distro set --preset TIER` re-derives it later. Options
  you set with an explicit flag stay pinned across tier changes;
  everything else follows the tier — including chains like home-mount
  inheriting the *effective* automount mode. `set` reports what was
  re-derived and what stayed pinned.
- **`set --reset`**: un-pin one option back to derived (`--reset
  home-mount`, `--reset ssh`, …), clear a list (`--reset mounts`,
  `--reset publish`), or reset every pin to a tier (`--reset
  restricted` — also sets the preset).
- **Catalog installs record `source` intent**: the entry's curated
  home-mount request fills options nothing else sets and caps what a
  tier would raise (a `ro`-pinned entry stays `ro` under `--trusted`),
  but explicit flags always win.
- **`distro list` shows the tier model**: MOUNTS gains `~`/`~(ro)` and
  `automounts`/`automounts(ro)` tokens; JSON adds `mounts` (user mounts
  only — automounts are no longer journaled), `automounts` (computed
  shares, home included), `effectiveMounts`, and `preset`. CPUS/MEMORY
  show a pinned value plainly, the host's pick bracketed (`[8]`), or
  `-` when nothing is recorded — unpinned resources are no longer
  frozen into the journal at create, so a host upgrade just changes
  what future boots get.
- `cm --install` gains `--preset` and `--ssh`/`--no-ssh`.
- The built-in catalog is now Microsoft's `DistributionInfo.json`
  vendored under `vendor/microsoft/`, merged with the
  `src/catalog.json` overlay (upstream fields win, the overlay fills
  gaps); entries prefer a `.wsl` source and fall back to `Image`. A
  catalog-freshness check runs in the release gate and the MIT license
  ships in the pkg.
- **Content-keyed image reuse on import**: imported rootfs images get a
  shared `local/rootfs:<sha256>` dedup tag plus per-importer
  `local/{distro,machine}-<name>:imported-<sha>` tags; `rm`/
  `--unregister` untag the per-name refs, and unused shared tags are
  evicted oldest-first past a size budget.
- **macOS terminal profiles** (`windowsterminal.enabled`, default
  true): a Terminal.app settings set and an iTerm2 dynamic profile that
  launch `cm -d NAME`; refreshed on rename, removed on delete/
  `migrate --out`.
- A `wsl.conf` subset is honored for distros: init reads the live file
  at boot (mode-gated; snapshots in the journal). Host-side `cm`
  config and the remaining keys are still open work.
- Release builds ship a CycloneDX SBOM
  (`container-distro-X.Y.Z-sbom.cdx.json`) and embed their cargo
  dependency tree (built with `cargo auditable`), so `cargo audit bin`
  can inventory an installed copy. The pkg installer gains a license
  step.

### Changed

- **Automounts are computed at materialization, never stored.** The
  journal's mount list is user mounts only; legacy journals drop
  automount-derived entries on migration. A user mount wins a target
  collision against a computed automount, and `automount none` no
  longer removes a mount you added yourself.
- **Machine-tier SSH check is honest about the knob.** `cm --install
  --machine` and `migrate --out` warn when the resolved agent setting
  is off rather than refusing non-`rw` home-mounts outright —
  `container machine` forwards `SSH_AUTH_SOCK` at every boot
  regardless (upstream has no opt-out), so choosing `--machine` is the
  opt-in. Only an explicit opt-out still refuses: `--no-ssh` at
  install, or a distro with `set --no-ssh` recorded on migrate.
- **`cm --shutdown` stops the services it started**: `ensure_started`
  leaves a `services-started` marker when it ran `container system
  start`; `--shutdown` now `system stop`s when the marker exists and no
  machines/containers remain running. `--shutdown --system` stays the
  explicit big hammer.

### Fixed

- The journal drift check no longer reports drift for image refs that
  only differ in normalization, or for cpus/memory left unpinned at
  create.
- The pkg installer's welcome/conclusion pages carry `lang`/`viewport`
  metadata (HTML validity; Installer.app rendering unchanged).

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

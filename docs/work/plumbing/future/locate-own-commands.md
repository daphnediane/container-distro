# Locate our own commands through `layout`

- **Issue:** [#36]
- **Milestone:** none (future)

`terminal.rs`'s `cm_path` (sibling `cm`, symlinks unresolved, baked
into terminal profiles) and `cm`'s alias `our_exe` (canonicalized,
hard error) each find binaries their own way. Add `layout` helpers for
`cm` and `container-distro`: `current_exe()` when we *are* that
command, the install-prefix sibling otherwise, PATH as the last resort
-- with one documented symlink policy (resolve, or deliberately keep
the user-facing path in generated profiles).

Catch: the `container distro` plugin can be installed from a user copy
(`install-plugin --from`) with no `cm` beside it, so the plugin can't
count on a sibling. Consider folding `cm` into the plugin binary as a
subcommand (e.g. `container distro cli ...`), with `cm` reduced to a
thin launcher that `exec`s `<path-to-container-distro> cli "$@"` (a
script or a tiny binary -- `cm --install-alias` and terminal profiles
must keep working, and `wsl`-style aliases need argv[0] handling).

## Investigation (proposed direction)

### One binary; `cm` is the default alias

Both front ends move into the library, and `container-distro` becomes
the **only** bin target. `cm` is no longer built. It is the first alias
`--install-alias` knows about, so `cm` and `wsl` are installed the same
way.

- **Aliases are symlinks to `container-distro`** (relative within the
  same directory, so they survive a prefix move or a `cargo install`
  replacing the binary in place). `main()` picks the front end from
  the basename of `argv[0]`: `distro` (the plugin's `bin/distro`) or a
  name starting with `container-distro` gives the plugin front end, and
  any other name gives `cm`. Error and help text keep the invoked name
  (`wsl: ...`).
- **`container-distro cli [CM ARGS...]`** (also `container distro
  cli`) runs the `cm` front end explicitly. It embeds `cm`'s clap
  `Args`, so help and completions come for free.
- **`cm --install-alias` / `--uninstall-alias`** stay. They link to the
  canonical `container-distro`, and removal verifies that the link
  resolves to it.
- Alternative considered: aliases as generated `#!/bin/sh` scripts
  (`exec <container-distro> cli "$@"`). That avoids argv[0] dispatch,
  but it bakes an absolute path that breaks if the binary moves, adds
  a shell per call, and loses the alias name. Symlinks also match how
  `--install-alias` already works and verifies its links.

### `install-all` / `uninstall-all`

With one binary, a single subcommand can install everything that is
split across `cm --install-*` and `container-distro install-*` today:

- the `container distro` plugin (`install-plugin`, elevating as it does
  now)
- `container-distro*.1` man pages, and `cm.1` plus a `<alias>.1` link
  for each alias. The `cm` page and `container-distro-cli.1` are the
  same page: render it once and link the other name.
- completions for `container-distro` and `cm`
- the `cm` alias, plus any `--alias NAME` given (for example `wsl`)

It takes the same `--prefix`/dir overrides as the individual commands,
so `scripts/build-pkg.sh` and `scripts/install-local.sh` become one
`install-all` into the payload or prefix. The individual `install-*`
subcommands stay for partial installs.

- Run from the plugin copy, the default prefix would be the root-owned
  `<plugins>/distro`. Require an explicit `--prefix` there.
- Upgrades: an old install has a real `cm` *file*, which the
  symlink-only alias install refuses to replace. `install-all` (and
  the pkg payload) must replace a regular `cm` file it recognizes as
  ours (`cm --version` output, or the pkg receipt). Also check whether
  `cargo install` of the new version removes the old `cm` bin from
  `$CARGO_HOME/bin`. If it doesn't, `install-all` handles it.
- `cargo install container-distro` alone no longer provides `cm`. The
  install docs say to follow it with `container-distro install-all`,
  which is already the pattern for the plugin and man pages.

### Self-location

- **Terminal launchers** bake the canonical running binary and the
  plugin front end: `'<container-distro>' run -n NAME -w '~' "$@"`.
  From the plugin copy that is the plugin's own path. `run -n` reaches
  the same `ops::run_command` as `cm -d NAME --cd ~` (`-w '~'` expands
  to the guest home the same way). It only ever targets the distro,
  unlike `cm -d`, which resolves names across machines and distros and
  honors `CM_BACKEND`. There is **no PATH fallback**, which today
  could bake whatever `cm` comes first on PATH into a `uchg`-locked
  launcher. A dev build bakes its `target/` path, the same as today.
  This covers the `terminal-cmds/<name>.sh` scripts and the
  Terminal/iTerm2 fallback commands used when the alias can't be made
  (`launch_command`, `iterm2_command`).
- **Passthrough semantics change.** `name.sh cmd args...` (#33) gives
  `run` exact argv. Under `cm` a bare command line was evaluated by the
  guest shell. Exact argv is intended: the caller's shell already
  split the arguments, and re-evaluating them double-expands quotes and
  `$`. A pipeline is still `name.sh sh -c '...'`. With no arguments
  (every profile launch) both give a login shell. Note this in the
  changelog.
- Nothing needs to locate `cm` any more. `cm_path()` and `our_exe()`
  are replaced by `layout::current_exe()`.

Symlink policy: **always resolve**, matching `layout`'s existing rule.

Edges to handle:

- Confirm what `argv[0]` `container` gives the plugin when it execs
  `bin/distro`, and keep the existing `argv[1] == "distro"` strip.
- A renamed `container-distro` copy (one that doesn't start with
  that name) runs as `cm`. Document this.
- Existing launchers point at `cm`. They keep working through the
  alias, and `sync-profiles` rewrites them to the new form.
- Update security.md: the launcher target changes from "sibling or
  PATH `cm`" to "canonical running binary".

### Code organization

Proposed tree. Each move is a pure move; behavior changes come later.

```text
src/
  lib.rs               dev mode, module list
  naming.rs            APP_NAME etc.
  cli/                 front ends (moved from src/bin/)
    mod.rs             main(): argv[0] dispatch, exit-code helpers
    docs.rs            man + completion rendering (now duplicated per bin)
    cm/                WSL-style front end
      mod.rs           Action dispatch
      args.rs          (was bin/cm/cli.rs)
      alias.rs backend.rs list.rs
      run.rs           run_in_machine / run_in_distro / launch
      install.rs       --install / --import / --export
      manage.rs        -s / -t / --unregister / --shutdown / --rename / --migrate
      forward.rs       --forward
    distro/            `container distro` front end
      mod.rs           Command enum + dispatch for trivial subcommands
      list.rs          (print_table)  set.rs  catalog.rs  setup.rs
      ...              one file per non-trivial subcommand, each owning
                       its clap args struct
  distro/              distro operations (was ops.rs, 5.2k lines)
    mod.rs             DistroSummary, find/resolve, re-exports
    host.rs            host user/home/memsize, default_resources
    assets.rs          sbin.distro assets, uchg lock helpers
    journal.rs         journal paths, read/write, adoption, DistroLock
    create.rs          CreateOptions, spec building, boot
    list.rs            summaries / inspect
    lifecycle.rs       start / stop / delete / set-default
    run.rs             RunOpts, run_command
    set.rs             spec diff, converge
    export.rs import.rs rename.rs migrate.rs
    machine.rs         machine export/import/create, scratch container
    terminal.rs        (was src/terminal.rs)
    spec/              (was spec.rs, 3.8k lines)
      mod.rs           DistroSpec / ResolvedSpec / JournalSpec
      intent.rs        Preset, Intent, ResetTarget, ResolveCtx
      options.rs       HomeMount, Arch, Automount
      wsl_conf.rs      WslConf, WslConfMode, ConfList
      shares.rs        MountSpec, PublishSpec, automounts
      changes.rs       SpecChanges
  container/           Apple `container` CLI wrapper (was container.rs)
    mod.rs             binary resolution, container_cmd, ensure_started
    types.rs           serde structs for its JSON
    machine.rs         machine run/exec helpers
    install.rs         offer to install `container` (was install.rs, minus sudo)
  catalog/             (catalog.rs + catalog_ops.rs + wsl.rs + catalog.json)
    mod.rs  resolve.rs  info.rs  wsl.rs (.wsl packages + download cache)
  setup/               installing ourselves
    layout.rs plugin.rs man.rs completions.rs update.rs privilege.rs
  util/                table.rs oci.rs forward.rs
```

Each `tests` module splits along with its code, and moving the front
ends into the library lets many `pub` items narrow to `pub(crate)`.

### Phasing (each phase is a separate, green commit)

1. Move both front ends into `src/cli/` unchanged (both bins become
   one-line mains for now); dedupe man/completion rendering.
2. Split `ops.rs` / `spec.rs` / `terminal.rs` into `distro/`.
3. Regroup `container/`, `catalog/`, `setup/`, `util/`.
4. Behavior: argv[0] dispatch, the `cli` subcommand, drop the `cm`
   bin target, `cm` becomes the default alias, launchers switch to
   `run -n`, plus docs, security.md and changelog.
5. `install-all` / `uninstall-all`, including replacing a legacy `cm`
   file, then switch `build-pkg.sh` and `install-local.sh` over.

`feature/distrofs` touches `ops.rs`, `plugin.rs` and the plugin front
end (adds `src/distrofs.rs`, `bin/container-distro/fs.rs`). Phases 1-3
will conflict with it, so either land it first or rebase it right
after each move.

[#36]: https://github.com/daphnediane/container-distro/issues/36

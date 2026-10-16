# Locate our own commands; one binary for both front ends

- **Issue:** [#36]
- **Milestone:** none (future)

## Done so far

- Both front ends live in the library (`src/cli/`), and the library
  is grouped by concern (`distro/`, `container/`, `catalog/`,
  `setup/`, `util/`).
- Every binary can run both front ends. `container-distro cli ARGS`
  is `cm ARGS`. `layout::self_invocation` re-runs *this* binary with
  the leading arguments that select a front end. Nothing searches for
  a sibling or on `PATH` any more.
- Terminal launchers bake `'<this binary>' run -n NAME -w '~' "$@"`.
- Aliases are generated `/bin/sh` scripts (`exec '<binary>'
  --invoked-as "$0" [cli] "$@"`) that record which front end they
  run. A bare-name alias lands beside the alias it was created
  through. `container-distro version`, `install-alias` and
  `uninstall-alias` exist, and a lone `cm -v` is `--version`.

## Remaining: `install-all`, then drop the `cm` binary

`cm` becomes the default alias of `container-distro`, installed like
`wsl`, and the separate `cm` bin target goes away. That also retires
the hidden `--front-end=distro` selector, which only exists so
launchers baked by the `cm` binary can reach `container distro run`.

`container-distro install-all` / `uninstall-all` installs what is
split across `cm --install-*` and `container-distro install-*` today:

- the `container distro` plugin (`install-plugin`, elevating as now)
- `container-distro*.1` man pages, including `container-distro-cli.1`.
  `cm.1` and each alias's `NAME.1` link to that one page.
- completions for `container-distro` and `cm`
- the `cm` alias, plus any `--alias NAME` given (for example `wsl`)

`scripts/build-pkg.sh` and `scripts/install-local.sh` then become one
`install-all` each.

Edges to handle:

- **Payload builds bake the wrong path.** An alias records the
  canonical binary that wrote it. `build-pkg.sh` runs from the
  payload staging dir, so `install-all --prefix DIR` must bake the
  *installed* path (`DIR/bin/container-distro`), not the staging copy.
- **Legacy `cm` file.** An old install has a real `cm` binary where
  the alias goes. `install-all` (and the pkg payload) must replace a
  `cm` it recognizes as ours (`cm --version` output, or the pkg
  receipt). Check whether `cargo install` of the new version removes
  the old `cm` bin from `$CARGO_HOME/bin`; if it doesn't,
  `install-all` handles it.
- **`cargo install` alone no longer provides `cm`.** The install docs
  say to follow it with `container-distro install-all`, which is
  already the pattern for the plugin and man pages.
- **The plugin copy's default prefix** is `container`'s root-owned
  plugin tree. Require an explicit `--prefix` there, as
  `install-alias` already requires a path.
- **A moved binary leaves aliases pointing at the old path.**
  `install-all` re-runs fix them; consider having `update` do the same.
- **Terminal automation errors are invisible.** `launch_window`
  discards `osascript`'s stderr, so a missing Automation permission
  (`-1743`) surfaces only as "couldn't open NAME's profile". Report
  it, with the System Settings path.

[#36]: https://github.com/daphnediane/container-distro/issues/36

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

[#36]: https://github.com/daphnediane/container-distro/issues/36

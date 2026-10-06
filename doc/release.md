# Release process

Releases are git tags plus a maintenance branch — there is no crates.io
publish step (`cargo install --path` is the only install path; see
[install](install.md)).

## Versioning

- One version for the whole workspace, in the root `Cargo.toml`
  (`[workspace.package].version`); `cm`, `cm-core`, and
  `container-distro` all inherit it via `version.workspace = true`.
- [SemVer](https://semver.org): `MAJOR.MINOR.PATCH`. Pre-1.0, minor bumps
  (`0.x.0`) may break CLI behavior; patch bumps (`0.x.y`) are fixes only.
- Tags are `v<version>` (`v0.2.0`), annotated. Maintenance branches are
  `release/<major>.<minor>` (`release/0.2`).

## Cutting a minor release (from `main`)

1. Confirm `main` is green: `cargo test --workspace` passes, and
   [TODO](TODO.md) plus the user-facing docs reflect what actually
   shipped.
2. Bump `[workspace.package].version` in `Cargo.toml` to the release
   version, then run `cargo check` so `Cargo.lock` (committed) picks it
   up.
3. Commit: `chore: release 0.2.0` (message format per
   `.devin/rules/comment-file.md`).
4. Tag and branch:

   ```bash
   git tag -a v0.2.0 -m "container-distro 0.2.0"
   git branch release/0.2 v0.2.0
   ```

5. Push everything:

   ```bash
   git push origin main release/0.2 v0.2.0
   ```

6. Create the GitHub Release on the pushed tag (`gh` is authenticated;
   `--verify-tag` refuses to mint a lightweight tag if the annotated one
   didn't push):

   ```bash
   gh release create v0.2.0 --verify-tag --generate-notes
   ```

   Edit the generated notes in the web UI or pass `--notes-file`
   instead. For a real prerelease (`v0.3.0-rc.1`) add `--prerelease`.
   Prebuilt binaries are optional — `gh release upload v0.2.0
   target/release/cm target/release/container-distro` — since install is
   source-only today.

7. Sanity-check the tag:

   ```bash
   git checkout v0.2.0 && cargo test --workspace
   cm --version   # should print 0.2.0
   git checkout main
   ```

## Patch releases (on `release/X.Y`)

1. Cherry-pick (or land directly) the fixes onto `release/X.Y`.
2. Bump the patch component in `Cargo.toml`, refresh `Cargo.lock`,
   commit `chore: release X.Y.Z`.
3. `git tag -a vX.Y.Z -m "container-distro X.Y.Z"` on the release
   branch; push the branch and tag. The branch head is always the latest
   `X.Y.*` tag.

## Notes

- `main` is the development branch; its version stays at the last
  release until the next release bumps it. Don't bump "just in case."
- Man pages are generated from the clap definitions at build time, so a
  release has no generated artifacts to regenerate or commit.

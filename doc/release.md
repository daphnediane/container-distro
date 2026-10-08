# Release process

Releases are git tags plus a maintenance branch -- there is no crates.io
publish step (`cargo install --path` is the only install path; see
[install]).

## Versioning

- One version for the whole workspace, in the root `Cargo.toml`
  (`[workspace.package].version`); `cm`, `cm-core`, and
  `container-distro` all inherit it via `version.workspace = true`.
- [SemVer]: `MAJOR.MINOR.PATCH`. Pre-1.0, minor bumps (`0.x.0`) may
  break CLI behavior; patch bumps (`0.x.y`) are fixes only.
- Tags are `v<version>` (`v0.2.0`), annotated. Maintenance branches are
  `release/<major>.<minor>` (`release/0.2`).

## Cutting a minor release (from `main`)

1. Confirm `main` is green: `cargo test --workspace` passes (the
   proptest parser-fuzz targets run inside it; for a deeper release-time
   pass use `PROPTEST_CASES=100000 cargo test --workspace`), the
   supply-chain gate is clean (`cargo audit` + `cargo deny check` --
   advisory DB needs network; license policy lives in `deny.toml`),
   `THIRD-PARTY-NOTICES.yaml` is regenerated if deps changed
   (`cargo bundle-licenses -f yaml -o THIRD-PARTY-NOTICES.yaml`), and
   [TODO] plus the user-facing docs reflect what actually shipped. If
   `container`'s minor version changed since the last release, first
   re-verify the appRoot internals we depend on ([Version gating]) and
   extend `VERIFIED_CONTAINER_MINOR` once verified.
2. Update `CHANGELOG.md` (see [Changelog]): prepend a
   `## X.Y.Z -- YYYY-MM-DD` section drafted from the commit log since
   the previous tag (`git log --oneline v<prev>..HEAD`) and the
   `TODO.md` items those commits completed -- then delete the completed
   `[x]` entries so the changelog is their permanent record.
3. Bump `[workspace.package].version` in `Cargo.toml` to the release
   version, then run `cargo check` so `Cargo.lock` (committed) picks it
   up.
4. Commit: `chore: release X.Y.Z` (message format per
   `.devin/rules/comment-file.md`).
5. Tag and branch:

   ```bash
   git tag -a vX.Y.Z -m "container-distro X.Y.Z"
   git branch release/X.Y vX.Y.Z
   ```

6. Push everything:

   ```bash
   git push origin main release/X.Y vX.Y.Z
   ```

7. Create the GitHub Release on the pushed tag (`gh` is authenticated;
   `--verify-tag` refuses to mint a lightweight tag if the annotated one
   didn't push). The new `CHANGELOG.md` section doubles as the notes:

   ```bash
   gh release create vX.Y.Z --verify-tag --notes-file <(sed -n '/^## X.Y.Z/,/^## /p' CHANGELOG.md | head -n -1)
   ```

   Or `--generate-notes` and edit in the web UI. For a real prerelease
   (`vX.Y.Z-rc.1`) add `--prerelease`. Prebuilt binaries are optional --
   `gh release upload v0.2.0 target/release/cm
   target/release/container-distro` -- since install is source-only
   today. If binaries ever ship, attach `THIRD-PARTY-NOTICES.yaml` too
   -- it carries every dep's copyright + license text
   (MIT/BSD/Apache/Unicode all require reproducing them in distributed
   copies). `cargo install` alone doesn't trigger that -- users compile
   the deps themselves.

8. Sanity-check the tag:

   ```bash
   git checkout vX.Y.Z && cargo test --workspace
   cm --version   # should print X.Y.Z
   git checkout main
   ```

## Patch releases (on `release/X.Y`)

1. Cherry-pick (or land directly) the fixes onto `release/X.Y`.
2. Bump the patch component in `Cargo.toml`, refresh `Cargo.lock`, and
   prepend the `CHANGELOG.md` section the same way; commit
   `chore: release X.Y.Z`.
3. `git tag -a vX.Y.Z -m "container-distro X.Y.Z"` on the release
   branch; push the branch and tag. The branch head is always the latest
   `X.Y.*` tag.

## Changelog

`CHANGELOG.md` (repo root) records user-facing changes per release,
newest first -- `## X.Y.Z -- YYYY-MM-DD` sections grouped Added /
Changed / Fixed. It's drafted at release time from the commit log since
the previous tag plus the `TODO.md` entries being retired: completed
`[x]` items are deleted from `TODO.md` in the release commit, so the
changelog is their permanent record and TODO stays a list of work that
remains.

## Notes

- `main` is the development branch; its version stays at the last
  release until the next release bumps it. Don't bump "just in case."
- Man pages are generated from the clap definitions at build time, so a
  release has no generated artifacts to regenerate or commit.
- The supply-chain gate (`cargo audit` + `cargo deny check`) is a
  manual release step until CI exists ([#16]); the `FromStr` parser
  fuzz targets and their release-time runs are tracked by [#17], and
  CI automation for both by [#12].

[#12]: https://github.com/daphnediane/container-distro/issues/12
[#16]: https://github.com/daphnediane/container-distro/issues/16
[#17]: https://github.com/daphnediane/container-distro/issues/17
[Changelog]: #changelog
[install]: install.md
[SemVer]: https://semver.org
[TODO]: TODO.md
[Version gating]: container-internals.md#version-gating

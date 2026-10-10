# Release process

Releases are git tags plus a maintenance branch; `cargo publish` after
tagging makes the release installable via `cargo install
container-distro` (`--path .` builds a checkout; see [install]).

## Versioning

- One version for the package, in the root `Cargo.toml`
  (`[package].version`); the `cm` and `container-distro` binaries
  share it.
- [SemVer]: `MAJOR.MINOR.PATCH`. Pre-1.0, minor bumps (`0.x.0`) may
  break CLI behavior; patch bumps (`0.x.y`) are fixes only.
- Tags are `v<version>` (`v0.2.0`), annotated. Maintenance branches are
  `release/<major>.<minor>` (`release/0.2`).
- **Only tagged commits carry a clean `X.Y.Z` version.** Every other
  commit carries a prerelease suffix: `main` sits at
  `<next-minor>.0-dev` (`0.5.0-dev`), and `release/X.Y` sits at
  `<next-patch>-dev` (`0.5.1-dev`) between patches. Consequences:
  `cm --version` on a dev build can't be mistaken for a release, and a
  mistaken `cargo publish` lands as a crates.io *prerelease* (never
  selected by default installs) instead of colliding with a real
  release.

## Tooling: `cargo release`

The mechanical bits are driven by [cargo-release] (`cargo install
--locked cargo-release`), configured in `[package.metadata.release]` in
`Cargo.toml`:

- `cargo release release` is the release command: it strips the
  `-dev` suffix, refreshes `Cargo.lock`, commits
  `chore: release X.Y.Z`, and creates the annotated tag `vX.Y.Z`
  (`container-distro X.Y.Z`). A **dry run by default**; `-x`
  executes.
- `publish = false` / `push = false`: crates.io publish and git push
  stay manual steps below -- nothing escapes before the tag is pushed
  and `cargo publish` runs from it.
- `allow-branch = ["main", "release/*"]`: releases can only be cut
  from `main` or a release branch.
- `cargo release version X.Y.Z-dev -x` sets an explicit dev version
  (the post-release bump step) without tagging.

## Cutting a minor release (from `main`)

1. Run the green gate: `scripts/release-gate.sh` covers fmt, clippy,
   `cargo test --workspace` (the proptest parser-fuzz targets run
   inside it; `PROPTEST_CASES=100000` deepens the pass), the
   supply-chain gate (`cargo audit` + `cargo deny check` -- advisory DB
   needs network; license policy lives in `deny.toml`), and a
   `THIRD-PARTY-NOTICES.yaml` freshness check (regenerate with
   `cargo bundle-licenses -f yaml -o THIRD-PARTY-NOTICES.yaml` if
   stale). Then confirm [TODO] plus the user-facing docs reflect what
   actually shipped. If `container`'s minor version changed since the
   last release, first re-verify the appRoot internals we depend on
   ([Version gating]) and extend `VERIFIED_CONTAINER_MINOR` plus the
   install-offer pin (`PINNED_CONTAINER_VERSION`, `PKG_SHA256`) once
   verified.

   Then run the **legacy adoption check**,
   `scripts/check-legacy-adoption.sh`, against the real state dir and
   appRoot (`container` must be running). It builds pre-journal distros
   with the newest pre-journal release -- pinned under `scratch/` as
   `cargo install --locked container-distro@0.4.2 --root
   scratch/old-0.4.2` (bump the pin if a later pre-journal release ever
   ships) -- and verifies this build auto-adopts them on first contact
   (journal `uchg`-locked, disk and live config carried), refuses
   journal-less containers once the `journalized` marker is set until
   `cm --migrate` adopts them, and folds a legacy `preserved/`
   `set`-staging pair into the journal. It cleans up its own
   `relcheck-*` distros; re-opening the migration window also re-adopts
   any other still-pre-journal distros on the machine, which is the
   intended upgrade behavior. Keep the check in the gate until
   auto-adoption sunsets in 1.2.0.
2. Update `CHANGELOG.md` (see [Changelog]): prepend a
   `## X.Y.Z -- YYYY-MM-DD` section drafted from the commit log since
   the previous tag (`git log --oneline v<prev>..HEAD`) and the
   `TODO.md` items those commits completed -- then delete the completed
   `[x]` entries so the changelog is their permanent record. Commit
   this *before* releasing: `cargo release` requires a clean tree.
3. Preview then cut the release:

   ```bash
   cargo release release      # dry run: 0.5.0-dev -> 0.5.0, commit, tag
   cargo release release -x   # bumps, commits "chore: release 0.5.0",
                              # tags v0.5.0
   ```

   (Always pass `release` explicitly: it strips the prerelease suffix,
   while bare `cargo release` releases the manifest version unchanged
   and would tag `v0.5.0-dev`. `cargo release minor` would bump
   `0.5.0-dev` to `0.6.0` -- only use a level when the dev version
   isn't already the intended release.)
4. Create the maintenance branch on the tag:

   ```bash
   git branch release/X.Y vX.Y.Z
   ```

5. Push everything (`push = false` keeps this manual):

   ```bash
   git push origin main release/X.Y vX.Y.Z
   ```

6. Pushing the tag runs `.github/workflows/release.yml`, which verifies
   the tag matches the package version, re-runs the tests, and
   creates a **draft** GitHub Release using the new `CHANGELOG.md`
   section as notes (`--prerelease` when the tag carries a `-` suffix
   like `v0.4.0-rc.1`). Review the draft and publish it -- in the web
   UI, or `gh release edit vX.Y.Z --draft=false`. If the workflow is
   unavailable, the manual equivalent is:

   ```bash
   gh release create vX.Y.Z --verify-tag --notes-file \
     <(awk -v hdr="## X.Y.Z " 'f && /^## / {exit} index($0, hdr) == 1 {f = 1} f' CHANGELOG.md)
   ```

   (`awk`, not `sed | head -n -1` — BSD `head` has no negative counts,
   and macOS is the dev and CI platform.)

   Prebuilt binaries are optional --
   `gh release upload v0.2.0 target/release/cm
   target/release/container-distro` -- since install is source-only
   today ([#14] owns packaging). If binaries ever ship, attach
   `THIRD-PARTY-NOTICES.yaml` too -- it carries every dep's copyright +
   license text (MIT/BSD/Apache/Unicode all require reproducing them in
   distributed copies). `cargo install` alone doesn't trigger that --
   users compile the deps themselves.

7. Publish to crates.io **from the tagged commit**: `cargo publish`
   (one-time `cargo login` first). `cargo publish --dry-run` packages
   and verify-builds without uploading. Never publish from a `-dev`
   version -- it would land as a prerelease.
8. Open the next development cycle on `main`:

   ```bash
   cargo release version X.Y.0-dev -x   # next minor, e.g. 0.6.0-dev
   git commit -am "chore: start X.Y development"
   git push
   ```

   (message format per `.devin/rules/comment-file.md`)
9. Sanity-check the tag:

   ```bash
   git checkout vX.Y.Z && cargo test --workspace
   cm --version   # should print X.Y.Z
   git checkout main
   ```

## Patch releases (on `release/X.Y`)

The `release/X.Y` branch is created at minor-release time (step 4), so
it already exists when a backport is needed. If it doesn't (e.g. a
patch for an old line), create it from the tag:

```bash
git branch release/X.Y vX.Y.0
```

1. Check out `release/X.Y`. If the branch head is still the last tag
   (clean `X.Y.Z` version), first open the patch's dev cycle so
   in-flight builds report a prerelease:

   ```bash
   cargo release version X.Y.Z-dev -x   # e.g. 0.5.1-dev
   git commit -am "chore: start X.Y.Z development"
   ```

2. Cherry-pick the fixes (`git cherry-pick -x <sha>` records the
   source commit) or land them directly on the branch.
3. Prepend the `CHANGELOG.md` section the same way as a minor release;
   commit.
4. Cut the patch release: `cargo release release` then
   `cargo release release -x` -- strips `-dev`, commits
   `chore: release X.Y.Z`, tags `vX.Y.Z`.
5. Push the branch and tag, publish the draft GitHub Release the
   workflow creates, then `cargo publish` from the tag.
6. Re-open the branch's dev cycle:

   ```bash
   cargo release version X.Y.Z-dev -x   # e.g. 0.5.2-dev
   git commit -am "chore: start X.Y.Z development"
   git push origin release/X.Y
   ```

## Changelog

`CHANGELOG.md` (repo root) records user-facing changes per release,
newest first -- `## X.Y.Z -- YYYY-MM-DD` sections grouped Added /
Changed / Fixed. It's drafted at release time from the commit log since
the previous tag plus the `TODO.md` entries being retired: completed
`[x]` items are deleted from `TODO.md` in the release commit, so the
changelog is their permanent record and TODO stays a list of work that
remains.

## Notes

- `main` is the development branch; its version is always
  `<next-minor>.0-dev` except inside the release commit itself, so
  building from `main` never masquerades as a released version.
- Man pages are generated from the clap definitions at build time, so a
  release has no generated artifacts to regenerate or commit.
- `cargo audit` + `cargo deny check` run in CI on every push/PR and
  weekly ([#12]); `scripts/release-gate.sh` bundles them with the rest
  of the step-1 gate ([#16]). The `FromStr` parser fuzz targets are
  proptest properties that run inside `cargo test` ([#17]).

[#12]: https://github.com/daphnediane/container-distro/issues/12
[#14]: https://github.com/daphnediane/container-distro/issues/14
[#16]: https://github.com/daphnediane/container-distro/issues/16
[#17]: https://github.com/daphnediane/container-distro/issues/17
[cargo-release]: https://github.com/crate-ci/cargo-release
[Changelog]: #changelog
[install]: ../install.md
[SemVer]: https://semver.org
[TODO]: ../TODO.md
[Version gating]: container-internals.md#version-gating

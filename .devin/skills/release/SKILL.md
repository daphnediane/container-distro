---
name: release
description: Cut a release — gate, changelog adoption, tag, publish, close issues
---

# Release

Checklist for a minor release from `main`. Full detail — versioning
rules, signing, patch-release flow on `release/X.Y` — lives in
`docs/design/release.md`; read it before deviating.

1. Confirm `Cargo.toml` is at `X.Y.Z-dev` and the tree is on `main` (for a patch release, `release/X.Y` — see release.md § Patch releases).
2. Run `scripts/release-gate.sh`, then `scripts/check-legacy-adoption.sh` (needs `container` running). Confirm `docs/work/` lists only still-open work (prune links to issues closed this cycle) and user-facing docs match what shipped. Sweep the release's GitHub milestone for open issues (`gh issue list --milestone <name> --state open`) — fix, re-milestone, or consciously defer each before tagging (only `1.0` exists today; interim minors skip this).
3. In `CHANGELOG.md` rename `## X.Y.Z-dev` to `## X.Y.Z -- YYYY-MM-DD`, then audit `git log --oneline v<prev>..HEAD` — add entries for anything that landed without one, and note which `[#N]` issues the cycle resolves for step 7.
4. Commit per `.devin/rules/comment-file.md` (`cargo release` needs a clean tree), then:

   ```bash
   cargo release release                  # dry run
   cargo release release -x --no-confirm  # strips -dev, commits, tags vX.Y.Z
                                          # (--no-confirm: no interactive prompt)
   git branch release/X.Y vX.Y.Z
   git push origin main release/X.Y vX.Y.Z
   ```

5. The tag push drafts a GitHub Release with the artifacts. If signing is configured (`packaging/signing.local.conf`): `scripts/build-pkg-signed.sh` then `scripts/upload-release-artifacts.sh vX.Y.Z`. Review the draft and publish it.
6. `cargo publish` from the tagged commit.
7. Close the issues the release resolves (`gh issue list` vs. the new changelog section and `[#N]` commit refs), commenting the `vX.Y.Z` tag on each.
8. Open the next cycle: `cargo release version X.Y.0-dev -x --no-confirm`, start a fresh empty `## X.Y.0-dev` section atop `CHANGELOG.md`, commit, push.
9. Sanity-check: `git checkout vX.Y.Z && cargo test --workspace`, `cm --version` prints `X.Y.Z`, back to `main`.

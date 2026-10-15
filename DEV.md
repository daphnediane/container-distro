# Developing container-distro

Working *in* this codebase: building, testing, packaging, and
releasing. For contributing changes back (issues, PRs, conventions),
see [CONTRIBUTING.md](CONTRIBUTING.md). User-facing install detail
lives in [docs/install.md](docs/install.md); this file covers the
maintainer view.

## Prerequisites

- macOS 26 on Apple silicon, with Apple's [`container`] installed and
  working (`container system start`, `container machine list`) —
  development targets `container` 1.5.x
- a current stable Rust toolchain (edition 2024; `rustup` recommended);
  for the guest `distrofs` build (below) also the
  `aarch64-unknown-linux-musl` target and a sibling checkout of
  [daphnediane/distrofs](https://github.com/daphnediane/distrofs)
- `cargo deny`, `cargo audit`, and `cargo bundle-licenses` for the
  release gate; `jq`, `pkgbuild`, `productbuild` (Xcode tools) for
  packaging
- `cargo cyclonedx` and `cargo auditable` — optional for ad-hoc
  `build-pkg.sh` runs (skipped with a warning), required on the
  official release path (`build-pkg-signed.sh`, release.yml): they
  produce the CycloneDX SBOM and the dep tree embedded in the binaries

## Build and test

```bash
cargo build --workspace      # debug binaries in target/debug/
cargo build --release        # optimized
```

The CI-equivalent check before committing:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`cargo test` includes proptest parser-fuzz targets;
`PROPTEST_CASES=100000 cargo test --workspace` deepens the pass.
`scripts/release-gate.sh` runs the whole pre-release gate: fmt,
clippy, tests, an `sh -n` syntax pass on `assets/*`, `cargo audit`,
`cargo deny --workspace check` (license policy in `deny.toml`), a
THIRD-PARTY-NOTICES.yaml freshness check (regenerate with
`cargo bundle-licenses -f yaml -o THIRD-PARTY-NOTICES.yaml`), and a
freshness check on the vendored Microsoft catalog (refresh
`vendor/microsoft/` with `scripts/update-microsoft-catalog.sh`).

### Building distrofs

`distrofs` is a separate project —
[daphnediane/distrofs](https://github.com/daphnediane/distrofs): one
crate shipping the guest file server *and* the host client (the wire
protocol, socket hygiene, peer check, and handshake all live there).
This repo keeps only deployment: locating the binaries, verifying the
client's signature/ownership, the `--publish-socket` path, and
`container exec` launch.

For development, clone distrofs beside this checkout
(`../distrofs`, or set `DISTROFS_DIR`). The guest server is a static
`aarch64-unknown-linux-musl` binary that distros mount at
`/sbin.distro/distrofs`; one arm64 binary serves amd64 distros too —
the VM kernel is always arm64.

```bash
scripts/build-distrofs.sh                    # builds ../distrofs for aarch64-musl
scripts/build-distrofs.sh --install ~/.cargo # + ~/.cargo/libexec/container-distro/distrofs-server-aarch64
```

The host binaries copy the server into each distro's init assets at
materialize and again before each `fs` session (a live share, so
running distros pick it up without a restart). Server lookup order:
`--dev` sibling-worktree builds, `<prefix>/libexec/container-distro/`
(`distrofs-server-aarch64`, or plain `distrofs` from earlier
installs), then the distrofs-managed `<prefix>/libexec/distrofs/` —
where `distrofs fetch-servers` puts verified release downloads.
The client binary is found as a sibling of the running binary
(`install-plugin` copies it into `bin/`), the libexec dir, or PATH;
before it's exec'd it's verified — a Developer ID signature must
satisfy `identifier "io.github.daphnediane.distrofs"`, unsigned
cargo/dev builds must be owned by us or root on a path writable only
by trusted accounts.

The dev loop is `scripts/build-distrofs.sh` in the sibling checkout
plus `cargo build` for the client, then `cargo run --bin
container-distro -- --dev fs check NAME`.

Man pages and shell completions are generated at runtime from the
clap definitions — never edit generated output; change the
definitions. If a change alters the CLI surface, update the matching
reference doc (`docs/cm.md` or `docs/container-distro.md`).

## Repository layout

One published crate shipping two binaries over a shared library:

| Path                        | Contents                                                            |
| --------------------------- | ------------------------------------------------------------------- |
| `src/`                      | `container_distro` library: shared `container` CLI plumbing         |
|                             | (naming, OCI, forwarder, self-update, …) plus distro logic          |
| `src/bin/cm/`               | the `cm` binary (WSL-compatible CLI)                                |
| `src/bin/container-distro/` | the `container-distro` binary (`container distro` plugin front end) |
| `assets/`                   | distro init scripts (`sbin.distro*/`)                               |
| `scripts/`                  | maintainer scripts (below)                                          |
| `packaging/`                | pkg resources: Distribution.xml, uninstaller/updater, signing conf  |
| `docs/`                     | user guides and `docs/design/` architecture docs                    |
| `scratch/`                  | gitignored scratch space (pkg builds, pinned old releases)          |

## Scripts

| Script                                | Purpose                                                                 |
| ------------------------------------- | ----------------------------------------------------------------------- |
| `scripts/install-local.sh`            | Maintainer reinstall: `cargo install --path .` + plugin + man pages     |
| `scripts/build-distrofs.sh`           | Build (and `--install PREFIX`) the guest `distrofs` file server         |
| `scripts/dev-clean.sh`                | Remove all installed artifacts (pkg, cargo, manual) — `-n` previews     |
| `scripts/build-pkg.sh`                | Build the .pkg + binstall tarball + CycloneDX SBOM under `scratch/pkg/` |
| `scripts/build-pkg-signed.sh`         | Signed+notarized build; sources `packaging/signing.local.conf`          |
| `scripts/upload-release-artifacts.sh` | `vX.Y.Z` — upload the signed artifacts over the CI draft release        |
| `scripts/check-legacy-adoption.sh`    | ≤0.4.2 → journaled upgrade check against the real state dir             |
| `scripts/make-fake-releases.sh`       | Fake GitHub-releases tree for `--update` testing (below)                |
| `scripts/release-gate.sh`             | The step-1 release gate in one command                                  |

## Packaging

`scripts/build-pkg.sh` stages the payload — `cm`, `container-distro`,
the plugin registration, man pages, completions, and the
uninstall/update scripts — under `scratch/pkg/` and produces the
component+product `.pkg` and the `cargo-binstall` tarball. With
`cargo cyclonedx` installed it also produces the CycloneDX SBOM
(`container-distro-X.Y.Z-sbom.cdx.json`), and with `cargo auditable`
the binaries embed their dep tree in a `.dep-v0` section —
`cargo audit bin` can scan an installed copy. Missing plugins are
skipped with a warning for ad-hoc builds; the official release path
requires them. Signing is
auto-detected from the keychain (`--sign` requires it, `--no-sign`
forces the `-unsigned` artifact); `NOTARY_PROFILE` opts into
notarize+staple. The signed path is `scripts/build-pkg-signed.sh`
(reading the gitignored `packaging/signing.local.conf` — see
`signing.conf.example`), then `scripts/upload-release-artifacts.sh
vX.Y.Z` swaps the CI-built unsigned asset on the draft release.

The design — including the options considered and rejected — lives in
[docs/design/packaging.md](docs/design/packaging.md).

### Testing self-update without real releases

`cm --update` / `container distro update` only ever read
`<base>/releases/latest`, `<base>/releases/tags/vX.Y.Z`, and each
asset's `browser_download_url`, so a static file server can
impersonate the GitHub API:

```bash
# 1. Fixture pkgs: a detached worktree with version bumped down gives
#    an authentic pkg whose receipt reads 0.4.5/0.5.0 — payload content
#    doesn't matter to the update flow. CARGO_TARGET_DIR points at the
#    main build dir so only the crate itself recompiles.
git worktree add --detach scratch/wt-fixture HEAD
cd scratch/wt-fixture && sed -i '' 's/^version = ".*"/version = "0.5.0"/' Cargo.toml
CARGO_TARGET_DIR=$PWD/../../target cargo build --release
CARGO_TARGET_DIR=$PWD/../../target scripts/build-pkg.sh --skip-build --no-sign
#    (drop --no-sign when Developer ID identities are in the keychain —
#    that also exercises the signature + team-ID checks)

# 2. Serve fake releases — scripts/make-fake-releases.sh turns pkgs
#    into the manifest tree (tag derived from each filename, real
#    sha256 digests; rename a copy to fake a version, e.g. a signed
#    0.5.0 pkg as container-distro-0.7.0.pkg; -l picks the tag
#    releases/latest serves; --bad-digest corrupts digests for the
#    tamper path — or sed one manifest afterward):
scripts/make-fake-releases.sh -l v0.7.0 \
    scratch/pkg/container-distro-0.4.5-unsigned.pkg \
    scratch/pkg/container-distro-0.5.0.pkg \
    scratch/pkg/container-distro-0.5.0-unsigned.pkg
python3 -m http.server 8123 -d scratch/fake-gh &

# 3. Drive the update, stopping before the exec — the override is
#    honored only behind an explicit --dev (an env var alone mustn't
#    be able to redirect an upgrade):
CONTAINER_DISTRO_RELEASES_URL=http://127.0.0.1:8123 \
    target/debug/cm --update --dry-run --force --dev
```

`CONTAINER_DISTRO_RELEASES_URL` is a **dev-only** seam — it redirects
release lookups, and it's ignored unless `--dev` was passed on this
invocation, so a planted env var can't silently steer an upgrade.
`--dry-run` exercises everything except the Installer.app `exec`; drop
it for the real handoff (Installer.app still asks before installing,
so canceling is safe). Not covered by fixtures: the team-mismatch
prompt (needs a second developer account) and the no-receipt consent
(needs a machine without the pkg installed — `sudo pkgutil --forget`
works if you're willing to re-install after).

## Dev-mode overrides

`--dev` (a global flag on both binaries) is the per-invocation opt-in
for seams that would be backdoors if the environment alone could
enable them — see `.devin/rules/security.md`. Currently gated:

- `CONTAINER_CLI` — which `container` binary every spawned call execs.
  To test against an unreleased `container` build:
  `CONTAINER_CLI=/path/to/dev/container cm --dev -l` (same for
  `container-distro --dev …`). Values equal to the default resolution
  (`container`, `/usr/local/bin/container`) are honored without
  `--dev` — they can't redirect anything. Ignored values warn once.
- `CONTAINER_DISTRO_RELEASES_URL` — the release-API fixture above.
- `DISTROFS` — which distrofs client binary `fs` execs with the
  session token (bare `distrofs` = PATH lookup, always honored).

## Releases

`cargo release` driven, `vX.Y.Z` tags plus `release/X.Y` maintenance
branches; pushing a tag runs `.github/workflows/release.yml` to draft
the GitHub Release with the built artifacts. The full process —
versioning rules, the gate, the legacy-adoption check, crates.io
publish — is [docs/design/release.md](docs/design/release.md).

## Where things live

- `docs/README.md` — index of all documentation
- `docs/design/security.md` — the threat-model register; required
  reading before touching anything that crosses the host/guest
  boundary or runs with privilege (installer paths included)
- `docs/TODO.md` — the working roadmap

[`container`]: https://github.com/apple/container

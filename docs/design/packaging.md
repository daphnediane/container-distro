# Packaging (#14)

How releases reach users, and why. Decided: a **macOS Installer `.pkg`**
plus a **`cargo-binstall`-compatible tarball** attached to each GitHub
Release by `.github/workflows/release.yml`, alongside
`THIRD-PARTY-NOTICES.yaml` and a generated **CycloneDX SBOM**; `cargo
install` from crates.io stays a first-class path.

## Options considered

### Rust-ecosystem

- **`cargo install` (crates.io)** — the baseline; keeps working
  unchanged. Post-steps (`install-plugin`, `install-man`,
  `install-completions`) stay manual, as documented in
  [install.md](../install.md).
- **Prebuilt binaries + `cargo-binstall`** — `cargo binstall
  container-distro` downloads a release tarball instead of compiling.
  It reads the crate's crates.io metadata, follows `repository` to the
  GitHub release, and picks the asset matching
  `[package.metadata.binstall]` in `Cargo.toml`:
  `{name}-{target}-v{version}.tar.gz` with the binaries under a
  same-named inner dir. Same manual post-steps as `cargo install` —
  binstall only drops binaries into `~/.cargo/bin`.
- **`cargo-dist`** — rejected: it replaces the hand-rolled release
  workflow with generated ones, and its `pkg` installer is a plain
  component package — no `hostArchitectures`/`allowed-os-versions`
  gating or distribution UI of our own.
- **`cargo-bundle`/`cargo-wix`/`cargo-deb`** — other platforms and
  formats; none build macOS pkgs.

### The `.pkg`

`pkgbuild` builds a component package from a staged payload root;
`productbuild --distribution` wraps it in a product archive with a
Distribution XML (`packaging/Distribution.xml`) that supplies the
Installer.app UI (welcome/license/conclusion), the
`hostArchitectures="arm64"` and macOS ≥ 26 requirements (the floor for
`container` itself), and `rootVolumeOnly`. The license screen is
generated at build time: `LICENSE` plus each vendored crate's license
expression, with the full texts staying in `THIRD-PARTY-NOTICES.yaml`
(payload + release asset).

The payload is **everything, no scripts**: `container-distro`, the
`cm` alias script, the `container distro` plugin registration
(`libexec/container-plugins/distro/`), man pages, completions, and the
license/notices files under `/usr/local` — all generated at build time
by the binary's own `install-all --prefix /usr/local --stage <payload>`
(the alias runs `/usr/local/bin/container-distro`, not the staging
copy), so the pkg can't drift from what a manual install produces.
("No scripts" means no installer scripts; the `cm` alias is a payload
file.) Zero install scripts means the installer adds
no code-execution surface of its own (security.md T9).

Upgrades obsolete: for a same-identifier, higher-version install,
Installer removes files the old receipt owned that the new payload
doesn't — so a removed/renamed subcommand's man page can't go stale.
(`container-distro install-man` prunes stale generated pages anyway,
for the non-pkg paths.)

Uninstall has no pkg mechanism, so the payload ships
`uninstall-container-distro.sh` in `bin/` — modeled on Apple's own
`uninstall-container.sh`. It removes exactly the files the pkg receipt
owns (`pkgutil --only-files`), rmdir's now-empty dirs deepest-first
(`--only-dirs`), and `--forget`s the receipt. `sudo` is applied
per-command and only when the target's parent dir isn't already
writable (sudo_if), so a user-owned prefix never prompts. Like Apple's
script it requires `-k` (keep) or `-d` (delete) for per-user state
under `~/Library/Application Support/container-distro`; under `sudo` it
resolves `SUDO_USER`'s home, not root's. `-d` first runs
`container-distro delete --all -f` as the invoking user (root's
`container` sees a different appRoot) — but only when `container`
services are already running, since `system status` is read-only and a
stopped service can't be queried anyway. Whatever can't be reached
stays in `container`'s storage: plain machines are `container`'s own
data and are never touched. `-n` adds a dry run: every command the
script would run (sudo/user prefixes included) is printed instead.

Update is likewise script-shaped — `update-container-distro.sh` in
`bin/` — but only as a thin wrapper: the real path is
`container distro update` / `cm --update` in the binary itself
(`src/setup/update.rs`). It fetches the latest (or a named) GitHub release,
prefers the signed pkg, verifies the API-reported SHA-256 plus the
Developer ID signature and a signing-team match against the installed
binary's `TeamIdentifier`, then `exec`s `open -a Installer.app` so the
process is replaced before the privileged install runs. Downgrades,
the unsigned CI build, a foreign signer, and non-pkg installs prompt
first (`--force` skips); no `container` services need stopping — the
installer only swaps files under the install root.

## SBOM (#28)

`build-pkg.sh` also emits a CycloneDX 1.5 JSON SBOM,
`container-distro-<version>-sbom.cdx.json`, via `cargo cyclonedx
--target aarch64-apple-darwin` — the dependency tree of the shipped
binaries: dev-deps are omitted and build-only deps (e.g. `cc`) are
inventoried with `scope: excluded`. It's the consumer-side half of the
supply-chain story — grype/osv-scanner/Dependency-Track can match CVEs
against the shipped dep tree without re-deriving it — and it
complements `THIRD-PARTY-NOTICES.yaml`, which stays the carrier for
the license texts those licenses require reproducing. Since
`Cargo.lock` is committed, every machine generates the same tree, so
the SBOM has no signed/unsigned variant: the single CI-generated file
serves. The workflow attaches it by explicit filename — it can't match
the `container-distro-*.pkg`/`*.tar.gz` globs, but explicit is the
lesson of the component-pkg leak anyway.

`cargo-cyclonedx` (OWASP's plugin) was picked over `cargo sbom` for its
target/feature/dep-kind filtering — it derives the tree from `cargo
metadata`, not just `Cargo.lock` — and over language-agnostic scanners
(syft et al.) which can't distinguish shipped deps from build-time
ones.

The binaries additionally carry the dep tree themselves: `build-pkg.sh`
prefers `cargo auditable build`, which injects the resolved tree
(zlib-compressed JSON in a `.dep-v0` linker section) into each binary.
`cargo audit bin` (already a gate dependency), `rust-audit-info`, or
`auditable2cdx` can then inventory an *installed* binary — no release
asset required — and the section survives `strip = "symbols"` and
pre-codesign since it's injected at link time. This is the interim
stand-in until Cargo grows native SBOM embedding
(rust-lang/rfcs#3553); when that lands, `cargo build` alone produces
the same property and the wrapper can drop.

Both plugins are **optional for `build-pkg.sh`** — a missing one is
reported and skipped, keeping ad-hoc and fixture builds (the
self-update test tree) free of supply-chain-tooling requirements. The
**official artifacts always carry the metadata**: `release.yml`
installs the pinned tools, `build-pkg-signed.sh` preflights them, and
the script reports a binary lacking `.dep-v0` so a `--skip-build`
reuse can't silently swap in a plain-cargo build.

## The `container` dependency

`container` is a hard prerequisite, but the pkg does **not** install
it. Options weighed:

- **Bundle Apple's signed pkg** (~112MB) as a second component with a
  conditional choice — fully offline/native, but huge and hard-pins a
  `container` release.
- **Postinstall downloads + `installer -pkg`** — works (nested installs
  are an established pattern) but hides a 112MB download inside a root
  script with no progress UI, and re-implements the pinned-hash +
  `pkgutil --check-signature` verification in shell.
- **Postinstall `open`s the verified pkg for the console user** —
  Installer.app drives the Apple install. Possible upgrade if the
  first-run offer proves too late.
- **Existing first-run offer** (chosen): an interactive
  `cm`/`container-distro` already downloads the SHA-256-pinned,
  signature-checked Apple installer and opens it in Installer.app
  (`src/container/install.rs`, security.md T8). Nothing container-related happens
  during our install — and none needs to.

## Signing and notarization

CI has no keychain, so the workflow's build produces
`container-distro-<version>-unsigned.pkg` and
`container-distro-<target>-v<version>-unsigned.tar.gz` — Apple's own
`container` releases use the same signed/unsigned naming pair for the
pkg, and the tarball follows it. Mangling the names means the
canonical assets only ever appear in their final signed form: a
signed upload adds assets rather than mutating ones already on the
release, even if an unsigned build got published. The signed pkg
(`container-distro-<version>.pkg`) and canonical tarball are produced
by running `scripts/build-pkg.sh` on the cert-holding machine and
uploading them over the draft release: `productsign` on the CI
artifact is *not* enough — it would sign the outer pkg but leave the
binaries unsigned, which notarization rejects.

Even unsigned or unnotarized, the pkg installs via `sudo installer
-pkg` or right-click → Open (Gatekeeper), and tarball binaries aren't
quarantined anyway (CLI downloads carry no quarantine xattr).
`build-pkg.sh` auto-detects the pieces when a Developer ID exists —
`--sign` makes signing a hard requirement (fails without the
identities), `--no-sign` forces the `-unsigned` artifact — so no env
or script changes are needed either way. For the maintainer,
`scripts/build-pkg-signed.sh` wraps it: it sources the gitignored
`packaging/signing.local.conf` (see `signing.conf.example`) for the
identities + profile, and `scripts/upload-release-artifacts.sh`
uploads the result over the draft release:

| Piece        | Identity                                                     | Tool                                          |
| ------------ | ------------------------------------------------------------ | --------------------------------------------- |
| binaries     | `Developer ID Application` (hardened runtime, `--timestamp`) | `codesign`                                    |
| `.pkg`       | `Developer ID Installer`                                     | `productbuild --sign`                         |
| tarball      | `Developer ID Application` detached signature (`.sig`)       | `codesign --detached --timestamp`             |
| tarball      | `GPG_KEY_ID` armored signature (`.asc`, optional)            | `gpg --detach-sign`                           |
| notarization | `NOTARY_PROFILE` keychain profile                            | `notarytool submit --wait` + `stapler staple` |

The tarball's `.sig` is a detached code signature of the archive
itself -- `codesign` signs arbitrary files when `--detached` is used --
so it verifies with `codesign --verify --detached=<file>.sig <file>`
against the same Apple-PKI chain as the binaries inside; the `.asc`
(GPG, configured via `GPG_KEY_ID` in `signing.local.conf`) covers the
crowd that expects a PGP signature on a tarball. Both are detached
sidecars of the canonical tarball — the
`{name}-{target}-v{version}.tar.gz` name is fixed by the binstall
metadata, which is also why unsigned builds mangle the archive name
(`-unsigned.tar.gz`) instead of shipping the canonical name unsigned
(`cargo binstall` then falls back to building from source — an
unsigned tarball isn't worth presenting as canonical). The README's
Verifying downloads section lists the expected identities;
`upload-release-artifacts.sh` verifies the `.sig` (and `.asc` when
present) before uploading them over the draft release, and deletes
the `-unsigned` pkg and tarball the CI build attached. Two provenance
guards close the loop (#50): a `build-pkg.sh` run that will sign
doesn't build the checkout at all — it re-enters itself inside a
`git worktree` of the `v<version>` tag under `scratch/`, so
uncommitted changes and a checkout moved mid-build can't contaminate
signed artifacts (`--allow-dirty` signs the working tree for test
builds; unsigned runs build in place with a warning) — and
`upload-release-artifacts.sh` refuses to touch a release that is no
longer a draft.

Setup (a paid Developer Program membership is required; an "Apple
Development" cert from a free account is *not* usable — Gatekeeper
treats it like unsigned and the notary service rejects it):

1. Xcode → Settings → Accounts → *Manage Certificates* → `+` → create
   **Developer ID Application** and **Developer ID Installer** (both
   land in the login keychain; `build-pkg.sh` finds them).
2. `xcrun notarytool store-credentials <profile> --apple-id …` →
   `NOTARY_PROFILE=<profile>` opts the build into notarize + staple.

Both certs are needed for a clean chain: notarization rejects
unsigned/hardened-runtime-less binaries inside a signed pkg. If CI
signing is ever wanted, the same env vars apply with the cert imported
into a temp keychain from a base64 `.p12` secret — currently unwired
on purpose: the private key stays on the maintainer's Mac, and signed
artifacts are uploaded at draft-review time.

/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Self-update: download a release `.pkg` and hand it to Installer.app.
//!
//! `cm --update` / `container distro update` (the `wsl --update`
//! analog) fetch a GitHub Release — `latest`, or an explicit `vX.Y.Z`
//! tag — pick the signed `container-distro-<ver>.pkg` asset (the
//! unsigned CI build needs an explicit yes), and verify it: the SHA-256
//! the API reports for the asset, a `pkgutil --check-signature`
//! Developer ID, and a team-ID match against the installed binary's
//! `TeamIdentifier` when there's a baseline to compare against.
//!
//! The install itself is an `exec` into `open -a Installer.app`: our
//! process is *replaced*, so nothing of ours can run while the
//! privileged install proceeds, and Installer.app collects the admin
//! password and re-verifies notarization itself — the T8 model. We
//! never invoke `sudo installer` (the CLI can't self-elevate anyway —
//! that's why Apple's script wraps it in `sudo`); when `open` can't be
//! exec'd we bail with the verified path and the manual command.
//!
//! Nothing needs stopping first: the binaries are per-invocation, not
//! daemons, and running distros mount their own asset copies — the
//! installer only swaps files under the install root.

use std::cmp::Ordering;
use std::io::{self, IsTerminal, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use tempfile::NamedTempFile;

use crate::catalog::wsl;
use crate::container::install;
use crate::setup::privilege;

/// The GitHub repo releases come from — matches Cargo.toml's
/// `repository`, restated here so a typo can't redirect the download.
/// `CONTAINER_DISTRO_RELEASES_URL` overrides the API base for testing:
/// point it at a local server whose layout mirrors
/// `<base>/releases/latest`, `<base>/releases/tags/vX.Y.Z`, and the
/// asset `browser_download_url`s (see DEV.md). Honored only with the
/// explicit `--dev` flag, so an env var alone can't redirect an
/// upgrade at an untrusted server.
const REPO: &str = "daphnediane/container-distro";
const RELEASES_URL_ENV: &str = "CONTAINER_DISTRO_RELEASES_URL";
/// Our pkg identifier — what `pkgutil` answers for.
const PKG_ID: &str = "io.github.daphnediane.container-distro";
/// Explicit `-a` target for `open` so a remapped `.pkg` LaunchServices
/// binding can't send the installer elsewhere. This is the real path
/// on macOS 26+ — `/System/Applications/Utilities/` doesn't have one.
const INSTALLER_APP: &str = "/System/Library/CoreServices/Installer.app";

/// `cm --update` / `container distro update`: update the installed
/// container-distro to `version` (default: the latest release).
/// `dry_run` resolves and verifies everything, then reports instead of
/// installing. The `CONTAINER_DISTRO_RELEASES_URL` fixture-server
/// override applies only under `--dev` — an env var alone mustn't
/// redirect release downloads, or a shell-profile line becomes a
/// backdoor. Never returns on a real success — the process `exec`s
/// into `open`; an early `Ok(())` means there was nothing to do or the
/// user declined.
pub fn update(version: Option<&str>, force: bool, dry_run: bool) -> Result<()> {
    if privilege::is_root() {
        bail!("run without sudo — Installer.app asks for admin rights itself");
    }
    let (tag, release) = fetch_release(version)?;
    let current = env!("CARGO_PKG_VERSION");
    match cmp_version(current, &tag) {
        Ordering::Equal if !force => {
            println!("container-distro {tag} is already installed (--force to reinstall)");
            return Ok(());
        }
        Ordering::Equal => eprintln!("reinstalling container-distro {tag}"),
        Ordering::Greater => consent(
            &format!("{tag} is older than the installed {current}"),
            &format!("Downgrade container-distro to {tag}? [y/N] "),
            force,
        )?,
        Ordering::Less => eprintln!("updating container-distro {current} -> {tag}"),
    }

    // A pkg receipt means the pkg owns the install we're updating.
    // Without one this looks like a cargo/dev install — laying the pkg
    // down anyway creates a second copy under /usr/local, so it needs
    // an explicit yes.
    let root = receipt_root();
    if root.is_none() {
        consent(
            "no pkg receipt — this looks like a cargo/manual install (`cargo install \
             --locked container-distro` or `cargo binstall` is that update path)",
            "Install the .pkg anyway? [y/N] ",
            force,
        )?;
    }

    let asset = pick_asset(&release, &tag, force)?;
    let pkg = download_pkg(&asset)?;
    let pkg_team = if asset.signed {
        install::check_signature(&pkg)?
    } else {
        // Consent for unsigned was collected at asset pick.
        None
    };
    if let Some(root) = &root
        && let (Some(installed), Some(downloaded)) = (installed_team_id(root), pkg_team.as_deref())
        && installed != downloaded
    {
        consent(
            &format!(
                "the {tag} package is signed by team {downloaded}, but the installed copy is signed by {installed}"
            ),
            "Install it anyway? [y/N] ",
            force,
        )?;
    }
    if dry_run {
        println!("dry run — would install {tag} from {}", pkg.display());
        return Ok(());
    }
    exec_installer(&pkg, &tag)
}

/// An interactive yes, or `--force`. The context prints either way —
/// `--force` shouldn't hide *why* this needed confirming.
/// Non-interactive sessions get an error naming --force rather than a
/// prompt that reads EOF as "no".
fn consent(context: &str, question: &str, force: bool) -> Result<()> {
    eprintln!("{context}");
    if force {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        bail!("that needs a yes — rerun on a terminal or pass --force");
    }
    if !install::prompt_yes(question)? {
        bail!("declined");
    }
    Ok(())
}

/// `tag` minus the `v` prefix — release tags carry one, versions don't.
fn strip_v(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// The releases API base — api.github.com normally. The env override
/// is the test seam that lets a local static server impersonate it,
/// and it's honored only behind an explicit `--dev` on this
/// invocation: a bare env var could be planted in a shell profile to
/// redirect the next `cm --update`, which must not silently work.
fn releases_base() -> String {
    let base = format!("https://api.github.com/repos/{REPO}");
    let Ok(u) = std::env::var(RELEASES_URL_ENV) else {
        return base;
    };
    if u.is_empty() {
        return base;
    }
    if !crate::dev_mode() {
        eprintln!("ignoring {RELEASES_URL_ENV} (pass --dev to honor it)");
        return base;
    }
    eprintln!("--dev: release lookups redirect to {u}");
    u.trim_end_matches('/').to_string()
}

/// `(version, release JSON)` for `latest` or an explicit tag.
fn fetch_release(version: Option<&str>) -> Result<(String, Value)> {
    let base = releases_base();
    let (url, label) = match version {
        None => (
            format!("{base}/releases/latest"),
            "the latest release".to_string(),
        ),
        Some(v) => {
            let tag = format!("v{}", strip_v(v));
            (
                format!("{base}/releases/tags/{tag}"),
                format!("release {tag}"),
            )
        }
    };
    let mut resp = match wsl::probe_agent().get(&url).call() {
        Ok(r) => r,
        Err(e) => bail!("cannot fetch {label} from GitHub: {e}"),
    };
    let mut text = String::new();
    resp.body_mut()
        .as_reader()
        .take(8 << 20)
        .read_to_string(&mut text)
        .context("cannot read the release response")?;
    let json: Value = serde_json::from_str(&text).context("release response is not JSON")?;
    let tag = json
        .get("tag_name")
        .and_then(Value::as_str)
        .map(strip_v)
        .with_context(|| format!("{label} has no tag_name"))?
        .to_string();
    Ok((tag, json))
}

/// `X.Y.Z[-suffix]`: the numeric triple first, then a prerelease sorts
/// before its release (`0.6.0-dev` < `0.6.0`). Two prerelease suffixes
/// compare lexically — `dev` < `rc.N` — close enough for our tags.
fn cmp_version(a: &str, b: &str) -> Ordering {
    fn split(v: &str) -> ([u64; 3], Option<&str>) {
        let (core, pre) = match strip_v(v).split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (strip_v(v), None),
        };
        let mut nums = [0u64; 3];
        for (n, part) in core.split('.').take(3).enumerate() {
            nums[n] = part.parse().unwrap_or(0);
        }
        (nums, pre)
    }
    let (an, ap) = split(a);
    let (bn, bp) = split(b);
    an.cmp(&bn).then_with(|| match (ap, bp) {
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (x, y) => x.cmp(&y),
    })
}

/// The pkg receipt's install root (`pkgutil --pkg-info`), normalized
/// like the uninstaller: `location:` joins under `/`, and the
/// `/`-recorded location of the earliest builds falls back to
/// /usr/local. `None` when the receipt is absent (cargo/manual
/// installs).
fn receipt_root() -> Option<PathBuf> {
    let out = Command::new("/usr/sbin/pkgutil")
        .args(["--pkg-info", PKG_ID])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let loc = text
        .lines()
        .find_map(|l| l.strip_prefix("location:").map(str::trim))?;
    let root = PathBuf::from(format!("/{}", loc.trim_matches('/')));
    Some(if root == Path::new("/") {
        PathBuf::from("/usr/local")
    } else {
        root
    })
}

struct Asset {
    name: String,
    url: String,
    /// The asset's `digest` field minus its `sha256:` prefix.
    sha256: Option<String>,
    signed: bool,
}

/// The release asset called `name`, or `None`.
fn find_asset(release: &Value, name: &str) -> Option<Asset> {
    release.get("assets")?.as_array()?.iter().find_map(|a| {
        if a.get("name")?.as_str()? != name {
            return None;
        }
        Some(Asset {
            name: name.to_string(),
            url: a.get("browser_download_url")?.as_str()?.to_string(),
            sha256: a
                .get("digest")
                .and_then(Value::as_str)
                .and_then(|d| d.strip_prefix("sha256:"))
                .map(str::to_string),
            signed: !name.ends_with("-unsigned.pkg"),
        })
    })
}

/// The signed pkg when the release has one; the unsigned CI build only
/// with an explicit yes (same rule as Apple's update-container.sh).
fn pick_asset(release: &Value, tag: &str, force: bool) -> Result<Asset> {
    if let Some(a) = find_asset(release, &format!("container-distro-{tag}.pkg")) {
        return Ok(a);
    }
    let unsigned = format!("container-distro-{tag}-unsigned.pkg");
    match find_asset(release, &unsigned) {
        Some(a) => {
            consent(
                &format!("release {tag} has no signed package — only the unsigned CI build"),
                "Install the unsigned package? [y/N] ",
                force,
            )?;
            Ok(a)
        }
        None => bail!("release {tag} has no container-distro .pkg asset"),
    }
}

/// The downloaded pkg in the per-user cache, verified against the
/// API-reported digest. A cached copy is re-verified before reuse —
/// same rule as the `.wsl` cache and the installer offer.
fn download_pkg(asset: &Asset) -> Result<PathBuf> {
    let dir = wsl::app_cache_dir().unwrap_or_else(std::env::temp_dir);
    let path = dir.join(&asset.name);
    if path.is_file()
        && let Some(d) = &asset.sha256
        && install::sha256_of(&path).ok().as_deref() == Some(d.as_str())
    {
        eprintln!("{}: already downloaded", asset.url);
        return Ok(path);
    }
    let mut tmp = NamedTempFile::new_in(&dir).context("cannot create a temp file")?;
    wsl::download_to(&asset.url, tmp.as_file_mut())?;
    if let Some(d) = &asset.sha256 {
        let got = install::sha256_of(tmp.path())?;
        if got != *d {
            bail!(
                "SHA-256 mismatch for {}: expected {d}, got {got}",
                asset.url
            );
        }
    }
    tmp.persist(&path)
        .map_err(|e| anyhow!(e.error))
        .with_context(|| format!("cannot move the download to {}", path.display()))?;
    Ok(path)
}

/// `TeamIdentifier` of the installed binary under the receipt root.
/// `codesign -dvv` reports `not set` for adhoc/unsigned copies, which
/// reads as `None` — no baseline to compare against.
fn installed_team_id(root: &Path) -> Option<String> {
    let out = Command::new("/usr/bin/codesign")
        .arg("-dvv")
        .arg(root.join("bin/container-distro"))
        .output()
        .ok()?;
    // `codesign -dvv` writes the info to stderr.
    let text = String::from_utf8_lossy(&out.stderr);
    let id = text
        .lines()
        .find_map(|l| l.strip_prefix("TeamIdentifier=").map(str::trim))?;
    (id != "not set").then(|| id.to_string())
}

/// Replace this process with the Installer.app handoff: `exec` never
/// returns on success, so nothing of ours runs while the privileged
/// install proceeds. When `open` can't be exec'd (no window server,
/// e.g. over SSH), bail with the verified path and the command to run.
fn exec_installer(pkg: &Path, tag: &str) -> Result<()> {
    eprintln!(
        "installing container-distro {tag} — {} opens in Installer.app",
        pkg.display()
    );
    let err = Command::new("/usr/bin/open")
        .arg("-a")
        .arg(INSTALLER_APP)
        .arg(pkg)
        .exec();
    Err(err).with_context(|| {
        format!(
            "could not launch Installer.app; install the verified package manually:\n  \
            sudo installer -pkg {} -target /",
            pkg.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(assets: &[(&str, &str)]) -> Value {
        serde_json::json!({
            "tag_name": "v0.5.0",
            "assets": assets.iter().map(|(name, digest)| serde_json::json!({
                "name": name,
                "browser_download_url": format!("https://example/{name}"),
                "digest": digest,
            })).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn version_ordering() {
        assert_eq!(cmp_version("0.5.0", "0.5.0"), Ordering::Equal);
        assert_eq!(cmp_version("0.5.0", "v0.5.0"), Ordering::Equal);
        assert_eq!(cmp_version("0.5.0", "0.6.0"), Ordering::Less);
        assert_eq!(cmp_version("0.6.0-dev", "0.5.0"), Ordering::Greater);
        assert_eq!(cmp_version("0.6.0-dev", "0.6.0"), Ordering::Less);
        assert_eq!(cmp_version("0.6.0-rc.1", "0.6.0"), Ordering::Less);
        assert_eq!(cmp_version("0.6.0-dev", "0.6.0-rc.1"), Ordering::Less);
    }

    #[test]
    fn pick_prefers_signed() {
        let r = release(&[
            (
                "container-distro-0.5.0-unsigned.pkg",
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            (
                "container-distro-0.5.0.pkg",
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
            ("container-distro-aarch64-apple-darwin-v0.5.0.tar.gz", ""),
            ("THIRD-PARTY-NOTICES.yaml", ""),
        ]);
        let a = pick_asset(&r, "0.5.0", false).unwrap();
        assert!(a.signed);
        assert!(a.url.ends_with("container-distro-0.5.0.pkg"));
        assert_eq!(
            a.sha256.as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
    }

    #[test]
    fn unsigned_fallback_needs_force_off_tty() {
        let r = release(&[(
            "container-distro-0.5.0-unsigned.pkg",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )]);
        // No signed asset: force takes the unsigned one; on a tty-less
        // test runner without force it's a refusal, not a prompt.
        let a = pick_asset(&r, "0.5.0", true).unwrap();
        assert!(!a.signed);
        assert!(pick_asset(&r, "0.5.0", false).is_err());
    }

    #[test]
    fn no_pkg_asset_errors() {
        let r = release(&[(
            "container-distro-aarch64-apple-darwin-v0.5.0.tar.gz",
            "sha256:cc",
        )]);
        assert!(pick_asset(&r, "0.5.0", true).is_err());
    }
}

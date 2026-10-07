/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Reading `.wsl` distribution packages.
//!
//! A `.wsl` file (WSL 2.4.4+) is a tar — optionally gzipped — holding a
//! rootfs at the tar root plus `/etc/wsl-distribution.conf`, an INI the
//! distro ships to describe itself. Its `[oobe]` section carries
//! `defaultName` (the suggested registration name), `defaultUid`, and a
//! first-run `command`. We honor `defaultName` only: distros provision
//! the host user (uid is ours), and running a distro-supplied command
//! during install is a surface we don't take on silently — see
//! security.md T4.

use std::fs::{self, File, TryLockError};
use std::io::{BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::naming::APP_NAME;

const CONF_PATH: &str = "etc/wsl-distribution.conf";

/// Cap on the `.wsl` download cache. macOS may reclaim the directory
/// under storage pressure but eviction isn't scheduled, so we prune
/// oldest-first past this size ourselves.
const MAX_CACHE_BYTES: u64 = 8 << 30;

/// The `[oobe] defaultName` from `file`'s `etc/wsl-distribution.conf`,
/// or `None` when the tar has no manifest (plain rootfs tar) or no
/// name. Accepts a raw or gzipped tar — compression is sniffed, so the
/// file extension doesn't matter.
pub fn distribution_name(file: &Path) -> Result<Option<String>> {
    let mut f = File::open(file).with_context(|| format!("cannot open {}", file.display()))?;
    let mut magic = [0u8; 2];
    let gz = f.read(&mut magic)? == 2 && magic == [0x1f, 0x8b];
    f.rewind()?;
    let reader: Box<dyn Read> = if gz {
        Box::new(GzDecoder::new(BufReader::new(f)))
    } else {
        Box::new(BufReader::new(f))
    };
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries().context("cannot read tar")? {
        let entry = entry.context("cannot read tar entry")?;
        let path = entry.path().context("cannot read tar entry path")?;
        let normalized = path.strip_prefix(".").unwrap_or(&path);
        if normalized == Path::new(CONF_PATH) {
            let mut text = String::new();
            entry
                .take(64 * 1024)
                .read_to_string(&mut text)
                .context("cannot read wsl-distribution.conf")?;
            return Ok(parse_default_name(&text));
        }
    }
    Ok(None)
}

/// Pull `defaultName` out of the `[oobe]` section of INI text.
/// Sections and keys are matched case-insensitively.
fn parse_default_name(ini: &str) -> Option<String> {
    let mut in_oobe = false;
    for line in ini.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            in_oobe = line
                .trim_matches(['[', ']'])
                .trim()
                .eq_ignore_ascii_case("oobe");
            continue;
        }
        if in_oobe
            && let Some((k, v)) = line.split_once('=')
            && k.trim().eq_ignore_ascii_case("defaultname")
        {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// A fetched `.wsl` ready to import. While it lives it holds a shared
/// flock on the cache's `<sha256>.lock` file (same per-file pattern as
/// the distro locks), so `purge_cache`/`prune_cache` can't remove the
/// file mid-import — the lock releases on drop or process exit.
#[derive(Debug)]
pub struct Fetched {
    path: PathBuf,
    _lock: Option<File>,
}

impl Fetched {
    /// The verified file to import; valid only while `self` is alive.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Fetch `url` (a `.wsl` rootfs) and verify it against `sha256` (hex;
/// a `0x` prefix, as in Microsoft's manifest, is stripped).
///
/// Verified files are kept in a content-addressed cache
/// (`<sha256>.wsl` under the Darwin per-user cache dir — see
/// [`cache_dir`]); a hit is reused only after re-verification, so a
/// corrupt or planted entry just costs a re-download. If the cache is
/// unusable the download lands in a temp file instead.
///
/// Concurrency: an exclusive flock on `<sha256>.lock` serializes
/// download+verify+persist per hash — a second fetch waits, then finds
/// the verified file and skips its own download. The returned
/// [`Fetched`] keeps a shared lock so consumers (purge, prune) wait
/// for the import rather than deleting the file out from under it.
/// `name` is the catalog NAME the download came from; it's recorded in
/// the lockfile (see [`CacheMeta`]) so the cache listing can identify
/// the file without consulting the catalog again.
pub fn fetch(url: &str, sha256: &str, name: &str) -> Result<Fetched> {
    let expected = sha256
        .strip_prefix("0x")
        .or_else(|| sha256.strip_prefix("0X"))
        .unwrap_or(sha256)
        .to_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid SHA-256 {sha256:?} for {url}");
    }
    let Some(dir) = cache_dir() else {
        let mut tmp = NamedTempFile::new().context("cannot create temp file")?;
        download_to(url, tmp.as_file_mut())?;
        verify_sha256(tmp.path(), &expected, url)?;
        let path = tmp.keep().map_err(|e| anyhow!(e.error))?.1;
        return Ok(Fetched { path, _lock: None });
    };
    let dest = dir.join(format!("{expected}.wsl"));
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(format!("{expected}.lock")))
        .context("cannot open cache lockfile")?;
    lock.lock().context("cannot lock cache entry")?;
    // Exclusive: nobody else is downloading or deleting this file. A
    // hit is still re-verified — the hash is the trust root.
    if !dest.is_file() || !file_sha256(&dest).is_ok_and(|h| h == expected) {
        if dest.is_file() {
            eprintln!("{url}: cache entry failed verification — re-downloading");
            let _ = fs::remove_file(&dest);
        }
        let mut tmp = NamedTempFile::new_in(&dir).context("cannot create temp file")?;
        download_to(url, tmp.as_file_mut())?;
        verify_sha256(tmp.path(), &expected, url)?;
        tmp.persist(&dest)
            .map_err(|e| anyhow!(e.error))
            .with_context(|| format!("cannot move download to {}", dest.display()))?;
    } else {
        eprintln!("{url}: already cached");
    }
    write_meta(&lock, url, name);
    // Downgrade to shared — same-fd conversion, so there is no window
    // for a purge to slip in. prune() try_locks and skips held files.
    lock.lock_shared().context("cannot downgrade cache lock")?;
    prune_cache();
    Ok(Fetched {
        path: dest,
        _lock: Some(lock),
    })
}

fn verify_sha256(path: &Path, expected: &str, url: &str) -> Result<()> {
    eprintln!("  verifying SHA-256 …");
    let got = file_sha256(path)?;
    if got != expected {
        bail!("SHA-256 mismatch for {url}: expected {expected}, got {got}");
    }
    Ok(())
}

/// The content-addressed `.wsl` cache: `confstr(_CS_DARWIN_USER_CACHE_DIR)`
/// (`/var/folders/…/C/<APP_NAME>/wsl`), the OS-managed per-user cache —
/// excluded from backups, counted as purgeable space. `None` when the
/// directory can't be resolved or created; `fetch` falls back to a
/// temp file.
#[must_use]
pub fn cache_dir() -> Option<PathBuf> {
    let base = unsafe {
        let mut buf = vec![0u8; 1024];
        let n = libc::confstr(
            libc::_CS_DARWIN_USER_CACHE_DIR,
            buf.as_mut_ptr().cast(),
            buf.len(),
        );
        if n == 0 || n >= buf.len() {
            return None;
        }
        PathBuf::from(String::from_utf8_lossy(&buf[..n - 1]).into_owned())
    };
    let dir = base.join(APP_NAME).join("wsl");
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// GET `url` into `out` with ureq (rustls + the platform trust store,
/// so the macOS keychain applies, and `http_proxy`-style env vars).
/// Retries twice, rewinding `out` between attempts, with a one-line
/// progress meter on stderr.
fn download_to(url: &str, out: &mut File) -> Result<()> {
    eprintln!("Downloading {url}");
    let mut last = anyhow!("no attempts made");
    for attempt in 1..=3 {
        match try_download(url, out) {
            Ok(()) => {
                eprintln!();
                return Ok(());
            }
            Err(e) => {
                last = e;
                if attempt < 3 {
                    eprintln!("\n  retry {attempt}/3: {last:#}");
                }
            }
        }
    }
    Err(last.context(format!("download of {url} failed")))
}

fn try_download(url: &str, out: &mut File) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(ureq::Proxy::try_from_env())
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into();
    let mut resp = agent
        .get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    out.rewind()?;
    out.set_len(0)?;
    let total = resp
        .headers()
        .get("Content-Length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let mut reader = resp.body_mut().as_reader();
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    // Progress every ~half second — visible feedback whether the
    // download takes seconds or minutes, without spamming the terminal.
    let mut last_print = Instant::now() - std::time::Duration::from_secs(1);
    loop {
        let n = reader.read(&mut buf).context("read failed mid-download")?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        if last_print.elapsed() >= std::time::Duration::from_millis(500) {
            match total {
                Some(t) => eprint!("\r  {} / {} MiB", done >> 20, t >> 20),
                None => eprint!("\r  {} MiB", done >> 20),
            }
            last_print = Instant::now();
        }
    }
    match total {
        Some(t) => eprint!("\r  {} / {} MiB", done >> 20, t >> 20),
        None => eprint!("\r  {} MiB", done >> 20),
    }
    Ok(())
}

/// Provenance for a cached `.wsl`, stored as JSON inside the file's
/// `<sha256>.lock`. Written under the exclusive lock at fetch time, so
/// `--list --cache` doesn't need the catalog to say what a file is —
/// and can still identify it after the catalog points that entry at a
/// newer SHA-256.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CacheMeta {
    pub url: String,
    pub name: String,
}

fn write_meta(lock: &File, url: &str, name: &str) {
    let Ok(json) = serde_json::to_string(&CacheMeta {
        url: url.to_string(),
        name: name.to_string(),
    }) else {
        return;
    };
    let _ = lock.set_len(0);
    let _ = (&mut &*lock).rewind();
    let _ = (&mut &*lock).write_all(json.as_bytes());
}

/// The [`CacheMeta`] recorded for `path` (a `<sha256>.wsl`), read from
/// its `.lock` sidecar. `None` when there's no sidecar or it isn't
/// ours — e.g. a manually placed file, or one cached before metadata.
fn read_meta(path: &Path) -> Option<CacheMeta> {
    let mut buf = String::new();
    File::open(path.with_extension("lock"))
        .ok()?
        .take(4096)
        .read_to_string(&mut buf)
        .ok()?;
    serde_json::from_str(&buf).ok()
}

/// One file in the `.wsl` download cache.
#[derive(Debug)]
pub struct CacheEntry {
    /// The SHA-256 the file is stored under (its filename stem).
    pub sha256: String,
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
    /// Recorded source URL and catalog name, if the file was fetched
    /// by `fetch` (rather than placed in the cache by hand).
    pub meta: Option<CacheMeta>,
}

/// Whether a `fetch` of `sha256` is in flight in another process: the
/// entry's `<sha256>.lock` is held while the verified `.wsl` isn't
/// there yet. Never blocks — the lockfile is probed with
/// `try_lock_shared`, which fails only while a download holds it
/// exclusive (a shared holder means the import phase, which implies
/// the `.wsl` already exists), so a listing reports the download
/// rather than waiting on it.
#[must_use]
pub fn download_in_progress(sha256: &str) -> bool {
    let Some(dir) = cache_dir() else {
        return false;
    };
    let Ok(lock) = File::open(dir.join(format!("{sha256}.lock"))) else {
        return false;
    };
    matches!(lock.try_lock_shared(), Err(TryLockError::WouldBlock))
}

/// `.wsl` files in the download cache, oldest first. Empty when the
/// cache directory can't be read. Reads files directly — no flocking —
/// so listing never waits on an in-flight download or import.
pub fn cache_entries() -> Vec<CacheEntry> {
    let Some(dir) = cache_dir() else {
        return Vec::new();
    };
    let mut files: Vec<CacheEntry> = fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| {
                let e = e.ok()?;
                let path = e.path();
                if path.extension().is_none_or(|x| x != "wsl") {
                    return None;
                }
                let m = e.metadata().ok()?;
                Some(CacheEntry {
                    sha256: path.file_stem()?.to_string_lossy().into_owned(),
                    meta: read_meta(&path),
                    path,
                    size: m.len(),
                    modified: m.modified().ok(),
                })
            })
            .collect()
        })
        .unwrap_or_default();
    files.sort_by_key(|f| f.modified);
    files
}

/// The `<sha256>.lock` file a `Fetched` holds for `entry`.
fn lock_path(entry: &CacheEntry) -> PathBuf {
    entry.path.with_extension("lock")
}

/// Delete every `.wsl` in the download cache; returns
/// `(files removed, bytes freed)`. Takes each entry's lockfile
/// exclusive, so it waits for in-flight downloads and imports rather
/// than deleting files out from under them. The directory and the
/// 0-byte lockfiles are kept — the OS may reclaim the dir wholesale
/// anyway.
pub fn purge_cache() -> (usize, u64) {
    let mut removed = 0;
    let mut freed = 0;
    for e in cache_entries() {
        let Ok(l) = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path(&e))
        else {
            continue;
        };
        if l.lock().is_ok() && fs::remove_file(&e.path).is_ok() {
            removed += 1;
            freed += e.size;
        }
    }
    (removed, freed)
}

/// Evict oldest `.wsl` files until the cache is under
/// `MAX_CACHE_BYTES`, skipping entries that are downloading or being
/// imported (`try_lock` fails). Best-effort: failures are ignored.
fn prune_cache() {
    let entries = cache_entries();
    let mut total: u64 = entries.iter().map(|f| f.size).sum();
    for e in &entries {
        if total <= MAX_CACHE_BYTES {
            break;
        }
        let evictable = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path(e))
            .ok()
            .is_some_and(|l| matches!(l.try_lock(), Ok(())));
        if evictable && fs::remove_file(&e.path).is_ok() {
            total -= e.size;
        }
    }
}

fn file_sha256(path: &Path) -> Result<String> {
    let f = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut h = Sha256::new();
    std::io::copy(&mut BufReader::new(f), &mut h)?;
    Ok(format!("{:x}", h.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tar_with_conf(conf: Option<&str>) -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::new().unwrap();
        let mut builder = tar::Builder::new(f.reopen().unwrap());
        let mut header = tar::Header::new_gnu();
        let data = b"x";
        header.set_size(1);
        header.set_cksum();
        builder
            .append_data(&mut header, "etc/os-release", &data[..])
            .unwrap();
        if let Some(c) = conf {
            let mut header = tar::Header::new_gnu();
            header.set_size(c.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, CONF_PATH, c.as_bytes())
                .unwrap();
        }
        builder.finish().unwrap();
        f
    }

    #[test]
    fn reads_default_name() {
        let f = tar_with_conf(Some("[oobe]\ndefaultUid = 1000\ndefaultName = Rocky-9\n"));
        assert_eq!(
            distribution_name(f.path()).unwrap().as_deref(),
            Some("Rocky-9")
        );
    }

    #[test]
    fn name_outside_oobe_ignored() {
        let f = tar_with_conf(Some("[shortcut]\ndefaultName = nope\n"));
        assert_eq!(distribution_name(f.path()).unwrap(), None);
    }

    #[test]
    fn gzipped_tar() {
        let f = tar_with_conf(Some("[oobe]\n  defaultname = \"my-distro\"\n"));
        let gz = tempfile::NamedTempFile::new().unwrap();
        let mut enc =
            flate2::write::GzEncoder::new(gz.reopen().unwrap(), flate2::Compression::fast());
        enc.write_all(&std::fs::read(f.path()).unwrap()).unwrap();
        enc.finish().unwrap();
        assert_eq!(
            distribution_name(gz.path()).unwrap().as_deref(),
            Some("my-distro")
        );
    }

    #[test]
    fn no_conf() {
        let f = tar_with_conf(None);
        assert_eq!(distribution_name(f.path()).unwrap(), None);
    }
}

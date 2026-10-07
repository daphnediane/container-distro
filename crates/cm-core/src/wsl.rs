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

use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;

const CONF_PATH: &str = "etc/wsl-distribution.conf";

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

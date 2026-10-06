/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Wrap a bare rootfs tar (as written by `wsl --export` or
//! `container export`) in an OCI image-layout tar that
//! `container image load` accepts.
//!
//! The rootfs becomes the image's single layer, stored as-is: a plain tar
//! is an `…layer.v1.tar` blob, a gzipped one a `…layer.v1.tar+gzip` blob.
//! Only the `diff_id` needs the uncompressed digest.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, Write};
use std::path::Path;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use serde_json::json;
use sha2::{Digest, Sha256};

const MT_INDEX: &str = "application/vnd.oci.image.index.v1+json";
const MT_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const MT_CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const MT_LAYER_TAR: &str = "application/vnd.oci.image.layer.v1.tar";
const MT_LAYER_GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

/// The OCI platform name of the host (`arm64` / `amd64`).
#[must_use]
pub fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// `io::Write` sink that only hashes and counts.
struct HashWriter {
    hasher: Sha256,
    len: u64,
}

impl Write for HashWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.hasher.update(buf);
        self.len += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn hash_reader(r: impl Read) -> io::Result<(String, u64)> {
    let mut w = HashWriter {
        hasher: Sha256::new(),
        len: 0,
    };
    io::copy(&mut BufReader::new(r), &mut w)?;
    Ok((format!("{:x}", w.hasher.finalize()), w.len))
}

fn is_gzip(file: &mut File) -> io::Result<bool> {
    let mut magic = [0u8; 2];
    let n = file.read(&mut magic)?;
    file.rewind()?;
    Ok(n == 2 && magic == [0x1f, 0x8b])
}

/// Write an OCI image-layout tar for `rootfs` to `out`, tagged `reference`
/// for platform `linux/<arch>`. `labels` land in `config.Labels`, where
/// `container image list --format json` reports them.
pub fn build_layout(
    rootfs: &Path,
    out: impl Write,
    reference: &str,
    arch: &str,
    labels: &[(String, String)],
) -> Result<()> {
    let mut layer =
        File::open(rootfs).with_context(|| format!("failed to open {}", rootfs.display()))?;
    let gzip = is_gzip(&mut layer)?;
    let (layer_digest, layer_size) = hash_reader(&mut layer).context("failed to read rootfs")?;
    layer.rewind()?;
    let diff_id = if gzip {
        let (d, _) = hash_reader(GzDecoder::new(&mut layer))
            .context("failed to decompress gzipped rootfs")?;
        layer.rewind()?;
        d
    } else {
        layer_digest.clone()
    };

    let label_map: serde_json::Map<String, serde_json::Value> = labels
        .iter()
        .map(|(k, v)| (k.clone(), v.clone().into()))
        .collect();
    let config = serde_json::to_vec(&json!({
        "architecture": arch,
        "os": "linux",
        "config": { "Labels": label_map },
        "rootfs": { "type": "layers", "diff_ids": [format!("sha256:{diff_id}")] },
        "history": [{ "created_by": "container distro import", "comment": "imported rootfs" }],
    }))?;
    let config_digest = sha256_hex(&config);

    let manifest = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": MT_MANIFEST,
        "config": {
            "mediaType": MT_CONFIG,
            "digest": format!("sha256:{config_digest}"),
            "size": config.len(),
        },
        "layers": [{
            "mediaType": if gzip { MT_LAYER_GZIP } else { MT_LAYER_TAR },
            "digest": format!("sha256:{layer_digest}"),
            "size": layer_size,
        }],
    }))?;
    let manifest_digest = sha256_hex(&manifest);

    let index = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": MT_INDEX,
        "manifests": [{
            "mediaType": MT_MANIFEST,
            "digest": format!("sha256:{manifest_digest}"),
            "size": manifest.len(),
            "platform": { "architecture": arch, "os": "linux" },
            "annotations": {
                "org.opencontainers.image.ref.name": reference,
                "com.apple.containerization.image.name": reference,
                "io.containerd.image.name": reference,
            },
        }],
    }))?;

    let mut tar = tar::Builder::new(out);
    let mut add = |path: &str, data: &[u8]| -> io::Result<()> {
        let mut h = tar::Header::new_ustar();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tar.append_data(&mut h, path, data)
    };
    add("oci-layout", br#"{"imageLayoutVersion":"1.0.0"}"#)?;
    add("index.json", &index)?;
    add(&format!("blobs/sha256/{manifest_digest}"), &manifest)?;
    add(&format!("blobs/sha256/{config_digest}"), &config)?;
    let mut h = tar::Header::new_ustar();
    h.set_size(layer_size);
    h.set_mode(0o644);
    h.set_cksum();
    tar.append_data(&mut h, format!("blobs/sha256/{layer_digest}"), &mut layer)?;
    tar.into_inner()?.flush()?;
    Ok(())
}

/// Load `rootfs` (a tar or tar.gz path) into the `container` image store
/// as `reference`, for `linux/<host arch>`, carrying `labels`.
pub fn load_rootfs(rootfs: &Path, reference: &str, labels: &[(String, String)]) -> Result<()> {
    let layout = tempfile::Builder::new()
        .prefix("cm-import-")
        .suffix(".tar")
        .tempfile()
        .context("failed to create a temporary file")?;
    build_layout(
        rootfs,
        io::BufWriter::new(layout.as_file()),
        reference,
        host_arch(),
        labels,
    )?;
    let status = crate::container::container_cmd()
        .args(["image", "load", "-i"])
        .arg(layout.path())
        .status()
        .context("failed to run `container image load`")?;
    if !status.success() {
        anyhow::bail!("`container image load` exited with {status}");
    }
    Ok(())
}

/// Copy stdin to a temporary file (for `-` file arguments).
pub fn stdin_to_tempfile() -> Result<tempfile::NamedTempFile> {
    let mut tmp = tempfile::Builder::new()
        .prefix("cm-rootfs-")
        .tempfile()
        .context("failed to create a temporary file")?;
    io::copy(&mut io::stdin().lock(), &mut tmp).context("failed to read stdin")?;
    Ok(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn rootfs_tar() -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_ustar();
        let data = b"NAME=test\n";
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "etc/os-release", &data[..]).unwrap();
        b.into_inner().unwrap()
    }

    fn read_layout(bytes: &[u8]) -> HashMap<String, Vec<u8>> {
        let mut a = tar::Archive::new(bytes);
        a.entries()
            .unwrap()
            .map(|e| {
                let mut e = e.unwrap();
                let path = e.path().unwrap().to_string_lossy().into_owned();
                let mut v = Vec::new();
                e.read_to_end(&mut v).unwrap();
                (path, v)
            })
            .collect()
    }

    fn blob<'a>(files: &'a HashMap<String, Vec<u8>>, digest: &str) -> &'a [u8] {
        let hex = digest.strip_prefix("sha256:").unwrap();
        let data = &files[&format!("blobs/sha256/{hex}")];
        assert_eq!(sha256_hex(data), hex, "blob content must match its name");
        data
    }

    fn check_chain(rootfs: &[u8], uncompressed: &[u8], expect_mt: &str) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("rootfs.tar");
        std::fs::write(&src, rootfs).unwrap();
        let mut out = Vec::new();
        build_layout(
            &src,
            &mut out,
            "local/test:imported",
            "arm64",
            &[("io.example.distro".into(), "d1".into())],
        )
        .unwrap();
        let files = read_layout(&out);

        let index: serde_json::Value = serde_json::from_slice(&files["index.json"]).unwrap();
        let desc = &index["manifests"][0];
        assert_eq!(
            desc["annotations"]["org.opencontainers.image.ref.name"],
            "local/test:imported"
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(blob(&files, desc["digest"].as_str().unwrap())).unwrap();
        let config: serde_json::Value =
            serde_json::from_slice(blob(&files, manifest["config"]["digest"].as_str().unwrap()))
                .unwrap();
        let layer = &manifest["layers"][0];
        assert_eq!(layer["mediaType"], expect_mt);
        assert_eq!(blob(&files, layer["digest"].as_str().unwrap()), rootfs);
        assert_eq!(
            config["rootfs"]["diff_ids"][0],
            format!("sha256:{}", sha256_hex(uncompressed))
        );
        assert_eq!(config["architecture"], "arm64");
        assert_eq!(config["config"]["Labels"]["io.example.distro"], "d1");
    }

    #[test]
    fn test_layout_plain_tar_digest_chain() {
        let t = rootfs_tar();
        check_chain(&t, &t, MT_LAYER_TAR);
    }

    #[test]
    fn test_layout_gzip_tar_diff_id_uncompressed() {
        let t = rootfs_tar();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&t).unwrap();
        let gz = gz.finish().unwrap();
        check_chain(&gz, &t, MT_LAYER_GZIP);
    }
}

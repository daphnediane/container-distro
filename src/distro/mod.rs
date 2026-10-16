/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

pub mod spec;

mod adopt;
mod assets;
mod converge;
mod create;
mod default;
mod export;
mod host;
mod images;
mod import;
mod journal;
mod lifecycle;
mod list;
mod lock;
mod machine;
mod migrate;
mod names;
mod profiles;
mod rename;
mod run;
mod set;
mod terminal;
#[cfg(test)]
mod test_support;

use crate::container::{self, ContainerInfo, container_cmd};
use crate::distro::spec::LABEL_DISTRO;
use crate::naming::label_lookup;
use anyhow::{Context, Result, bail};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

pub use adopt::adopt;
pub(crate) use assets::assets_flavor;
pub use create::{CreateOptions, create};
pub use default::{default_name, set_default};
pub use export::export;
pub use host::default_resources;
pub use images::release_machine_images;
pub use import::{ImportSource, import};
pub use journal::distro_names;
pub use lifecycle::{delete, delete_all, start, stop};
pub use list::{DistroSummary, inspect, summaries};
pub(crate) use lock::{is_locked, lock, lock_symlink, unlock, write_nofollow};
pub use machine::{create_machine, export_machine, import_machine};
pub use migrate::{migrate, migrate_to_machine};
pub use names::{bail_distro_shadow, bail_machine_shadow};
pub use profiles::{open_terminal, sync_profiles};
pub use rename::{rename, rename_machine};
pub use run::{RunOpts, run_command};
pub use set::set;

/// Run a `container` command with inherited stdio; fail on non-zero exit.
fn run_container(args: &[&str]) -> Result<()> {
    let status = container_cmd()
        .args(args)
        .status()
        .with_context(|| format!("failed to run `container {}`", args.join(" ")))?;
    if !status.success() {
        bail!("`container {}` exited with {status}", args.join(" "));
    }
    Ok(())
}

/// Like [`run_container`] but with stdout discarded (for commands that
/// echo the container ID).
fn run_container_quiet(args: &[&str]) -> Result<()> {
    let status = container_cmd()
        .args(args)
        .stdout(Stdio::null())
        .status()
        .with_context(|| format!("failed to run `container {}`", args.join(" ")))?;
    if !status.success() {
        bail!("`container {}` exited with {status}", args.join(" "));
    }
    Ok(())
}

/// All distro containers.
pub fn distros() -> Result<Vec<ContainerInfo>> {
    Ok(container::list_containers()?
        .into_iter()
        .filter(|c| label_lookup(&c.configuration.labels, LABEL_DISTRO).is_some())
        .collect())
}

/// The distro a labeled container belongs to — its *logical* name.
/// `c.id()` is the ephemeral runtime id (`<name>-<suffix>`); the label
/// value is what the journal, `cm`, and the user know the distro by.
fn distro_name(c: &ContainerInfo) -> Option<&str> {
    label_lookup(&c.configuration.labels, LABEL_DISTRO).map(String::as_str)
}

/// The runtime container id for `name`: `<name>-<6 hex>`, like a
/// machine's backing container (`poc-a07fee`). The suffix keeps the
/// ephemeral container out of the logical namespace — a fresh id each
/// materialize, so nothing can address tomorrow's container by
/// yesterday's assumption. Truncated to `container`'s 63-char id
/// budget.
fn runtime_id(name: &str) -> String {
    let mut b = [0u8; 3];
    // SAFETY: arc4random_buf fills the buffer; it cannot fail.
    unsafe { libc::arc4random_buf(b.as_mut_ptr().cast(), b.len()) };
    let suffix = format!("{:02x}{:02x}{:02x}", b[0], b[1], b[2]);
    let budget = 63usize.saturating_sub(suffix.len() + 1);
    let base: String = name.chars().take(budget).collect();
    format!("{}-{suffix}", base.trim_end_matches('-'))
}

pub fn find(name: &str) -> Result<ContainerInfo> {
    distros()?
        .into_iter()
        .find(|c| distro_name(c) == Some(name))
        .with_context(|| format!("no distro named `{name}`"))
}

pub fn resolve(name: Option<String>) -> Result<String> {
    name.or_else(default_name).context(
        "no distro given and no default set (use -n NAME or `container distro set-default`)",
    )
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The current time as ISO 8601 UTC — the same `YYYY-MM-DDTHH:MM:SSZ`
/// shape `container` writes as `creationDate`. A libc call rather than
/// a time crate for one formatted string.
fn iso_now() -> String {
    // SAFETY: time() writes through a null pointer (returns the value)
    // and gmtime_r fills a valid out-param.
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm = unsafe { std::mem::zeroed::<libc::tm>() };
    unsafe { libc::gmtime_r(&t, &mut tm) };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::distro::spec::size_bytes;

    #[test]
    fn runtime_id_shape() {
        let id = runtime_id("alpine");
        let (base, suffix) = id.rsplit_once('-').unwrap();
        assert_eq!(base, "alpine");
        assert_eq!(suffix.len(), 6);
        assert!(suffix.bytes().all(|b| b.is_ascii_hexdigit()));
        // Long names truncate to `container`'s 63-char budget, never
        // leaving a dangling `-` before the suffix.
        let long = runtime_id(&"x".repeat(63));
        assert_eq!(long.len(), 63);
        assert!(long.chars().nth(56) == Some('-'));
        // Two ids for the same name differ.
        assert_ne!(runtime_id("alpine"), runtime_id("alpine"));
    }

    #[test]
    fn size_bytes_parses_container_sizes() {
        assert_eq!(size_bytes("4096M"), Some(4096 << 20));
        assert_eq!(size_bytes("8G"), Some(8 << 30));
        assert_eq!(size_bytes("512K"), Some(512 << 10));
        assert_eq!(size_bytes("1024"), Some(1024));
        assert_eq!(size_bytes("garbage"), None);
        assert_eq!(size_bytes(""), None);
    }
}

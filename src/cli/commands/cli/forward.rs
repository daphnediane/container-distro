/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `--forward`: localhost ports to a machine or distro.

use std::net::{IpAddr, Ipv4Addr};
use std::process::{ExitCode, Stdio};

use anyhow::{Context, Result, bail};

use super::args::Backend;
use super::backend::Target;
use super::target;
use crate::container::{self, ArgvMode};
use crate::distro;
use crate::util::forward::PortMapping;

/// The IPv4 address of a running machine, booting it if needed.
fn machine_ip(machine: Option<&str>) -> Result<(String, IpAddr)> {
    let find = || -> Result<Option<container::Machine>> {
        Ok(container::list_machines()?
            .into_iter()
            .find(|m| machine.map_or(m.default, |n| m.id == n)))
    };
    let label = machine.unwrap_or("default");
    let mut m = find()?.with_context(|| format!("no `{label}` machine found"))?;
    if !m.is_running() {
        let status = container::run_command(
            Some(&m.id),
            None,
            None,
            &container::GuestEnv::default(),
            None,
            &["true".to_string()],
            ArgvMode::Shell,
        )
        .stdin(Stdio::null())
        .status()
        .context("failed to boot the machine")?;
        if !status.success() {
            bail!("failed to boot machine `{}`", m.id);
        }
        m = find()?.with_context(|| format!("machine `{label}` disappeared"))?;
    }
    let ip = parse_ip(m.ip_address.as_deref(), &m.id)?;
    Ok((m.id, ip))
}

/// The IPv4 address of a distro, booting it if needed.
fn distro_ip(name: &str) -> Result<(String, IpAddr)> {
    if !distro::find(name)?.is_running() {
        distro::start(name)?;
    }
    let info = distro::find(name)?;
    Ok((name.to_string(), parse_ip(info.ipv4(), name)?))
}

fn parse_ip(addr: Option<&str>, id: &str) -> Result<IpAddr> {
    addr.with_context(|| format!("`{id}` has no IP address"))?
        .split('/')
        .next()
        .unwrap_or_default()
        .parse()
        .with_context(|| format!("failed to parse the IP address of `{id}`"))
}

/// Forward localhost ports to a machine's or distro's IP.
pub(super) fn forward(
    name: Option<&str>,
    mappings: &[PortMapping],
    sel: Backend,
) -> Result<ExitCode> {
    let (id, ip) = match target(name, sel)? {
        Target::Machine(m) => machine_ip(m.as_deref())?,
        Target::Distro(d) => distro_ip(&d)?,
    };
    eprintln!("Forwarding to `{id}` (Ctrl-C to stop)");
    crate::util::forward::forward(IpAddr::V4(Ipv4Addr::LOCALHOST), ip, mappings)?;
    Ok(ExitCode::SUCCESS)
}

/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! `container distro fs` — a thin adapter that maps the clap command
//! onto `distrofs` client arguments. The client owns the whole connect
//! path (socket hygiene, the relay peer's code-signature check, the
//! session handshake); container-distro supplies the socket, a fresh
//! token, and the distro's uid/gid — see [`distrofs::run`].

use anyhow::Result;
use container_distro::distrofs;

use crate::cli::FsCommand;

pub fn run(cmd: FsCommand) -> Result<()> {
    let (name, args): (Option<String>, Vec<String>) = match cmd {
        FsCommand::Ls { name, path, long } => (
            Some(name),
            [Some("ls".into()), long.then(|| "-l".into()), Some(path)]
                .into_iter()
                .flatten()
                .collect(),
        ),
        FsCommand::Stat {
            name,
            path,
            dereference,
        } => (
            Some(name),
            [
                Some("stat".into()),
                dereference.then(|| "-L".into()),
                Some(path),
            ]
            .into_iter()
            .flatten()
            .collect(),
        ),
        FsCommand::Get { name, path, dest } => {
            let mut a = vec!["get".into(), path];
            if let Some(d) = dest {
                a.push(d.to_string_lossy().into_owned());
            }
            (Some(name), a)
        }
        FsCommand::Put { name, src, path } => (
            Some(name),
            vec!["put".into(), src.to_string_lossy().into_owned(), path],
        ),
        FsCommand::Check { name } => (name, vec!["check".into()]),
    };
    let status = distrofs::run(name, &args)?;
    // Propagate the client's exit code exactly — a distrofs diagnostic
    // (bad token, missing path, EACCES) is the command's result.
    std::process::exit(status.code().unwrap_or(1));
}

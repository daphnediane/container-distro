/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! Installing generated shell completions.
//!
//! `cargo install` only copies binaries, so completion scripts are
//! generated from the clap definitions at runtime (`clap_complete` —
//! they can never drift from the CLI) and written by the installed
//! binary itself: `cm --install-completions` /
//! `container-distro install-completions`, under the `share` root
//! next to the binary's `bin` directory.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// A shell a completion script is generated for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    /// The shells `install-completions` writes, each in its
    /// conventional vendor location under the share root.
    pub const ALL: &[Self] = &[Self::Bash, Self::Zsh, Self::Fish];

    /// `bin`'s script path relative to the share root:
    /// `bash-completion/completions/<bin>`,
    /// `zsh/site-functions/_<bin>`,
    /// `fish/vendor_completions.d/<bin>.fish`.
    pub fn rel_path(self, bin: &str) -> PathBuf {
        match self {
            Shell::Bash => PathBuf::from(format!("bash-completion/completions/{bin}")),
            Shell::Zsh => PathBuf::from(format!("zsh/site-functions/_{bin}")),
            Shell::Fish => PathBuf::from(format!("fish/vendor_completions.d/{bin}.fish")),
        }
    }

    /// Whether `existing` looks like a completion script generated for
    /// `bin` — not a foreign or hand-edited file sharing the name.
    /// Each script carries a distinctive signature: the bash `complete`
    /// trailer, the zsh `#compdef` header, fish `complete -c` rules.
    pub fn is_generated(self, existing: &str, bin: &str) -> bool {
        match self {
            Shell::Bash => existing.contains(&format!("-o bashdefault -o default {bin}")),
            Shell::Zsh => existing.starts_with(&format!("#compdef {bin}")),
            Shell::Fish => existing.contains(&format!("complete -c {bin} ")),
        }
    }
}

/// `<exe>/../share` for a binary that lives in a `bin` directory —
/// `$CARGO_HOME/share` after `cargo install`, where the shell vendor
/// directories live on a normal `fpath`/`XDG_DATA_DIRS` setup.
fn share_dir_for(exe: &Path) -> Result<PathBuf> {
    let exe = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
    exe.parent()
        .filter(|p| p.file_name() == Some(OsStr::new("bin")))
        .and_then(Path::parent)
        .map(|prefix| prefix.join("share"))
        .with_context(|| {
            format!(
                "{} isn't under a `bin` directory — pass an explicit dir",
                exe.display()
            )
        })
}

/// The share root completions are written under when none is given:
/// the `share` of the prefix the running binary is installed under.
/// Dev builds (e.g. `target/debug/cm`) don't have one and must pass a
/// dir explicitly.
pub fn default_share_dir() -> Result<PathBuf> {
    share_dir_for(&env::current_exe().context("cannot locate the running binary")?)
}

/// Write `files` (`(shell, script)` for `bin`) under `share`, creating
/// the shell subdirs. Returns the written paths.
pub fn write_files(share: &Path, bin: &str, files: &[(Shell, String)]) -> Result<Vec<PathBuf>> {
    let mut written = Vec::with_capacity(files.len());
    for (shell, script) in files {
        let path = share.join(shell.rel_path(bin));
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
        }
        fs::write(&path, script).with_context(|| {
            format!(
                "cannot write {} (re-run with sudo, or pass an explicit dir)",
                path.display()
            )
        })?;
        written.push(path);
    }
    Ok(written)
}

/// Remove previously written `files` from `share` — only when the
/// file on disk still looks like a generated script for `bin`;
/// foreign or edited files are left in place with a warning. Missing
/// files and a missing directory are not errors.
pub fn remove_files(share: &Path, bin: &str, files: &[(Shell, String)]) -> Result<()> {
    for (shell, _) in files {
        let path = share.join(shell.rel_path(bin));
        match fs::read_to_string(&path) {
            Ok(existing) if !shell.is_generated(&existing, bin) => {
                eprintln!(
                    "not removing {}: not a generated completion for {bin}",
                    path.display()
                );
            }
            Ok(_) => {
                fs::remove_file(&path)
                    .with_context(|| format!("failed to remove {}", path.display()))?;
                println!("Removed {}", path.display());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("failed to read {}", path.display()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_dir_follows_the_bin_prefix() {
        assert_eq!(
            share_dir_for(Path::new("/usr/local/bin/cm")).unwrap(),
            Path::new("/usr/local/share")
        );
        // A dev build in target/debug has no prefix — the caller must
        // pass a dir.
        assert!(share_dir_for(Path::new("/w/target/debug/cm")).is_err());
    }

    #[test]
    fn rel_paths_are_the_vendor_locations() {
        assert_eq!(
            Shell::Bash.rel_path("cm"),
            Path::new("bash-completion/completions/cm")
        );
        assert_eq!(
            Shell::Zsh.rel_path("cm"),
            Path::new("zsh/site-functions/_cm")
        );
        assert_eq!(
            Shell::Fish.rel_path("cm"),
            Path::new("fish/vendor_completions.d/cm.fish")
        );
    }

    #[test]
    fn is_generated_matches_each_shell_signature() {
        assert!(Shell::Bash.is_generated("x\ncomplete -F _cm -o bashdefault -o default cm", "cm"));
        assert!(!Shell::Bash.is_generated("complete -F _cm cm", "cm"));
        assert!(Shell::Zsh.is_generated("#compdef cm\n\nx", "cm"));
        assert!(!Shell::Zsh.is_generated("x\n#compdef cm", "cm"));
        assert!(Shell::Fish.is_generated("complete -c cm -n x", "cm"));
        assert!(!Shell::Fish.is_generated("complete -c cm", "cm"));
        // Another bin's script isn't ours.
        assert!(!Shell::Zsh.is_generated("#compdef cargo", "cm"));
    }

    #[test]
    fn write_and_remove_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let files = vec![
            (
                Shell::Bash,
                "complete -F _cm -o bashdefault -o default cm".to_string(),
            ),
            (Shell::Zsh, "#compdef cm\n".to_string()),
        ];
        let written = write_files(tmp.path(), "cm", &files).unwrap();
        assert_eq!(written.len(), 2);
        assert!(written.iter().all(|p| p.exists()));
        remove_files(tmp.path(), "cm", &files).unwrap();
        assert!(written.iter().all(|p| !p.exists()));
        // Removing again is a no-op, not an error.
        remove_files(tmp.path(), "cm", &files).unwrap();
    }

    #[test]
    fn remove_files_leaves_foreign_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("zsh/site-functions");
        fs::create_dir_all(&dir).unwrap();
        // Same file name, different contents — not ours.
        let foreign = dir.join("_cm");
        fs::write(&foreign, "local x=1\n").unwrap();
        let files = vec![(Shell::Zsh, "#compdef cm\n# generated\n".to_string())];
        remove_files(tmp.path(), "cm", &files).unwrap();
        assert!(foreign.exists());
    }
}

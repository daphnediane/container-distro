/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

//! macOS terminal profiles for distros (#30).
//!
//! A `.wsl` package's `wsl-distribution.conf` carries a
//! `[windowsterminal]` section that Windows turns into a Windows
//! Terminal profile when the distro is installed — `enabled`
//! (default `true`) plus an optional `ProfileTemplate` styling
//! fragment (not yet mapped). The macOS equivalents, generated on
//! distro create/import, refreshed on rename, and removed on
//! delete:
//!
//! - a Terminal.app "settings set" whose `CommandString` is
//!   `cm -d <name>`, upserted into `Window Settings` through `defaults`
//!   (cfprefsd-aware — Terminal notices without a launch, unlike
//!   `open file.terminal`, which would open a window per import) and
//!   deleted through AppleScript while Terminal is running plus a
//!   direct plist edit — a running Terminal loads settings lazily, so
//!   a fresh profile can be invisible to the scripting model;
//! - an iTerm2 dynamic profile in our own `container-distro.json`
//!   under `DynamicProfiles`, written only when iTerm2 is installed.
//!
//! Profiles we create carry a `ContainerDistroDistro` marker (and the
//! generated profile name is journaled in `terminal-profiles.json`),
//! so rename/delete only touch what we made — a user's same-named
//! profile is left alone, ours then taking a ` (cm)` suffix.
//! Everything here is best-effort: a failure warns and never fails
//! the distro operation.
//!
//! `[shortcut]` metadata — the Start-menu launcher equivalent —
//! is #33.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::{Map, Value, json};

use crate::naming::state_dir;
use crate::wsl;

/// The Terminal.app defaults domain and the dict holding profiles.
const TERMINAL_DOMAIN: &str = "com.apple.Terminal";
const WINDOW_SETTINGS: &str = "Window Settings";

/// Marker key inside a profile dict naming the distro that owns it.
const MARKER: &str = "ContainerDistroDistro";

/// Our iTerm2 dynamic-profile file — we own its whole `Profiles` list.
const ITERM_FILE: &str = "container-distro.json";

/// The `wsl-distribution.conf` fields a profile can use.
#[derive(Debug)]
struct Meta {
    /// `windowsterminal.enabled` — whether to generate a profile at
    /// all; WSL's default is `true` even with no section.
    enabled: bool,
}

/// Read the conf text's profile metadata. `ProfileTemplate` (a
/// Windows-Terminal-fragment JSON inside the rootfs) isn't mapped
/// yet — #34 — name and command are ours, as WSL auto-fills them.
fn meta(conf: Option<&str>) -> Meta {
    Meta {
        enabled: conf
            .and_then(|t| wsl::ini_value(t, "windowsterminal", "enabled"))
            .is_none_or(|v| !v.trim().eq_ignore_ascii_case("false")),
    }
}

/// The command a profile runs — `cm -d <name>` with `cm` resolved to an
/// absolute path when it sits next to the running binary or on PATH,
/// so a profile survives PATH-less launch contexts.
fn launch_command(name: &str) -> String {
    let cm = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("cm")))
        .filter(|p| p.is_file())
        .or_else(|| {
            std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path)
                    .map(|d| d.join("cm"))
                    .find(|p| p.is_file())
            })
        })
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "cm".into());
    format!("{cm} -d {name}")
}

// ---- sidecar: which profile names we wrote, per distro ----

fn sidecar_path() -> Option<PathBuf> {
    Some(state_dir().ok()?.join("terminal-profiles.json"))
}

fn sidecar() -> Map<String, Value> {
    sidecar_path()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn sidecar_write(mut map: Map<String, Value>, name: &str, entry: Option<Value>) {
    if let Some(e) = entry {
        map.insert(name.to_string(), e);
    } else {
        map.remove(name);
    }
    if let Some(p) = sidecar_path()
        && let Some(parent) = p.parent()
    {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let _ = fs::write(
            &p,
            serde_json::to_string_pretty(&Value::Object(map)).unwrap(),
        );
    }
}

// ---- Terminal.app ----

fn terminal_plist() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
        .join("Library/Preferences/com.apple.Terminal.plist")
}

/// `Window Settings` as JSON, or empty — profile names and our
/// string marker survive the trip (binary blobs like colors don't
/// matter for key/marker inspection).
fn window_settings() -> Map<String, Value> {
    let plist = terminal_plist();
    let out = Command::new("plutil")
        .args(["-extract", WINDOW_SETTINGS, "json", "-o", "-"])
        .arg(&plist)
        .output();
    out.ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// The name under which to write our profile: the display name when
/// free or already ours, else a ` (cm)`-suffixed variant — never
/// overwrite a profile we don't own.
fn pick_profile_name(settings: &Map<String, Value>, name: &str, display: &str) -> Option<String> {
    for candidate in [display.to_string(), format!("{display} (cm)")] {
        match settings.get(&candidate) {
            None => return Some(candidate),
            Some(v) if v.get(MARKER).and_then(Value::as_str) == Some(name) => {
                return Some(candidate);
            }
            Some(_) => continue,
        }
    }
    None
}

/// Escape a string for a NeXTSTEP `{ k = "v"; }` defaults fragment.
fn plist_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn terminal_upsert(name: &str, display: &str) -> Option<String> {
    let settings = window_settings();
    let profile = pick_profile_name(&settings, name, display)?;
    // `defaults write` goes through cfprefsd — Terminal picks the
    // profile up live; dict-add replaces when the key is already ours.
    let dict = format!(
        "{{ type = \"Window Settings\"; name = {}; \
         ProfileCurrentVersion = \"2.09\"; CommandString = {}; \
         RunCommandAsShell = 1; TitleDisplaysCustomTitle = 1; \
         CustomTitle = {}; {} = {}; }}",
        plist_quote(&profile),
        plist_quote(&launch_command(name)),
        plist_quote(display),
        MARKER,
        plist_quote(name),
    );
    let status = Command::new("defaults")
        .args([
            "write",
            TERMINAL_DOMAIN,
            WINDOW_SETTINGS,
            "-dict-add",
            &profile,
            &dict,
        ])
        .status();
    match status {
        Ok(s) if s.success() => Some(profile),
        _ => {
            eprintln!("container-distro: warning: couldn't write a Terminal profile for `{name}`");
            None
        }
    }
}

/// Every Terminal profile name belonging to `name`: the sidecar's
/// record plus any `Window Settings` entry still carrying our marker.
fn terminal_owned(name: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Some(v) = sidecar().get(name)
        && let Some(p) = v.get("terminal").and_then(Value::as_str)
    {
        names.push(p.to_string());
    }
    for (k, v) in window_settings() {
        if v.get(MARKER).and_then(Value::as_str) == Some(name) && !names.contains(&k) {
            names.push(k);
        }
    }
    names
}

fn terminal_remove(name: &str) {
    let running = Command::new("pgrep")
        .args(["-x", "Terminal"])
        .status()
        .is_ok_and(|s| s.success());
    for profile in terminal_owned(name) {
        if running {
            // The dictionary's `delete` works on settings sets — but
            // only for profiles a *running* Terminal has loaded, and
            // it lazily reloads `Window Settings`, so a profile we
            // just wrote can be invisible to it.
            let _ = Command::new("osascript")
                .args([
                    "-e",
                    &format!("tell application \"Terminal\" to delete settings set \"{profile}\""),
                ])
                .status();
        }
        // The plist is authoritative either way — PlistBuddy covers
        // both the not-running case (spawning Terminal just to delete
        // would open a window) and a stale live model above. Its key
        // paths are `:`-separated, so names containing dots still work.
        let _ = Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", &format!("Delete ':{WINDOW_SETTINGS}:{profile}'")])
            .arg(terminal_plist())
            .status();
    }
}

// ---- iTerm2 ----

fn iterm_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let support = home.join("Library/Application Support/iTerm2");
    if support.is_dir()
        || PathBuf::from("/Applications/iTerm.app").is_dir()
        || home.join("Applications/iTerm.app").is_dir()
    {
        Some(support.join("DynamicProfiles"))
    } else {
        None
    }
}

/// A stable per-distro GUID — dynamic profiles key on it, so it must
/// survive a rename of the display (it follows the distro name).
fn iterm_guid(name: &str) -> String {
    let h = wsl_sha(format!("container-distro:{name}").as_bytes());
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

fn wsl_sha(data: &[u8]) -> String {
    crate::oci::sha256_hex(data)
}

fn iterm_file() -> Option<PathBuf> {
    iterm_dir().map(|d| d.join(ITERM_FILE))
}

/// The `Profiles` array from our file — `None` when the file exists
/// but isn't parseable (user-edited; don't clobber it).
fn iterm_profiles() -> Option<Vec<Value>> {
    let f = iterm_file()?;
    let Ok(text) = fs::read_to_string(&f) else {
        return Some(Vec::new());
    };
    serde_json::from_str::<Value>(&text)
        .ok()?
        .get("Profiles")?
        .as_array()
        .cloned()
}

fn iterm_write(profiles: Vec<Value>) {
    let Some(f) = iterm_file() else { return };
    if profiles.is_empty() {
        let _ = fs::remove_file(&f);
        return;
    }
    if let Some(parent) = f.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return;
    }
    let _ = fs::write(
        &f,
        serde_json::to_string_pretty(&json!({ "Profiles": profiles })).unwrap(),
    );
}

fn iterm_upsert(name: &str, display: &str) {
    let Some(mut profiles) = iterm_profiles() else {
        return;
    };
    let guid = iterm_guid(name);
    profiles.retain(|p| p.get("Guid").and_then(Value::as_str) != Some(guid.as_str()));
    profiles.push(json!({
        "Name": display,
        "Guid": guid,
        "Custom Command": "Yes",
        "Command": launch_command(name),
        "Custom Title Modes": ["Profile"],
        "Tags": ["container-distro"],
    }));
    iterm_write(profiles);
}

fn iterm_remove(name: &str) {
    let Some(mut profiles) = iterm_profiles() else {
        return;
    };
    let guid = iterm_guid(name);
    let before = profiles.len();
    profiles.retain(|p| p.get("Guid").and_then(Value::as_str) != Some(guid.as_str()));
    if profiles.len() != before {
        iterm_write(profiles);
    }
}

// ---- public lifecycle ----

/// (Re)generate `name`'s terminal profiles: Terminal.app settings set
/// and the iTerm2 dynamic profile. `conf` is the distro's packaged
/// `wsl-distribution.conf` text, when it ships one — the profile is
/// named for the distro, as WSL names Windows Terminal profiles.
/// `windowsterminal.enabled = false` instead drops anything generated.
pub fn sync(name: &str, conf: Option<&str>) {
    if !meta(conf).enabled {
        remove(name);
        return;
    }
    if iterm_dir().is_some() {
        iterm_upsert(name, name);
    }
    let entry = json!({
        "terminal": terminal_upsert(name, name),
        "display": name,
    });
    sidecar_write(sidecar(), name, Some(entry));
}

/// Remove everything we generated for `name` — distro delete and
/// `migrate --out`. Profiles the user renamed or duplicated are
/// matched by marker, not just the recorded name.
pub fn remove(name: &str) {
    iterm_remove(name);
    terminal_remove(name);
    sidecar_write(sidecar(), name, None);
}

/// `rename`: drop `old`'s profiles, then regenerate under `new` —
/// `conf` is the journaled `wsl-distribution.conf` when present.
pub fn rename(old: &str, new: &str, conf: Option<&str>) {
    remove(old);
    sync(new, conf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_defaults_true_unless_set_false() {
        assert!(meta(None).enabled);
        assert!(meta(Some("[windowsterminal]\nenabled = true")).enabled);
        assert!(!meta(Some("[windowsterminal]\nenabled = false")).enabled);
        assert!(!meta(Some("[WindowsTerminal]\nEnabled = FALSE")).enabled);
        assert!(meta(Some("[shortcut]\nenabled = false")).enabled);
    }

    #[test]
    fn guid_is_stable_and_shaped() {
        let g = iterm_guid("ubuntu");
        assert_eq!(g, iterm_guid("ubuntu"));
        assert_ne!(g, iterm_guid("debian"));
        assert_eq!(g.len(), 36);
        assert_eq!(g.chars().filter(|c| *c == '-').count(), 4);
    }

    #[test]
    fn pick_name_never_takes_a_foreign_profile() {
        let mut settings = Map::new();
        // Name is free → display wins.
        assert_eq!(
            pick_profile_name(&settings, "ubuntu", "Ubuntu").as_deref(),
            Some("Ubuntu")
        );
        // A user's profile squats on the name → ours takes a suffix.
        settings.insert("Ubuntu".into(), json!({"name": "Ubuntu"}));
        assert_eq!(
            pick_profile_name(&settings, "ubuntu", "Ubuntu").as_deref(),
            Some("Ubuntu (cm)")
        );
        // Both taken by others → give up rather than clobber.
        settings.insert("Ubuntu (cm)".into(), json!({"name": "Ubuntu (cm)"}));
        assert_eq!(pick_profile_name(&settings, "ubuntu", "Ubuntu"), None);
        // A profile with our marker is ours to refresh.
        settings.insert("Mine".into(), json!({MARKER: "ubuntu"}));
        assert_eq!(
            pick_profile_name(&settings, "ubuntu", "Mine").as_deref(),
            Some("Mine")
        );
    }
}

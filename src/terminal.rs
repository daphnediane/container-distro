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
//! - a `.terminal` file per distro under
//!   `state_dir()/terminal-profiles/` — opening it
//!   (`container distro terminal`, `cm --terminal`, double-click)
//!   makes Terminal import the settings set *through its own model*,
//!   so it survives the prefs flushes that drop profiles injected
//!   behind a running Terminal's back, and launches `cm -d <name>`;
//! - an iTerm2 dynamic profile in our own `container-distro.json`
//!   under `DynamicProfiles`, written only when iTerm2 is installed.
//!
//! Imported Terminal profiles carry a `ContainerDistroDistro` marker,
//! so rename/delete only touch what we made — a user's same-named
//! profile is left alone, ours then taking a ` (cm)` suffix. Whether
//! a distro wants profiles is journaled intent
//! (`Intent::terminal_profile`): `explicit` carries
//! `--terminal-profile`/`--no-terminal-profile`, `source` carries a
//! package's `windowsterminal.enabled`, and the built-in default is
//! `true`. Nothing tracks whether the imported profile still exists —
//! only the user imports it (by opening the file), so a deletion is
//! automatically honored; a sync just rewrites the launchers.
//! Everything here is best-effort: a failure warns and never fails
//! the distro operation.
//!
//! `[shortcut]` metadata — the Start-menu launcher equivalent —
//! is #33; the `.terminal` files double as the launcher artifacts.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::naming::state_dir;
use crate::wsl;

/// The dict holding Terminal's profiles in its preferences plist.
const WINDOW_SETTINGS: &str = "Window Settings";

/// Marker key inside a profile dict naming the distro that owns it.
const MARKER: &str = "ContainerDistroDistro";

/// Our iTerm2 dynamic-profile file — we own its whole `Profiles` list.
const ITERM2_FILE: &str = "container-distro.json";

/// `windowsterminal.enabled` from a packaged `wsl-distribution.conf`,
/// as *source* intent — `Some` only when the key is present (`false`
/// suppresses generation, `true` re-affirms it); absent/unset inherits.
/// `ProfileTemplate` (a Windows-Terminal-fragment JSON inside the
/// rootfs) isn't mapped yet — #34 — name and command are ours, as WSL
/// auto-fills them.
pub(crate) fn conf_terminal_profile(conf: &str) -> Option<bool> {
    wsl::ini_value(conf, "windowsterminal", "enabled")
        .map(|v| !v.trim().eq_ignore_ascii_case("false"))
}

/// `cm` resolved to an absolute path when it sits next to the running
/// binary or on PATH, so a profile survives PATH-less launch contexts.
fn cm_path() -> String {
    std::env::current_exe()
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
        .unwrap_or_else(|| "cm".into())
}

/// The command a profile runs — the launcher script via `sh`, so the
/// same `exec cm --cd '~' -d NAME` semantics apply everywhere and
/// command updates are a script rewrite, never a profile re-import.
fn launch_command(name: &str) -> String {
    match script_file(name) {
        Some(s) => format!("/bin/sh '{}'", s.display()),
        None => format!("{} --cd '~' -d {name}", cm_path()),
    }
}

// ---- Terminal.app `.terminal` launchers ----

fn terminal_plist() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
        .join("Library/Preferences/com.apple.Terminal.plist")
}

/// `Window Settings` profile names paired with our marker value
/// (`None` for profiles we didn't write). Read through the `plist`
/// crate: `plutil -extract … json` chokes on the `<data>` blobs real
/// profiles carry (colors, fonts), which left every profile invisible
/// — including foreign ones `pick_profile_name` must not clobber.
fn window_settings() -> Vec<(String, Option<String>)> {
    plist::from_file::<_, plist::Value>(terminal_plist())
        .ok()
        .and_then(plist::Value::into_dictionary)
        .and_then(|mut d| d.remove(WINDOW_SETTINGS))
        .and_then(plist::Value::into_dictionary)
        .map(|d| {
            d.into_iter()
                .map(|(k, v)| {
                    let marker = v
                        .as_dictionary()
                        .and_then(|p| p.get(MARKER))
                        .and_then(plist::Value::as_string)
                        .map(str::to_string);
                    (k, marker)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The name under which to write our profile: the display name when
/// free or already ours, else a ` (cm)`-suffixed variant — never
/// overwrite a profile we don't own.
fn pick_profile_name(
    settings: &[(String, Option<String>)],
    name: &str,
    display: &str,
) -> Option<String> {
    for candidate in [display.to_string(), format!("{display} (cm)")] {
        match settings.iter().find(|(k, _)| *k == candidate) {
            None => return Some(candidate),
            Some((_, marker)) if marker.as_deref() == Some(name) => {
                return Some(candidate);
            }
            Some(_) => continue,
        }
    }
    None
}

/// `state_dir()/terminal-profiles/<name>.terminal` — the distro's
/// Terminal.app launcher file.
fn terminal_file(name: &str) -> Option<PathBuf> {
    Some(
        state_dir()
            .ok()?
            .join("terminal-profiles")
            .join(format!("{name}.terminal")),
    )
}

/// `state_dir()/terminal-cmds/<name>.sh` — the script imported
/// profiles point at. CommandString names the script's *path*, so the
/// imported copy never goes stale: command changes are a script
/// rewrite, not a re-import.
fn script_file(name: &str) -> Option<PathBuf> {
    Some(
        state_dir()
            .ok()?
            .join("terminal-cmds")
            .join(format!("{name}.sh")),
    )
}

/// The launcher's contents — `exec` so the window holds exactly `cm`,
/// started at the guest home (`--cd ~`). `"$@"` lets `name.sh cmd…`
/// run a guest command (the #33 launcher double-duty); a profile's
/// own CommandString passes none.
fn script_contents(name: &str) -> String {
    format!(
        "#!/bin/sh\nexec '{}' --cd '~' -d '{name}' \"$@\"\n",
        cm_path()
    )
}

/// Write `name`'s launcher script (+x). Returns false on I/O failure.
fn write_script_at(path: &Path, contents: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    fs::write(path, contents).is_ok()
        && fs::set_permissions(path, fs::Permissions::from_mode(0o755)).is_ok()
}

fn write_script(name: &str) -> bool {
    let Some(f) = script_file(name) else {
        return false;
    };
    write_script_at(&f, &script_contents(name))
}

/// Whether Terminal.app is running — deciding both how an import can
/// land silently and whether the AS delete in `terminal_remove` can
/// reach the live model.
fn terminal_running() -> bool {
    Command::new("pgrep")
        .args(["-x", "Terminal"])
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The `.terminal` settings-set dict `open` imports and launches.
/// `shellExitAction` 1 = close the window when the shell exits cleanly,
/// so a finished `cm` run drops its window but an error stays visible.
fn terminal_profile_dict(name: &str, profile: &str) -> plist::Value {
    plist::Value::Dictionary(plist::Dictionary::from_iter([
        ("name", plist::Value::from(profile)),
        ("type", plist::Value::from("Window Settings")),
        ("ProfileCurrentVersion", plist::Value::from(2.09)),
        ("CommandString", plist::Value::from(launch_command(name))),
        ("RunCommandAsShell", plist::Value::from(true)),
        ("shellExitAction", plist::Value::from(1)),
        ("TitleDisplaysCustomTitle", plist::Value::from(true)),
        ("CustomTitle", plist::Value::from(profile)),
        (MARKER, plist::Value::from(name)),
    ]))
}

/// Quote `s` as a plist old-style string: `defaults`'s parser treats
/// `()`/spaces/… in bare strings as syntax.
fn plist_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `key`-`dict` as a `defaults` plist fragment — old-style syntax is
/// all `-dict-add` accepts.
fn fragment(dict: &plist::Dictionary) -> String {
    dict.iter()
        .map(|(k, v)| {
            let v = match v {
                plist::Value::String(s) => plist_quote(s),
                plist::Value::Boolean(b) => b.to_string(),
                plist::Value::Real(r) => r.to_string(),
                plist::Value::Integer(i) => i.to_string(),
                other => plist_quote(&format!("{other:?}")),
            };
            format!("{} = {v};", plist_quote(k))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Write a whole profile dict into `Window Settings` via `defaults`
/// (cfprefsd-serialized — safe exactly when Terminal isn't running to
/// clobber it).
fn defaults_write_profile(name: &str, profile: &str) -> bool {
    let plist::Value::Dictionary(d) = terminal_profile_dict(name, profile) else {
        return false;
    };
    Command::new("defaults")
        .args([
            "write",
            "com.apple.Terminal",
            WINDOW_SETTINGS,
            "-dict-add",
            &plist_quote(profile),
            &fragment(&d),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// (Re)write `name`'s launcher artifacts — the script and the
/// `.terminal` file whose `name` key skips names a foreign profile
/// holds (ours then take ` (cm)`).
fn write_artifacts(name: &str) -> bool {
    let Some(f) = terminal_file(name) else {
        return false;
    };
    let Some(profile) = pick_profile_name(&window_settings(), name, name) else {
        eprintln!(
            "container-distro: warning: Terminal profile names for `{name}` are already taken"
        );
        return false;
    };
    if let Some(parent) = f.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    if let Err(e) = plist::to_file_xml(&f, &terminal_profile_dict(name, &profile)) {
        eprintln!(
            "container-distro: warning: couldn't write {}: {e}",
            f.display()
        );
        return false;
    }
    if !write_script(name) {
        eprintln!("container-distro: warning: couldn't write {name}'s launcher script");
        return false;
    }
    true
}

/// `open` `name`'s `.terminal` launcher — Terminal imports the profile
/// (a no-op when the name's already imported) and launches a window.
fn open_launcher(name: &str) -> bool {
    let Some(f) = terminal_file(name) else {
        return false;
    };
    Command::new("open")
        .args(["-b", "com.apple.Terminal"])
        .arg(&f)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `container distro terminal` / `cm --terminal`: refresh the
/// artifacts and `open` the launcher — the import lands through
/// Terminal's own model and the window runs `cm -d name`.
pub fn open_terminal(name: &str) -> anyhow::Result<()> {
    if !write_artifacts(name) {
        anyhow::bail!("couldn't write {name}'s launcher files");
    }
    if !open_launcher(name) {
        anyhow::bail!("couldn't open {name}'s profile in Terminal.app");
    }
    Ok(())
}

/// Install `name`'s profile into Terminal *without* a window — the
/// create/`--terminal-profile`/`sync-profiles` path.
///
/// - A marker-bearing profile already imported → nothing to do.
/// - Terminal not running → write `Window Settings` via `defaults`:
///   cfprefsd serializes it, and with no live model nothing can flush
///   it away — the profile is simply there at Terminal's next launch.
/// - Terminal running → a `defaults` write would die at Terminal's
///   next prefs flush (its model never loaded the entry). Instead the
///   launcher's script is swapped for a self-replacing no-op —
///   `mv -f "$0.real" "$0"` — the real command parked alongside; the
///   `.terminal` file opens, Terminal imports *through its model* and
///   the launched window runs the no-op: it restores the real script
///   and exits, so `shellExitAction` closes the window at once. Any
///   later run of the restored script behaves normally — the no-op
///   can only fire once, during the import.
pub fn ensure_profile(name: &str) -> bool {
    if !terminal_owned(name).is_empty() {
        return true;
    }
    if !terminal_running() {
        let Some(profile) = pick_profile_name(&window_settings(), name, name) else {
            return false;
        };
        return defaults_write_profile(name, &profile);
    }
    // Park the real script, arm the self-replacing no-op, and open.
    let (Some(script), Some(real)) = (
        script_file(name),
        script_file(name).map(|p| {
            let mut r = p.clone();
            r.set_extension("sh.real");
            r
        }),
    ) else {
        return false;
    };
    if !write_artifacts(name) || !write_script_at(&real, &script_contents(name)) {
        return false;
    }
    if !write_script_at(&script, "#!/bin/sh\nmv -f \"$0.real\" \"$0\"\nexit 0\n") {
        let _ = fs::remove_file(&real);
        return false;
    }
    if !open_launcher(name) {
        // The import never happened — put the real command back.
        let _ = write_script_at(&script, &script_contents(name));
        let _ = fs::remove_file(&real);
        return false;
    }
    true
}

/// Every Terminal profile name belonging to `name` — marker-bearing
/// `Window Settings` entries, however the user renamed them.
fn terminal_owned(name: &str) -> Vec<String> {
    window_settings()
        .into_iter()
        .filter(|(_, m)| m.as_deref() == Some(name))
        .map(|(k, _)| k)
        .collect()
}

fn terminal_remove(name: &str) {
    for profile in terminal_owned(name) {
        if terminal_running() {
            // The dictionary's `delete` works on settings sets — but
            // only for profiles a *running* Terminal has loaded, and
            // it lazily reloads `Window Settings`, so a profile we
            // just wrote can be invisible to it. -1728 ("can't get
            // settings set") is that benign case; the plist edit below
            // is authoritative, so its stderr is noise.
            let _ = Command::new("osascript")
                .args([
                    "-e",
                    &format!("tell application \"Terminal\" to delete settings set \"{profile}\""),
                ])
                .stderr(Stdio::null())
                .status();
        }
        // The plist is authoritative either way — PlistBuddy covers
        // both the not-running case (spawning Terminal just to delete
        // would open a window) and a stale live model above. Its key
        // paths are `:`-separated, so names containing dots still work.
        let _ = Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", &format!("Delete ':{WINDOW_SETTINGS}:{profile}'")])
            .arg(terminal_plist())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

// ---- iTerm2 ----

fn iterm2_dir() -> Option<PathBuf> {
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
fn iterm2_guid(name: &str) -> String {
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

fn iterm2_file() -> Option<PathBuf> {
    iterm2_dir().map(|d| d.join(ITERM2_FILE))
}

/// The `Profiles` array from our file — `None` when the file exists
/// but isn't parseable (user-edited; don't clobber it).
fn iterm2_profiles() -> Option<Vec<Value>> {
    let f = iterm2_file()?;
    let Ok(text) = fs::read_to_string(&f) else {
        return Some(Vec::new());
    };
    serde_json::from_str::<Value>(&text)
        .ok()?
        .get("Profiles")?
        .as_array()
        .cloned()
}

fn iterm2_write(profiles: Vec<Value>) -> bool {
    let Some(f) = iterm2_file() else { return false };
    if profiles.is_empty() {
        return fs::remove_file(&f).is_ok() || !f.exists();
    }
    if let Some(parent) = f.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    fs::write(
        &f,
        serde_json::to_string_pretty(&json!({ "Profiles": profiles })).unwrap(),
    )
    .is_ok()
}

fn iterm2_upsert(name: &str, display: &str) -> bool {
    let Some(mut profiles) = iterm2_profiles() else {
        return false;
    };
    let guid = iterm2_guid(name);
    profiles.retain(|p| p.get("Guid").and_then(Value::as_str) != Some(guid.as_str()));
    profiles.push(json!({
        "Name": display,
        "Guid": guid,
        "Custom Command": "Yes",
        "Command": launch_command(name),
        "Custom Title Modes": ["Profile"],
        "Tags": ["container-distro"],
    }));
    iterm2_write(profiles)
}

fn iterm2_remove(name: &str) {
    let Some(mut profiles) = iterm2_profiles() else {
        return;
    };
    let guid = iterm2_guid(name);
    let before = profiles.len();
    profiles.retain(|p| p.get("Guid").and_then(Value::as_str) != Some(guid.as_str()));
    if profiles.len() != before {
        let _ = iterm2_write(profiles);
    }
}

// ---- public lifecycle ----

/// (Re)generate `name`'s terminal-profile artifacts per the resolved
/// intent — the launcher script + `.terminal` file and, when iTerm2
/// is installed, the dynamic-profile entry. `enabled` is the distro's
/// resolved `terminal_profile`; `false` drops anything generated.
/// The artifacts are wholly ours, so a sync just converges them — a
/// missing file or deleted entry gets rewritten, never honored as a
/// deletion. Imported Terminal profiles are the user's: synced away
/// only by `remove`, and only re-imported by `ensure_profile` or an
/// explicit open — never by a routine sync.
pub fn sync(name: &str, enabled: bool) {
    if !enabled {
        remove(name);
        return;
    }
    let _ = write_artifacts(name);
    if iterm2_dir().is_some() && !iterm2_upsert(name, name) {
        eprintln!("container-distro: warning: couldn't write an iTerm2 profile for `{name}`");
    }
}

/// Remove everything we generated for `name` — distro delete,
/// `migrate --out`, and resolved-out `terminal_profile`: the
/// `.terminal` file, the launcher script (and a parked `.real`), the
/// iTerm2 entry, and any imported Terminal profiles (matched by
/// marker, so user-renamed ones go too).
pub fn remove(name: &str) {
    iterm2_remove(name);
    terminal_remove(name);
    for p in [terminal_file(name), script_file(name)]
        .into_iter()
        .flatten()
    {
        let _ = fs::remove_file(&p);
        let mut real = p.clone();
        real.set_extension("sh.real");
        let _ = fs::remove_file(real);
    }
}

/// `rename`: drop `old`'s profiles, then regenerate under `new` —
/// `enabled` is the renamed spec's resolved `terminal_profile`.
pub fn rename(old: &str, new: &str, enabled: bool) {
    remove(old);
    sync(new, enabled);
    if enabled {
        let _ = ensure_profile(new);
    }
}

/// Sweep generated artifacts against the journaled distro set
/// (`sync-profiles`): `.terminal` files and launcher scripts for
/// names that no longer name a distro are removed, as are imported
/// Terminal profiles whose marker names a gone distro. The retired
/// `terminal-profiles.json` sidecar goes too — the launchers
/// themselves are the state now.
pub fn reconcile(keep: &BTreeSet<String>) {
    if let Ok(d) = state_dir() {
        let _ = fs::remove_file(d.join("terminal-profiles.json"));
    }
    if let Ok(d) = state_dir().map(|d| d.join("terminal-profiles"))
        && let Ok(rd) = fs::read_dir(d)
    {
        for f in rd.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) == Some("terminal")
                && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
                && !keep.contains(stem)
            {
                remove(stem);
            }
        }
    }
    if let Ok(d) = state_dir().map(|d| d.join("terminal-cmds"))
        && let Ok(rd) = fs::read_dir(d)
    {
        for f in rd.flatten() {
            let p = f.path();
            let stem = p.file_name().and_then(|s| s.to_str()).map(|s| {
                s.trim_end_matches(".sh.real")
                    .trim_end_matches(".sh")
                    .to_string()
            });
            if let Some(stem) = stem
                && !keep.contains(&stem)
            {
                let _ = fs::remove_file(&p);
            }
        }
    }
    for (_, marker) in window_settings() {
        if let Some(d) = marker
            && !keep.contains(&d)
        {
            terminal_remove(&d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conf_enabled_maps_to_source_intent() {
        assert_eq!(
            conf_terminal_profile("[windowsterminal]\nenabled = true"),
            Some(true)
        );
        assert_eq!(
            conf_terminal_profile("[windowsterminal]\nenabled = false"),
            Some(false)
        );
        assert_eq!(
            conf_terminal_profile("[WindowsTerminal]\nEnabled = FALSE"),
            Some(false)
        );
        assert_eq!(conf_terminal_profile("[shortcut]\nenabled = false"), None);
        assert_eq!(conf_terminal_profile("[windowsterminal]"), None);
    }

    #[test]
    fn guid_is_stable_and_shaped() {
        let g = iterm2_guid("ubuntu");
        assert_eq!(g, iterm2_guid("ubuntu"));
        assert_ne!(g, iterm2_guid("debian"));
        assert_eq!(g.len(), 36);
        assert_eq!(g.chars().filter(|c| *c == '-').count(), 4);
    }

    #[test]
    fn pick_name_never_takes_a_foreign_profile() {
        let mut settings = Vec::new();
        // Name is free → display wins.
        assert_eq!(
            pick_profile_name(&settings, "ubuntu", "Ubuntu").as_deref(),
            Some("Ubuntu")
        );
        // A user's profile squats on the name → ours takes a suffix.
        settings.push(("Ubuntu".into(), None));
        assert_eq!(
            pick_profile_name(&settings, "ubuntu", "Ubuntu").as_deref(),
            Some("Ubuntu (cm)")
        );
        // Both taken by others → give up rather than clobber.
        settings.push(("Ubuntu (cm)".into(), None));
        assert_eq!(pick_profile_name(&settings, "ubuntu", "Ubuntu"), None);
        // A profile with our marker is ours to refresh.
        settings.push(("Mine".into(), Some("ubuntu".into())));
        assert_eq!(
            pick_profile_name(&settings, "ubuntu", "Mine").as_deref(),
            Some("Mine")
        );
    }
}

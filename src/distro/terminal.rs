/*
 * Copyright (c) 2026 Daphne Pfister
 * SPDX-License-Identifier: BSD-2-Clause
 * See LICENSE file for full license text
 */

use crate::naming::state_dir;
use crate::wsl;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// `~/.container-distro` — a symlink to `state_dir()` giving a
/// space-free path into it. A profile's `CommandString` is
/// whitespace-tokenized rather than shell-parsed — quoting is passed
/// through literally — so the `Application Support` space truncates
/// the launcher path however it's written.
fn alias_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join(".container-distro"))
}

/// Create/reuse the `uchg`-locked alias symlink; `None` when a foreign
/// file squats on the name (never clobbered) or the link can't be
/// made. The flag keeps a rw-shared guest from repointing it — the
/// link feeds `CommandString`, so a repoint is host code exec.
fn ensure_alias() -> Option<PathBuf> {
    let target = state_dir().ok()?;
    let link = alias_dir()?;
    match fs::symlink_metadata(&link) {
        Ok(m) if m.is_symlink() => {
            if fs::read_link(&link).ok().as_deref() != Some(target.as_path()) {
                return None;
            }
            if !crate::distro::is_locked(&link) {
                crate::distro::lock_symlink(&link);
            }
            Some(link)
        }
        Ok(_) => None,
        Err(_) => {
            std::os::unix::fs::symlink(&target, &link).ok()?;
            crate::distro::lock_symlink(&link);
            Some(link)
        }
    }
}

/// `~/`-relative path to the launcher script — Terminal expands `~` in
/// a profile's run command before whitespace-tokenizing it, so the
/// tilde stays space-free even where a resolved home path never could
/// (a username or mount point with a space). `Some` only when the
/// alias exists, so the path always resolves.
fn alias_launch_path(name: &str) -> Option<String> {
    ensure_alias().map(|_| format!("~/.container-distro/terminal-cmds/{name}.sh"))
}

/// The command a Terminal profile's `CommandString` runs — the
/// launcher script itself (it's +x with a shebang; a `/bin/sh` prefix
/// would only add a process layer), reached through the alias so the
/// path is space-free. The fallback when the alias can't be made runs
/// `cm` directly (`~` survives tokenizing; cm expands it guest-side).
fn launch_command(name: &str) -> String {
    match alias_launch_path(name) {
        Some(p) => p,
        None => format!("{} --cd ~ -d {name}", cm_path()),
    }
}

/// iTerm2's `Command` — shell-parsed, so it can fall back to the
/// quoted real path when the alias is unavailable.
fn iterm2_command(name: &str) -> String {
    if let Some(p) = alias_launch_path(name) {
        return p;
    }
    match script_file(name) {
        Some(s) => format!("'{}'", s.display()),
        None => format!("'{}' --cd '~' -d '{name}'", cm_path()),
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

/// Write `name`'s launcher script (+x), `uchg`-locked at rest like the
/// journal assets — a rw-shared guest rewriting it would own the next
/// profile launch.
fn write_script_at(path: &Path, contents: &str) -> bool {
    if let Some(parent) = path.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    crate::distro::unlock(path);
    // No-follow: a link planted here would otherwise get the launcher
    // written (and locked open) at its target.
    let ok = crate::distro::write_nofollow(path, contents.as_bytes(), 0o755).is_ok();
    if ok {
        crate::distro::lock(path);
    }
    ok
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
/// `noWarnProcesses` exempts the launcher chain — `cm` execs
/// `container exec` — from the "terminate running process" prompt on
/// ⌘W/quit; Terminal can't see inside the container, so the guest's
/// process tree never factors into the warning anyway.
fn terminal_profile_dict(name: &str, profile: &str) -> plist::Value {
    plist::Value::Dictionary(plist::Dictionary::from_iter([
        ("name", plist::Value::from(profile)),
        ("type", plist::Value::from("Window Settings")),
        ("ProfileCurrentVersion", plist::Value::from(2.09)),
        ("CommandString", plist::Value::from(launch_command(name))),
        ("RunCommandAsShell", plist::Value::from(true)),
        ("shellExitAction", plist::Value::from(1)),
        (
            "noWarnProcesses",
            plist::Value::Array(vec![
                plist::Dictionary::from_iter([("ProcessName", plist::Value::from("cm"))]).into(),
                plist::Dictionary::from_iter([("ProcessName", plist::Value::from("container"))])
                    .into(),
            ]),
        ),
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

/// A plist value in `defaults`'s old-style syntax — `()` arrays and
/// `{}` dicts nest, scalars quote or print bare.
fn frag_val(v: &plist::Value) -> String {
    match v {
        plist::Value::String(s) => plist_quote(s),
        plist::Value::Boolean(b) => b.to_string(),
        plist::Value::Real(r) => r.to_string(),
        plist::Value::Integer(i) => i.to_string(),
        plist::Value::Array(a) => format!(
            "({})",
            a.iter().map(frag_val).collect::<Vec<_>>().join(", ")
        ),
        plist::Value::Dictionary(d) => format!("{{ {} }}", fragment(d)),
        other => plist_quote(&format!("{other:?}")),
    }
}

/// `key`-`dict` as a `defaults` plist fragment — old-style syntax is
/// all `-dict-add` accepts.
fn fragment(dict: &plist::Dictionary) -> String {
    dict.iter()
        .map(|(k, v)| format!("{} = {};", plist_quote(k), frag_val(v)))
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
    crate::distro::unlock(&f);
    if let Err(e) = plist::to_file_xml(&f, &terminal_profile_dict(name, &profile)) {
        eprintln!(
            "container-distro: warning: couldn't write {}: {e}",
            f.display()
        );
        return false;
    }
    crate::distro::lock(&f);
    if !write_script(name) {
        eprintln!("container-distro: warning: couldn't write {name}'s launcher script");
        return false;
    }
    true
}

/// `open` `name`'s `.terminal` launcher — Terminal imports the profile
/// (a no-op when the name's already imported). Its *launch* half is
/// unreliable on a dormant Terminal — the window can queue
/// indefinitely or never spawn — so `open_terminal` doesn't rely on
/// it for the window.
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

/// `name of every settings set` and `count of windows` from Terminal's
/// live model — AS sees the model; the plist read lags a running
/// Terminal's flushes. `([], 0)` when the query fails.
fn model_state() -> (Vec<String>, usize) {
    let Ok(out) = Command::new("osascript")
        .args([
            "-e",
            "tell application \"Terminal\"
               try
                 set ns to name of every settings set
               on error
                 set ns to {}
               end try
               get ns & (count of windows)
             end tell",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return (Vec::new(), 0);
    };
    if !out.status.success() {
        return (Vec::new(), 0);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts: Vec<&str> = text.trim().split(", ").collect();
    let windows = parts.pop().and_then(|p| p.trim().parse().ok()).unwrap_or(0);
    (
        parts.iter().map(|s| s.trim().to_string()).collect(),
        windows,
    )
}

/// A window *now* running the launcher — `open`'s launch half can't be
/// trusted on a dormant Terminal, so `do script` makes the window
/// deterministically. It then adopts the imported settings set (the
/// distro title and close-on-clean-exit): ours is the model entry
/// absent from `before` — the marker isn't scriptable and the
/// import's model name is a filename dedup — or, when the profile was
/// already imported, `known` (the marker-bearing plist key). A miss
/// just leaves the window default-styled.
fn launch_window(name: &str, before: &[String], known: &str) -> bool {
    let Some(script) = script_file(name) else {
        return false;
    };
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let before_list = before
        .iter()
        .map(|n| format!("\"{}\"", esc(n)))
        .collect::<Vec<_>>()
        .join(", ");
    let cmd = alias_launch_path(name).unwrap_or_else(|| format!("'{}'", script.display()));
    Command::new("osascript")
        .args([
            "-e",
            &format!(
                "tell application \"Terminal\"
                   set t to do script \"{}\"
                   set adopted to false
                   try
                     if \"{1}\" is not \"\" then
                       set current settings of t to settings set \"{1}\"
                       set adopted to true
                     end if
                   end try
                   repeat 24 times
                     if adopted then exit repeat
                     try
                       repeat with s in settings sets
                         if (name of s) is not in {{{2}}} then
                           set current settings of t to s
                           set adopted to true
                           exit repeat
                         end if
                       end repeat
                     end try
                     if not adopted then delay 0.25
                   end repeat
                 end tell",
                esc(&cmd),
                esc(known),
                before_list
            ),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `container distro terminal` / `cm --terminal`: refresh the
/// artifacts, `open` the launcher to import the profile through
/// Terminal's own model — then make sure a window actually appeared:
/// `open`'s launch queues (or drops) on a dormant Terminal, so when
/// no new window materialized, `do script` spawns one and adopts the
/// imported settings set.
pub fn open_terminal(name: &str) -> anyhow::Result<()> {
    if !write_artifacts(name) {
        anyhow::bail!("couldn't write {name}'s launcher files");
    }
    // A parked `.real` means an earlier silent import armed the
    // self-replacing no-op and its window hasn't flushed yet — the
    // real script is already back in place (so a window running now
    // launches the distro, which is what an open wants anyway); drop
    // the park.
    if let Some(mut real) = script_file(name) {
        real.set_extension("sh.real");
        crate::distro::unlock(&real);
        let _ = fs::remove_file(real);
    }
    // The pre-import model names: AS's live list when Terminal runs,
    // else the plist the model boots from — the name-diff then finds
    // exactly the entry the import adds, however Terminal dedupes it.
    let (before, windows) = if terminal_running() {
        model_state()
    } else {
        (window_settings().into_iter().map(|(k, _)| k).collect(), 0)
    };
    let _ = open_launcher(name);
    for _ in 0..8 {
        if model_state().1 > windows {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let known = terminal_owned(name).into_iter().next().unwrap_or_default();
    if !launch_window(name, &before, &known) {
        anyhow::bail!("couldn't open {name}'s profile in Terminal.app");
    }
    Ok(())
}

/// `name`'s imported profile as the plist sees it — the marker-bearing
/// `Window Settings` key and its dict. A stale shape (an older
/// `CommandString`, missing keys like `noWarnProcesses`) is stale and
/// gets repaired by re-import.
fn imported(name: &str) -> Option<(String, plist::Dictionary)> {
    plist::from_file::<_, plist::Value>(terminal_plist())
        .ok()?
        .into_dictionary()?
        .get(WINDOW_SETTINGS)?
        .as_dictionary()?
        .iter()
        .find(|(_, v)| {
            v.as_dictionary()
                .and_then(|p| p.get(MARKER))
                .and_then(plist::Value::as_string)
                == Some(name)
        })
        .and_then(|(k, v)| v.as_dictionary().map(|d| (k.clone(), d.clone())))
}

/// Loose plist-value equality — the `defaults` fragment path stores
/// scalars as strings (`"1"`, `"true"`), so an imported value may be
/// the string form of what was generated.
fn plist_eq(a: &plist::Value, b: &plist::Value) -> bool {
    fn scalar(v: &plist::Value) -> Option<String> {
        Some(match v {
            plist::Value::String(s) => s.clone(),
            plist::Value::Integer(i) => i.to_string(),
            plist::Value::Real(r) => r.to_string(),
            plist::Value::Boolean(b) => b.to_string(),
            _ => return None,
        })
    }
    match (scalar(a), scalar(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// Whether `have` carries every key `want` generates — Terminal only
/// *adds* keys on import, so a subset match means fresh. The display
/// keys (`name`, `CustomTitle`) are exempt: a deduped import is
/// legitimately `NAME 1` while the command and behavior keys still
/// say ours.
fn profile_fresh(have: &plist::Dictionary, want: &plist::Dictionary) -> bool {
    want.iter().all(|(k, v)| {
        matches!(k.as_str(), "name" | "CustomTitle") || have.get(k).is_some_and(|h| plist_eq(h, v))
    })
}

/// Install `name`'s profile into Terminal *without* a window — the
/// create/`--terminal-profile`/`sync-profiles` path.
///
/// - A marker-bearing profile already imported carrying every key we
///   generate → nothing to do. An imported-but-stale shape (an older
///   `CommandString`, a missing `noWarnProcesses`) is removed first so
///   the import lands fresh — the profile can't be edited in place
///   behind a running Terminal's back.
/// - Terminal not running → write `Window Settings` via `defaults`:
///   cfprefsd serializes it, and with no live model nothing can flush
///   it away — the profile is simply there at Terminal's next launch.
/// - Terminal running → a `defaults` write would die at Terminal's
///   next prefs flush (its model never loaded the entry). Instead the
///   launcher's script is swapped for a self-replacing no-op —
///   `mv -f "$0.real" "$0"` — the real command parked alongside; the
///   `.terminal` file opens, Terminal imports *through its model* and
///   the launched window runs the no-op: it unflags, swaps, reflags
///   (artifacts rest `uchg`-locked) and exits, so `shellExitAction`
///   closes the window at once. Any later run of the restored script
///   behaves normally — the no-op can only fire once, during the
///   import.
pub fn ensure_profile(name: &str) -> bool {
    if let Some((key, dict)) = imported(name) {
        if let plist::Value::Dictionary(want) = terminal_profile_dict(name, &key)
            && profile_fresh(&dict, &want)
        {
            return true;
        }
        terminal_remove(name);
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
    // The artifacts rest `uchg`-locked, so the swap unlocks first and
    // re-locks after — host-side, where chflags is allowed (a guest
    // has no flag operations through virtiofs, hence the boundary).
    if !write_script_at(
        &script,
        "#!/bin/sh\n/usr/bin/chflags nouchg \"$0\" \"$0.real\" 2>/dev/null\nmv -f \"$0.real\" \"$0\"\n/usr/bin/chflags uchg \"$0\" 2>/dev/null\nexit 0\n",
    ) {
        crate::distro::unlock(&real);
        let _ = fs::remove_file(&real);
        return false;
    }
    if !open_launcher(name) {
        // The import never happened — put the real command back.
        let _ = write_script_at(&script, &script_contents(name));
        crate::distro::unlock(&real);
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
        crate::distro::unlock(&f);
        return fs::remove_file(&f).is_ok() || !f.exists();
    }
    if let Some(parent) = f.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    // `uchg`-locked like the other launch artifacts — the file sits
    // under a shared-able home and its Command runs on the host.
    crate::distro::unlock(&f);
    let body = serde_json::to_string_pretty(&json!({ "Profiles": profiles })).unwrap();
    let ok = crate::distro::write_nofollow(&f, body.as_bytes(), 0o644).is_ok();
    if ok {
        crate::distro::lock(&f);
    }
    ok
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
        "Command": iterm2_command(name),
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
        crate::distro::unlock(&p);
        let _ = fs::remove_file(&p);
        let mut real = p.clone();
        real.set_extension("sh.real");
        crate::distro::unlock(&real);
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
                crate::distro::unlock(&p);
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
    use std::fs;

    #[test]
    fn launcher_write_replaces_planted_link() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, b"keep").unwrap();
        let script = dir.path().join("d1.sh");
        std::os::unix::fs::symlink(&victim, &script).unwrap();
        let wrote = write_script_at(&script, "#!/bin/sh\n");
        let is_link = fs::symlink_metadata(&script).map(|m| m.file_type().is_symlink());
        crate::distro::unlock(&script);
        assert!(wrote);
        assert!(!is_link.unwrap(), "the planted link was written through");
        assert_eq!(fs::read(&victim).unwrap(), b"keep");
        assert_eq!(fs::read(&script).unwrap(), b"#!/bin/sh\n");
    }

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

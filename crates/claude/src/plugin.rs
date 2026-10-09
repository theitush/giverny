//! The `giverny` Claude Code plugin: the manager the management panel needs,
//! carried inside the binary.
//!
//! The pane shows Running, Next up with ETAs and Done when a managing
//! session writes a feed; `giverny manage` writes it, and this plugin's
//! `manage` skill (`/giverny:manage`) tells Claude how to run a manager session
//! with it. Nothing outside Giverny is needed: no separate skill, no GitHub.
//!
//! How it reaches Claude Code, measured on 2.1.283:
//! - The plugin lives in a local **directory marketplace** under Giverny's
//!   own config dir. Two keys in an account's `settings.json` —
//!   `extraKnownMarketplaces.giverny` (a `directory` source) and
//!   `enabledPlugins["giverny@giverny"]` — are all Claude Code needs; no
//!   `claude plugin install`, no network.
//! - A directory source is loaded straight from the directory at session
//!   start, not from a cache, so the files this binary writes are what the
//!   next session runs. The plugin's version is the binary's.
//! - Its `bin/` is on the Bash tool's `PATH`, so its launchers work in any
//!   session and in its subagents without Giverny on `PATH`:
//!   `giverny-manage` (the manage skill's), `giverny-eta`
//!   (any subagent's ETA) and `giverny-hook` (the hook's).
//! - It coexists with a project's own `/manage` skill: plugin skills are
//!   namespaced. Its one command, `/giverny:clear-done`, runs
//!   `giverny-manage clear-done` to clear the management panel's Done rows.
//! - Removing the keys unloads it; a missing directory makes Claude Code skip
//!   it silently.
//!
//! - `hooks/hooks.json` carries a `PostToolUse` hook running `giverny-hook`
//!   ([`crate::plugin_hook`]): a dispatcher that just started a worker with
//!   no estimate is asked for one, and five minutes into its work the worker
//!   is asked, once, to correct it, so every subagent gets an ETA. Any other
//!   call costs a few `stat`s.
//!
//! The skill is its own switch (`management_panel.manage_skill`, on by
//! default): off, the plugin is written without `skills/`, so the hook, the
//! launchers and `/giverny:clear-done` the pane needs stay, and only
//! the skill goes — from every account at once, since they all load this
//! one directory.
//!
//! The settings keys follow the house rules the other management-panel key does:
//! written only with `claude.management_panel` on, never over a
//! marketplace called `giverny` that is not ours, removed when the setting
//! goes off and on uninstall, and a no-op writes nothing.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub const MARKETPLACE: &str = "giverny";
pub const PLUGIN: &str = "giverny";
pub const PLUGIN_ID: &str = "giverny@giverny";
/// The marketplace directory's name under Giverny's config base.
pub const DIR_NAME: &str = "claude-plugin";
/// The plugin's version: the binary's.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const SKILL: &str = include_str!("../plugin/skills/manage/SKILL.md");
/// `/giverny:clear-done`: the management panel's Done rows, cleared.
const CLEAR_DONE: &str = include_str!("../plugin/commands/clear-done.md");

/// Where the marketplace lives: `<giverny config base>/claude-plugin`.
pub fn marketplace_dir(base: &Path) -> PathBuf {
    base.join(DIR_NAME)
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A `bin/` launcher: runs `<the binary that wrote it> <sub>`.
fn wrapper(exes: &[String], sub: &str) -> String {
    let list: Vec<String> = exes.iter().map(|e| sh_quote(e)).collect();
    format!(
        "#!/bin/sh\n\
         # Written by Giverny {VERSION}; rewritten each time it starts with the\n\
         # management panel on. Runs `giverny {sub}`: see `giverny {sub} --help`.\n\
         for g in {list}; do\n  \
           if [ -x \"$g\" ]; then exec \"$g\" {sub} \"$@\"; fi\n\
         done\n\
         if command -v giverny >/dev/null 2>&1; then exec giverny {sub} \"$@\"; fi\n\
         echo \"$0: the Giverny that wrote it is gone\" >&2\n\
         exit 127\n",
        list = list.join(" ")
    )
}

/// `hooks/hooks.json`: a `PostToolUse` hook, `giverny-hook`, which asks a
/// dispatcher for the estimate of a worker it just started, and a worker
/// five minutes in to correct it; on a manager's own calls it delivers
/// the session's `ask`/`reply` messages and renews its resource leases;
/// quiet and exit 0 whatever happens.
fn session_hooks() -> Value {
    json!({
        "description": "Giverny: ETAs for subagents, and manager sessions' upkeep",
        "hooks": {
            "PostToolUse": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": "\"${CLAUDE_PLUGIN_ROOT}/bin/giverny-hook\" 2>/dev/null || true"
                }]
            }]
        }
    })
}

/// Every file of the marketplace: (path under the dir, contents, executable).
/// `skill` false leaves the manage skill out.
pub fn files(exes: &[String], skill: bool) -> Vec<(&'static str, String, bool)> {
    let marketplace = json!({
        "name": MARKETPLACE,
        "owner": { "name": "Giverny" },
        "description": "The Claude Code plugin that ships inside Giverny",
        "plugins": [{
            "name": PLUGIN,
            "source": "./plugins/giverny",
            "description": "Manage subagents and show them in Giverny's management panel",
            "version": VERSION
        }]
    });
    let plugin = json!({
        "name": PLUGIN,
        "version": VERSION,
        "description": "Manage subagents and show them in Giverny's management panel: \
                        Running, Next up with ETAs, Done",
        "author": { "name": "Giverny" }
    });
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default() + "\n";
    let mut out = vec![
        (
            ".claude-plugin/marketplace.json",
            pretty(&marketplace),
            false,
        ),
        (
            "plugins/giverny/.claude-plugin/plugin.json",
            pretty(&plugin),
            false,
        ),
        (
            "plugins/giverny/commands/clear-done.md",
            CLEAR_DONE.to_string(),
            false,
        ),
        (
            "plugins/giverny/bin/giverny-manage",
            wrapper(exes, "manage"),
            true,
        ),
        (
            "plugins/giverny/bin/giverny-eta",
            wrapper(exes, "eta"),
            true,
        ),
        (
            "plugins/giverny/bin/giverny-hook",
            wrapper(exes, "hook"),
            true,
        ),
        (
            "plugins/giverny/hooks/hooks.json",
            pretty(&session_hooks()),
            false,
        ),
    ];
    if skill {
        out.push((SKILL_PATH, SKILL.to_string(), false));
    }
    out
}

/// Where the manage skill sits in the marketplace.
pub const SKILL_PATH: &str = "plugins/giverny/skills/manage/SKILL.md";

/// The binary as the wrapper should name it: this one, and on Windows also
/// its path from inside WSL (a WSL account's Claude runs it through interop).
pub fn exe_candidates() -> Vec<String> {
    let mut out = Vec::new();
    // The stable link when there is one ([`crate::hooks::link_path`]): the
    // wrapper then stays byte-identical across rebuilds and reinstalls.
    #[cfg(unix)]
    if let Some(link) = crate::hooks::link_path()
        && link.exists()
    {
        return vec![link.display().to_string()];
    }
    if let Some(exe) = crate::hooks::running_exe() {
        out.push(exe.display().to_string());
        #[cfg(windows)]
        if let Some(distro) = crate::wsl::default_distro()
            && let Some(inside) = crate::wsl::to_wsl_path(&distro, &exe)
        {
            out.push(inside);
        }
    }
    out
}

/// Write the marketplace into `dir`, touching only files whose bytes differ,
/// and removing anything else there (the directory is ours alone). Returns
/// whether anything changed. `skill` false writes the plugin without its
/// manage skill (and so prunes one written before).
pub fn sync(dir: &Path, exes: &[String], skill: bool) -> std::io::Result<bool> {
    let want = files(exes, skill);
    let mut changed = false;
    for (rel, body, exec) in &want {
        let path = dir.join(rel);
        if std::fs::read(&path).is_ok_and(|b| b == body.as_bytes()) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("giverny-tmp");
        std::fs::write(&tmp, body)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if *exec { 0o755 } else { 0o644 };
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        let _ = exec;
        std::fs::rename(&tmp, &path)?;
        changed = true;
    }
    let keep: Vec<PathBuf> = want.iter().map(|(rel, _, _)| dir.join(rel)).collect();
    changed |= prune(dir, &keep)?;
    Ok(changed)
}

/// Remove every file under `dir` not in `keep`, and directories left empty.
fn prune(dir: &Path, keep: &[PathBuf]) -> std::io::Result<bool> {
    let mut changed = false;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(false);
    };
    for e in entries.flatten() {
        let p = e.path();
        if e.file_type()?.is_dir() {
            changed |= prune(&p, keep)?;
            if std::fs::read_dir(&p)?.next().is_none() {
                std::fs::remove_dir(&p)?;
                changed = true;
            }
        } else if !keep.contains(&p) {
            std::fs::remove_file(&p)?;
            changed = true;
        }
    }
    Ok(changed)
}

/// Delete the marketplace directory — only when it is ours (its
/// `marketplace.json` names the `giverny` marketplace).
pub fn remove_dir(dir: &Path) -> std::io::Result<bool> {
    let manifest = dir.join(".claude-plugin/marketplace.json");
    let ours = std::fs::read(&manifest)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .is_some_and(|v| v.get("name").and_then(Value::as_str) == Some(MARKETPLACE));
    if !ours {
        return Ok(false);
    }
    std::fs::remove_dir_all(dir)?;
    Ok(true)
}

/// The marketplace path as the account's Claude Code must read it: a WSL
/// account on Windows reads a Windows directory through `/mnt/<drive>`.
pub fn dir_for(settings_path: &Path, dir: &Path) -> String {
    #[cfg(windows)]
    if let Some((distro, _)) = crate::wsl::split_unc(settings_path)
        && let Some(inside) = crate::wsl::to_wsl_path(&distro, dir)
    {
        return inside;
    }
    let _ = settings_path;
    dir.display().to_string()
}

/// Is this `extraKnownMarketplaces` entry (or `known_marketplaces.json`
/// entry) Giverny's: a directory source ending in `giverny/claude-plugin`?
fn is_our_source(entry: &Value) -> bool {
    let src = entry.get("source");
    src.and_then(|s| s.get("source")).and_then(Value::as_str) == Some("directory")
        && src
            .and_then(|s| s.get("path"))
            .and_then(Value::as_str)
            .is_some_and(|p| {
                let p = p.trim_end_matches(['/', '\\']).replace('\\', "/");
                p.ends_with(&format!("giverny/{DIR_NAME}"))
            })
}

/// Is the plugin configured (ours, enabled) in this settings file?
pub fn installed_in(settings_path: &Path) -> bool {
    let Some(root) = std::fs::read(settings_path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return false;
    };
    root.get("extraKnownMarketplaces")
        .and_then(|m| m.get(MARKETPLACE))
        .is_some_and(is_our_source)
        && root
            .get("enabledPlugins")
            .and_then(|m| m.get(PLUGIN_ID))
            .and_then(Value::as_bool)
            == Some(true)
}

/// Add or remove the plugin's two keys in one account's `settings.json`.
/// Returns whether the file changed.
///
/// On: `extraKnownMarketplaces.giverny` points at `dir` (refused when a
/// marketplace of that name is someone else's) and `enabledPlugins`
/// gains `giverny@giverny: true` unless the user already set it — a user who
/// ran `claude plugin disable` keeps it disabled. Off: both are removed, only
/// when ours, a map we emptied goes with them, and Claude Code's own record
/// of the marketplace (`plugins/known_marketplaces.json`) loses its entry
/// too, as `claude plugin marketplace remove` would.
pub fn set_plugin(settings_path: &Path, dir: &Path, enable: bool) -> anyhow::Result<bool> {
    let changed = set_keys(settings_path, dir, enable)?;
    if !enable && let Some(config) = settings_path.parent() {
        forget_known(&config.join("plugins").join("known_marketplaces.json"));
    }
    Ok(changed)
}

fn set_keys(settings_path: &Path, dir: &Path, enable: bool) -> anyhow::Result<bool> {
    let mut root: Value = match std::fs::read(settings_path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("won't touch unparseable settings: {e}"))?,
        Err(_) if !enable => return Ok(false),
        Err(_) => json!({}),
    };
    let obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("settings root is not an object"))?;
    let current = obj
        .get("extraKnownMarketplaces")
        .and_then(|m| m.get(MARKETPLACE))
        .cloned();
    if let Some(entry) = &current
        && !is_our_source(entry)
    {
        anyhow::bail!("a marketplace called `giverny` is already configured — leaving it alone");
    }
    let want = json!({ "source": { "source": "directory", "path": dir_for(settings_path, dir) } });
    let enabled = obj.get("enabledPlugins").and_then(|m| m.get(PLUGIN_ID));
    match (enable, &current) {
        (true, Some(e)) if *e == want && enabled.is_some() => return Ok(false),
        (false, None) if enabled.is_none() => return Ok(false),
        _ => {}
    }
    let backup = settings_path.with_extension("json.giverny-bak");
    if settings_path.exists() && !backup.exists() {
        let _ = std::fs::copy(settings_path, &backup);
    }
    if enable {
        let m = obj
            .entry("extraKnownMarketplaces")
            .or_insert_with(|| json!({}));
        m.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("extraKnownMarketplaces is not an object"))?
            .insert(MARKETPLACE.into(), want);
        let p = obj.entry("enabledPlugins").or_insert_with(|| json!({}));
        p.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("enabledPlugins is not an object"))?
            .entry(PLUGIN_ID)
            .or_insert(json!(true));
    } else {
        for (map, key) in [
            ("extraKnownMarketplaces", MARKETPLACE),
            ("enabledPlugins", PLUGIN_ID),
        ] {
            let emptied = obj
                .get_mut(map)
                .and_then(Value::as_object_mut)
                .is_some_and(|m| m.remove(key).is_some() && m.is_empty());
            if emptied {
                obj.remove(map);
            }
        }
    }
    let tmp = settings_path.with_extension("json.tmp");
    if let Some(d) = settings_path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&tmp, serde_json::to_vec_pretty(&root)?)?;
    std::fs::rename(&tmp, settings_path)?;
    Ok(true)
}

/// Drop our entry from Claude Code's `known_marketplaces.json`, which it
/// fills from `extraKnownMarketplaces` and does not empty by itself.
fn forget_known(path: &Path) {
    let Some(mut root) = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return;
    };
    let Some(map) = root.as_object_mut() else {
        return;
    };
    if !map.get(MARKETPLACE).is_some_and(is_our_source) {
        return;
    }
    map.remove(MARKETPLACE);
    let tmp = path.with_extension("json.giverny-tmp");
    if serde_json::to_vec_pretty(&root)
        .ok()
        .and_then(|b| std::fs::write(&tmp, b).ok())
        .is_some()
    {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("giverny-plugin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_skill_is_generic() {
        // What ships to every machine assumes no issue tracker, board or
        // helper scripts: only Claude Code and `giverny-manage`.
        let words: Vec<String> = SKILL
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric() && c != '-')
            .map(String::from)
            .collect();
        for word in [
            "coo",
            "gh",
            "github",
            "board",
            "machine-budget",
            "estimate-audit",
        ] {
            assert!(
                !words.iter().any(|w| w == word),
                "the plugin's skill mentions {word:?}"
            );
        }
        assert!(SKILL.starts_with("---\nname: manage\n"));
        assert!(SKILL.contains("giverny-manage plan"));
    }

    #[test]
    fn sync_writes_once_and_prunes_what_is_not_ours() {
        let d = scratch("sync").join(DIR_NAME);
        let exes = vec!["/opt/giverny/giverny".to_string()];
        assert!(sync(&d, &exes, true).unwrap());
        assert!(!sync(&d, &exes, true).unwrap(), "a second sync is a no-op");
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(d.join("plugins/giverny/.claude-plugin/plugin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["version"], VERSION);
        let wrapper =
            std::fs::read_to_string(d.join("plugins/giverny/bin/giverny-manage")).unwrap();
        assert!(wrapper.contains("'/opt/giverny/giverny'"), "{wrapper}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(d.join("plugins/giverny/bin/giverny-manage"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        // An old version's leftover skill goes; a moved binary rewrites the wrapper.
        std::fs::create_dir_all(d.join("plugins/giverny/skills/old")).unwrap();
        std::fs::write(d.join("plugins/giverny/skills/old/SKILL.md"), "x").unwrap();
        assert!(sync(&d, &["/elsewhere/giverny".into()], true).unwrap());
        assert!(!d.join("plugins/giverny/skills/old").exists());
        assert!(remove_dir(&d).unwrap());
        assert!(!d.exists());
        // A directory that is not ours is never deleted.
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("keep"), "mine").unwrap();
        assert!(!remove_dir(&d).unwrap());
        assert!(d.join("keep").exists());
    }

    #[test]
    fn the_skill_switch_takes_only_the_skill() {
        let d = scratch("skill").join(DIR_NAME);
        let exes = vec!["/opt/giverny/giverny".to_string()];
        assert!(sync(&d, &exes, true).unwrap());
        assert!(d.join(SKILL_PATH).exists());
        // Off: the skill goes, and its emptied directories with it; the
        // hook, the wrapper and the command stay.
        assert!(sync(&d, &exes, false).unwrap());
        assert!(!d.join(SKILL_PATH).exists());
        assert!(!d.join("plugins/giverny/skills").exists());
        for kept in [
            "plugins/giverny/hooks/hooks.json",
            "plugins/giverny/bin/giverny-manage",
            "plugins/giverny/commands/clear-done.md",
            "plugins/giverny/.claude-plugin/plugin.json",
            ".claude-plugin/marketplace.json",
        ] {
            assert!(d.join(kept).exists(), "{kept} stays");
        }
        assert!(!sync(&d, &exes, false).unwrap(), "a second sync is a no-op");
        // On again: back, byte for byte.
        assert!(sync(&d, &exes, true).unwrap());
        assert_eq!(std::fs::read_to_string(d.join(SKILL_PATH)).unwrap(), SKILL);
        assert!(remove_dir(&d).unwrap());
    }

    #[test]
    fn a_sync_prunes_a_dropped_hook() {
        // A plugin an older Giverny wrote with a SessionStart hook still
        // holds that hook's reply file; the next sync removes it, and
        // hooks.json carries no SessionStart hook.
        let d = scratch("dropped-hook").join(DIR_NAME);
        let exes = vec!["/opt/giverny/giverny".to_string()];
        let hooks = d.join("plugins/giverny/hooks/hooks.json");
        let reply = d.join("plugins/giverny/hooks/session-start.json");
        assert!(sync(&d, &exes, true).unwrap());
        std::fs::write(&reply, "{}").unwrap();
        std::fs::write(
            &hooks,
            r#"{"hooks":{"SessionStart":[{"matcher":"startup","hooks":[]}]}}"#,
        )
        .unwrap();
        assert!(sync(&d, &exes, true).unwrap());
        assert!(!reply.exists(), "the old reply is pruned");
        let h: Value = serde_json::from_slice(&std::fs::read(&hooks).unwrap()).unwrap();
        assert!(h["hooks"].get("SessionStart").is_none(), "{h}");
        assert!(h["hooks"].get("PostToolUse").is_some(), "{h}");
        assert!(!sync(&d, &exes, true).unwrap(), "a second sync is a no-op");
        assert!(remove_dir(&d).unwrap());
    }

    #[test]
    fn the_re_estimate_hook_is_always_there_and_quiet() {
        let d = scratch("nudge").join(DIR_NAME);
        // A wrapper naming no binary at all: the hook still exits 0, silent.
        assert!(sync(&d, &["/nonexistent/giverny".into()], true).unwrap());
        let h: Value = serde_json::from_slice(
            &std::fs::read(d.join("plugins/giverny/hooks/hooks.json")).unwrap(),
        )
        .unwrap();
        let post = &h["hooks"]["PostToolUse"][0];
        assert_eq!(post["matcher"], "*");
        let cmd = post["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("bin/giverny-hook\""), "{cmd}");
        #[cfg(unix)]
        {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .env("CLAUDE_PLUGIN_ROOT", d.join("plugins/giverny"))
                .env("PATH", "/usr/bin:/bin")
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");
        }
        assert!(remove_dir(&d).unwrap());
    }

    #[test]
    fn settings_on_off_round_trips_and_leaves_the_rest() {
        let d = scratch("settings");
        let s = d.join("settings.json");
        let mine = r#"{"enabledPlugins":{"other@x":true},"model":"opus"}"#;
        std::fs::write(&s, mine).unwrap();
        let dir = d.join("giverny").join(DIR_NAME);
        assert!(set_plugin(&s, &dir, true).unwrap());
        assert!(installed_in(&s));
        assert!(!set_plugin(&s, &dir, true).unwrap(), "no-op writes nothing");
        let v: Value = serde_json::from_slice(&std::fs::read(&s).unwrap()).unwrap();
        assert_eq!(v["enabledPlugins"]["other@x"], true);
        assert_eq!(v["enabledPlugins"][PLUGIN_ID], true);
        assert_eq!(
            v["extraKnownMarketplaces"]["giverny"]["source"]["path"],
            dir.display().to_string()
        );
        // Claude Code's own record of the marketplace.
        let known = d.join("plugins/known_marketplaces.json");
        std::fs::create_dir_all(known.parent().unwrap()).unwrap();
        std::fs::write(
            &known,
            serde_json::to_vec(&json!({
                "giverny": v["extraKnownMarketplaces"]["giverny"].clone(),
                "theirs": {"source": {"source": "github", "repo": "a/b"}}
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(set_plugin(&s, &dir, false).unwrap());
        assert!(!installed_in(&s));
        let v: Value = serde_json::from_slice(&std::fs::read(&s).unwrap()).unwrap();
        let orig: Value = serde_json::from_str(mine).unwrap();
        assert_eq!(v, orig, "off leaves the file as it was, as JSON");
        let k: Value = serde_json::from_slice(&std::fs::read(&known).unwrap()).unwrap();
        assert!(k.get("giverny").is_none() && k.get("theirs").is_some());
        assert!(
            !set_plugin(&s, &dir, false).unwrap(),
            "off twice is a no-op"
        );
    }

    #[test]
    fn off_on_a_file_without_us_touches_nothing_and_on_creates_one() {
        let d = scratch("absent");
        let s = d.join("settings.json");
        let dir = d.join("giverny").join(DIR_NAME);
        assert!(!set_plugin(&s, &dir, false).unwrap());
        assert!(!s.exists(), "off never creates the file");
        let body = br#"{"model": "opus"}"#;
        std::fs::write(&s, body).unwrap();
        assert!(!set_plugin(&s, &dir, false).unwrap());
        assert_eq!(std::fs::read(&s).unwrap(), body.to_vec());
        assert!(!s.with_extension("json.giverny-bak").exists());
        std::fs::remove_file(&s).unwrap();
        assert!(set_plugin(&s, &dir, true).unwrap());
        assert!(installed_in(&s));
        assert!(set_plugin(&s, &dir, false).unwrap());
        assert_eq!(std::fs::read_to_string(&s).unwrap().trim(), "{}");
    }

    #[test]
    fn someone_elses_giverny_marketplace_and_a_users_disable_are_kept() {
        let d = scratch("theirs");
        let s = d.join("settings.json");
        let dir = d.join("giverny").join(DIR_NAME);
        let theirs = r#"{"extraKnownMarketplaces":{"giverny":{"source":{"source":"github","repo":"x/giverny"}}}}"#;
        std::fs::write(&s, theirs).unwrap();
        assert!(set_plugin(&s, &dir, true).is_err());
        assert!(set_plugin(&s, &dir, false).is_err());
        assert_eq!(std::fs::read_to_string(&s).unwrap(), theirs);

        std::fs::write(
            &s,
            format!(r#"{{"enabledPlugins":{{"{PLUGIN_ID}":false}}}}"#),
        )
        .unwrap();
        assert!(set_plugin(&s, &dir, true).unwrap());
        let v: Value = serde_json::from_slice(&std::fs::read(&s).unwrap()).unwrap();
        assert_eq!(
            v["enabledPlugins"][PLUGIN_ID], false,
            "their disable stands"
        );
        assert!(!installed_in(&s));
    }
}

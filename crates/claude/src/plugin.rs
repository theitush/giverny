//! The `giverny` Claude Code plugin: the orchestrator the agents pane needs,
//! carried inside the binary (giverny#101).
//!
//! The pane shows Running, Next up with ETAs and Done when an orchestrating
//! session writes a feed; `giverny pass` writes it, and this plugin's
//! `orchestrate` skill (`/giverny:orchestrate`) tells Claude how to run a pass
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
//! - Its `bin/` is on the Bash tool's `PATH`, so `giverny-pass` works in any
//!   session and in its subagents without Giverny on `PATH`.
//! - It coexists with a project's own `/orchestrate` skill: plugin skills are
//!   namespaced. Its one command, `/giverny:clear-done`, runs
//!   `giverny-pass clear-done` to clear the agents pane's Done rows.
//! - Removing the keys unloads it; a missing directory makes Claude Code skip
//!   it silently.
//!
//! - With `claude.orchestrate_by_default` also on (giverny#130), the plugin
//!   carries a `SessionStart` hook (`hooks/hooks.json`) whose command prints
//!   `hooks/orchestrate-by-default.json`: an `additionalContext` telling the
//!   session to run anything longer than about a minute as a pass of
//!   subagents. Off, both files are pruned like any other stale file.
//!
//! The settings keys follow the house rules the other agents-pane key does
//! (giverny#68): written only with `claude.agents_pane` on, never over a
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

const SKILL: &str = include_str!("../plugin/skills/orchestrate/SKILL.md");
/// `/giverny:clear-done`: the agents pane's Done rows, cleared (giverny#112).
const CLEAR_DONE: &str = include_str!("../plugin/commands/clear-done.md");
/// What the `SessionStart` hook adds to a new session's context with
/// `claude.orchestrate_by_default` on (giverny#130).
pub const ORCHESTRATE_BY_DEFAULT: &str = include_str!("../plugin/hooks/orchestrate-by-default.md");

/// Where the marketplace lives: `<giverny config base>/claude-plugin`.
pub fn marketplace_dir(base: &Path) -> PathBuf {
    base.join(DIR_NAME)
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The `giverny-pass` wrapper: runs `<the binary that wrote it> pass`.
fn wrapper(exes: &[String]) -> String {
    let list: Vec<String> = exes.iter().map(|e| sh_quote(e)).collect();
    format!(
        "#!/bin/sh\n\
         # Written by Giverny {VERSION}; rewritten each time it starts with the\n\
         # agents pane on. Runs `giverny pass`: see `giverny pass --help`.\n\
         for g in {}; do\n  \
           if [ -x \"$g\" ]; then exec \"$g\" pass \"$@\"; fi\n\
         done\n\
         if command -v giverny >/dev/null 2>&1; then exec giverny pass \"$@\"; fi\n\
         echo \"giverny-pass: the Giverny that wrote $0 is gone\" >&2\n\
         exit 127\n",
        list.join(" ")
    )
}

/// `hooks/hooks.json`: on `SessionStart` (a new session, `/clear`, and after
/// a compaction, which is when the context is fresh), print the reply that
/// carries [`ORCHESTRATE_BY_DEFAULT`]. `cat` of a file Claude Code parses
/// itself, so nothing is escaped by a shell.
fn session_hooks() -> Value {
    json!({
        "description": "Giverny: orchestrate by default (claude.orchestrate_by_default)",
        "hooks": {
            "SessionStart": [{
                "matcher": "startup|clear|compact",
                "hooks": [{
                    "type": "command",
                    "command": "cat \"${CLAUDE_PLUGIN_ROOT}/hooks/orchestrate-by-default.json\""
                }]
            }]
        }
    })
}

/// The hook's stdout: the instruction as `additionalContext`.
fn session_reply() -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": ORCHESTRATE_BY_DEFAULT.trim_end()
        }
    })
}

/// Every file of the marketplace: (path under the dir, contents, executable).
/// `orchestrate` adds the `SessionStart` hook (`claude.orchestrate_by_default`).
pub fn files(exes: &[String], orchestrate: bool) -> Vec<(&'static str, String, bool)> {
    let marketplace = json!({
        "name": MARKETPLACE,
        "owner": { "name": "Giverny" },
        "description": "The Claude Code plugin that ships inside Giverny",
        "plugins": [{
            "name": PLUGIN,
            "source": "./plugins/giverny",
            "description": "Orchestrate subagents and show the pass in Giverny's agents pane",
            "version": VERSION
        }]
    });
    let plugin = json!({
        "name": PLUGIN,
        "version": VERSION,
        "description": "Orchestrate subagents and show the pass in Giverny's agents pane: \
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
            "plugins/giverny/skills/orchestrate/SKILL.md",
            SKILL.to_string(),
            false,
        ),
        (
            "plugins/giverny/commands/clear-done.md",
            CLEAR_DONE.to_string(),
            false,
        ),
        ("plugins/giverny/bin/giverny-pass", wrapper(exes), true),
    ];
    if orchestrate {
        out.push((
            "plugins/giverny/hooks/hooks.json",
            pretty(&session_hooks()),
            false,
        ));
        out.push((
            "plugins/giverny/hooks/orchestrate-by-default.json",
            pretty(&session_reply()),
            false,
        ));
    }
    out
}

/// The binary as the wrapper should name it: this one, and on Windows also
/// its path from inside WSL (a WSL account's Claude runs it through interop).
pub fn exe_candidates() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
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
/// whether anything changed.
pub fn sync(dir: &Path, exes: &[String], orchestrate: bool) -> std::io::Result<bool> {
    let want = files(exes, orchestrate);
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
        // helper scripts: only Claude Code and `giverny-pass`.
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
            "orchestrate-status",
        ] {
            assert!(
                !words.iter().any(|w| w == word),
                "the plugin's skill mentions {word:?}"
            );
        }
        assert!(SKILL.starts_with("---\nname: orchestrate\n"));
        assert!(SKILL.contains("giverny-pass plan"));
    }

    #[test]
    fn sync_writes_once_and_prunes_what_is_not_ours() {
        let d = scratch("sync").join(DIR_NAME);
        let exes = vec!["/opt/giverny/giverny".to_string()];
        assert!(sync(&d, &exes, false).unwrap());
        assert!(!sync(&d, &exes, false).unwrap(), "a second sync is a no-op");
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(d.join("plugins/giverny/.claude-plugin/plugin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["version"], VERSION);
        let wrapper = std::fs::read_to_string(d.join("plugins/giverny/bin/giverny-pass")).unwrap();
        assert!(wrapper.contains("'/opt/giverny/giverny'"), "{wrapper}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(d.join("plugins/giverny/bin/giverny-pass"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        // An old version's leftover skill goes; a moved binary rewrites the wrapper.
        std::fs::create_dir_all(d.join("plugins/giverny/skills/old")).unwrap();
        std::fs::write(d.join("plugins/giverny/skills/old/SKILL.md"), "x").unwrap();
        assert!(sync(&d, &["/elsewhere/giverny".into()], false).unwrap());
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
    fn orchestrate_by_default_adds_the_session_hook_and_off_prunes_it() {
        let d = scratch("orchestrate").join(DIR_NAME);
        let exes = vec!["/opt/giverny/giverny".to_string()];
        let hooks = d.join("plugins/giverny/hooks/hooks.json");
        let reply = d.join("plugins/giverny/hooks/orchestrate-by-default.json");
        assert!(sync(&d, &exes, false).unwrap());
        assert!(!hooks.exists() && !reply.exists(), "off, no hook at all");
        assert!(sync(&d, &exes, true).unwrap());
        assert!(!sync(&d, &exes, true).unwrap(), "a second sync is a no-op");

        let h: Value = serde_json::from_slice(&std::fs::read(&hooks).unwrap()).unwrap();
        let start = &h["hooks"]["SessionStart"][0];
        assert_eq!(start["matcher"], "startup|clear|compact");
        let cmd = start["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(start["hooks"][0]["type"], "command");
        assert!(cmd.contains("${CLAUDE_PLUGIN_ROOT}"), "{cmd}");

        // The hook's command, run as Claude Code runs it, prints the reply.
        #[cfg(unix)]
        {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .env("CLAUDE_PLUGIN_ROOT", d.join("plugins/giverny"))
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            let r: Value = serde_json::from_slice(&out.stdout).unwrap();
            let o = &r["hookSpecificOutput"];
            assert_eq!(o["hookEventName"], "SessionStart");
            let ctx = o["additionalContext"].as_str().unwrap();
            assert!(ctx.contains("/giverny:orchestrate"), "{ctx}");
            assert!(ctx.contains("giverny-pass plan"), "{ctx}");
        }

        assert!(sync(&d, &exes, false).unwrap());
        assert!(!hooks.exists() && !reply.exists(), "off prunes the hook");
        assert!(!d.join("plugins/giverny/hooks").exists());
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

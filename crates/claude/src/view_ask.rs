//! The plugin's way into a worker's view (giverny#525).
//!
//! Claude Code 2.1.323 stopped running `subagentStatusLine`, so the relay no
//! longer tags the rows of Claude Code's agent list with agent ids, and a busy
//! worker's row there carries a model-written summary, never its description:
//! the keyboard walk down that list (`agent_open::Walk` in the app) cannot
//! tell two busy workers apart. Claude Code's mod API opens a view by agent
//! id, `$.ui.view({ agentId })`, but only while it answers the person's own
//! input: from a timer, a watched file or a `prompt.edit` it is refused as
//! "not person-initiated" (measured on 2.1.325).
//!
//! So the plugin carries a hooks module, `hooks/open.tsx` ([`MODULE`]). The
//! app writes an ask into the tab's file here ([`write_ask`]); the module,
//! reading that file every [`POLL_MS`], draws a Button labelled
//! [`label`] in the hint line under the prompt; the app finds it on the
//! tab's screen ([`find_label`]) and clicks it with a mouse report. The press
//! is the person's input as far as Claude Code can tell, so `$.ui.view` moves
//! the view, and the module writes back how that went ([`read_reply`]).
//! Nothing is typed: the prompt, its draft and its cursor stay as they were.
//!
//! The module is loaded when a claude starts (or on `/reload-plugins`), like
//! the plugin's `hooks.json`; a claude started before it was written draws no
//! Button, and the app falls back to the walk.

use std::path::{Path, PathBuf};

/// The asks' directory's name, under Giverny's `state/`.
pub const DIR_NAME: &str = "claude-view";
/// How often the module reads its tab's ask file.
pub const POLL_MS: u64 = 150;
/// How old an ask may be and still be drawn: one an app left behind when it
/// went away is taken down after this.
pub const ASK_TTL_MS: u64 = 5000;
/// The module's source; [`module_source`] fills in the directory.
pub const MODULE: &str = include_str!("../plugin/hooks/open.tsx");
/// Where the module's source says where the asks are.
const DIR_SLOT: &str = "__GIVERNY_VIEW_DIR__";

/// The asks' directory under Giverny's config base: `<base>/state/claude-view`.
pub fn dir(base: &Path) -> PathBuf {
    base.join("state").join(DIR_NAME)
}

/// The module as the plugin carries it: its source with `dir` written in.
pub fn module_source(dir: &Path) -> String {
    let quoted =
        serde_json::to_string(&dir.display().to_string()).unwrap_or_else(|_| "\"\"".into());
    MODULE.replace(DIR_SLOT, &quoted)
}

/// Tab `tab_id`'s ask (`$GIVERNY_TAB_ID`, `giverny-<n>`).
pub fn ask_path(dir: &Path, tab_id: &str) -> PathBuf {
    dir.join(format!("{tab_id}.json"))
}

/// The module's answer to tab `tab_id`'s last ask.
pub fn reply_path(dir: &Path, tab_id: &str) -> PathBuf {
    dir.join(format!("{tab_id}.reply.json"))
}

/// The Button's text on screen for ask `nonce`.
pub fn label(nonce: &str) -> String {
    format!("giverny-open:{nonce}")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Ask tab `tab_id`'s module for worker `agent`'s view (`None`: the main
/// conversation's). Replaces any ask before it, and the answer to that.
pub fn write_ask(
    dir: &Path,
    tab_id: &str,
    nonce: &str,
    agent: Option<&str>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let _ = std::fs::remove_file(reply_path(dir, tab_id));
    let body = serde_json::json!({ "nonce": nonce, "agent": agent, "at": now_ms() }).to_string();
    let path = ask_path(dir, tab_id);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)
}

/// Take tab `tab_id`'s ask back, and its answer: the module takes its Button
/// down at its next read.
pub fn withdraw(dir: &Path, tab_id: &str) {
    let _ = std::fs::remove_file(ask_path(dir, tab_id));
    let _ = std::fs::remove_file(reply_path(dir, tab_id));
}

/// What the module answered ask `nonce`: `Ok(())` once Claude Code shows the
/// view asked for, `Err(why)` when it refused. `None` while there is no
/// answer to that ask.
pub fn read_reply(dir: &Path, tab_id: &str, nonce: &str) -> Option<Result<(), String>> {
    let bytes = std::fs::read(reply_path(dir, tab_id)).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    if v.get("nonce").and_then(|n| n.as_str()) != Some(nonce) {
        return None;
    }
    Some(match v.get("deny").and_then(|d| d.as_str()) {
        Some(why) => Err(why.to_string()),
        None => Ok(()),
    })
}

/// Where `label` sits on `screen` (its text, a row per line, a cell per
/// char): the row, and the column of its middle. The last match, as the hint
/// line is under everything else.
pub fn find_label(screen: &str, label: &str) -> Option<(usize, usize)> {
    screen
        .lines()
        .enumerate()
        .filter_map(|(row, line)| {
            let at = line.find(label)?;
            let col = line[..at].chars().count() + label.chars().count() / 2;
            Some((row, col))
        })
        .last()
}

/// A new ask's nonce: short, and not the last one's.
pub fn nonce() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNT: AtomicU32 = AtomicU32::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    format!("{:x}{:02x}", now_ms() & 0xff_ffff, n & 0xff)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("giverny-view-ask-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn the_module_is_told_where_the_asks_are() {
        let src = module_source(Path::new("/home/a \"b\"/state/claude-view"));
        assert!(!src.contains(DIR_SLOT));
        assert!(src.contains(r#"const DIR = "/home/a \"b\"/state/claude-view""#));
        // What the app finds the Button by is what the module draws.
        assert!(src.contains("label={`giverny-open:${ask.nonce}`}"));
        assert_eq!(label("ab12"), "giverny-open:ab12");
        assert!(src.contains(&format!("const POLL_MS = {POLL_MS}")));
        assert!(src.contains(&format!("const ASK_TTL_MS = {ASK_TTL_MS}")));
    }

    #[test]
    fn an_ask_is_answered_only_by_its_own_reply() {
        let dir = scratch("reply");
        write_ask(&dir, "giverny-3", "n1", Some("a95d7f")).unwrap();
        let ask: serde_json::Value =
            serde_json::from_slice(&std::fs::read(ask_path(&dir, "giverny-3")).unwrap()).unwrap();
        assert_eq!(ask["nonce"], "n1");
        assert_eq!(ask["agent"], "a95d7f");
        assert!(ask["at"].as_u64().unwrap() > 0);
        assert_eq!(read_reply(&dir, "giverny-3", "n1"), None);

        std::fs::write(
            reply_path(&dir, "giverny-3"),
            r#"{"nonce":"n0","deny":null}"#,
        )
        .unwrap();
        assert_eq!(
            read_reply(&dir, "giverny-3", "n1"),
            None,
            "an older ask's answer"
        );
        std::fs::write(
            reply_path(&dir, "giverny-3"),
            r#"{"nonce":"n1","deny":null}"#,
        )
        .unwrap();
        assert_eq!(read_reply(&dir, "giverny-3", "n1"), Some(Ok(())));
        std::fs::write(
            reply_path(&dir, "giverny-3"),
            r#"{"nonce":"n1","deny":"no agent under that id"}"#,
        )
        .unwrap();
        assert_eq!(
            read_reply(&dir, "giverny-3", "n1"),
            Some(Err("no agent under that id".into()))
        );

        // A new ask clears the last answer; withdrawing clears both.
        write_ask(&dir, "giverny-3", "n2", None).unwrap();
        assert!(!reply_path(&dir, "giverny-3").exists());
        let ask: serde_json::Value =
            serde_json::from_slice(&std::fs::read(ask_path(&dir, "giverny-3")).unwrap()).unwrap();
        assert!(ask["agent"].is_null(), "home: the main conversation");
        withdraw(&dir, "giverny-3");
        assert!(!ask_path(&dir, "giverny-3").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_the_button_in_the_hint_line() {
        let screen = "❯ giverny-open:ab12 in a draft is the prompt's\n\
                      ────────\n\
                      ❯ hello\n\
                      ────────\n  ⏵⏵ auto mode on · giverny-open:ab12\n";
        let l = label("ab12");
        // The last one, a cell per char: `  ⏵⏵ auto mode on · ` is 20.
        assert_eq!(find_label(screen, &l), Some((4, 20 + l.len() / 2)));
        assert_eq!(find_label(screen, &label("zz")), None);
    }

    #[test]
    fn nonces_differ() {
        assert_ne!(nonce(), nonce());
    }
}

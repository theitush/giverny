//! `giverny pass nudge`: the plugin's `PostToolUse` hook, which asks a
//! worker for a fresh estimate once it has been on its task a few minutes
//! (giverny#143).
//!
//! A first estimate is made before anyone has read the code. Five minutes in,
//! the worker has, so its own re-estimate is the better figure — and the
//! history learns more from it. The hook runs on every tool call of every
//! session the plugin is loaded in, so it does nothing at all unless the call
//! is a subagent's (Claude Code puts `agent_id` in the payload only then):
//! no file is read for the dispatcher's own calls.
//!
//! For a subagent's call it finds the dispatcher's feed (the payload's
//! `session_id` is the dispatcher's), and the Running row this worker holds:
//! the row's `agent_id`, else a row whose key the worker's spawn description
//! names as a whole word (`agent-<id>.meta.json` beside the transcript, the
//! join the pane makes). Once that row has run [`AFTER_MS`] of working time
//! without a re-estimate, the hook's reply puts the request in the worker's
//! context, once: the row is stamped `reestimate_asked`.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::feed;
use crate::pass::{self, Lock};

/// How long into its task a worker is asked to re-estimate.
pub const AFTER_MS: u64 = 5 * 60 * 1000;

/// What the hook payload says about who is calling.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Caller {
    pub session: String,
    pub agent_id: String,
    /// The dispatcher's transcript, whose `<stem>/subagents/` holds the
    /// workers' spawn metadata.
    pub transcript: Option<PathBuf>,
}

impl Caller {
    /// `None` for a call that is not a subagent's.
    pub fn of(payload: &Value) -> Option<Caller> {
        let s = |k: &str| {
            payload
                .get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(String::from)
        };
        Some(Caller {
            agent_id: s("agent_id")?,
            session: s("session_id")?,
            transcript: s("transcript_path").map(PathBuf::from),
        })
    }

    /// The worker's spawn description, from its `agent-<id>.meta.json`.
    fn description(&self) -> Option<String> {
        let t = self.transcript.as_ref()?;
        let dir = t.with_extension("").join("subagents");
        crate::subagents::read_meta(&dir, &self.agent_id).description
    }
}

/// The row this worker holds and whether it is due a nudge at `now`. On a
/// match the row is stamped and the request returned.
pub fn check(
    doc: &mut Value,
    agent_id: &str,
    description: Option<&str>,
    now: u64,
) -> Option<String> {
    let rows = doc.get_mut("rows")?.as_array_mut()?;
    let mine = |r: &Map<String, Value>| {
        let key = r.get("key").and_then(Value::as_str).unwrap_or("");
        match r.get("agent_id").and_then(Value::as_str) {
            Some(a) => a == agent_id,
            None => description.is_some_and(|d| feed::names_key(d, key)),
        }
    };
    let row = rows
        .iter_mut()
        .filter_map(Value::as_object_mut)
        .filter(|r| pass::stage_of(r) == Some(feed::Stage::Running))
        .find(|r| mine(r))?;
    // Asked once; a worker that has re-estimated already needs no asking.
    if row.contains_key("reestimate_asked") || row.contains_key("eta_first_s") {
        return None;
    }
    let started = pass::ms_of(row, "started")?;
    let upto = pass::ms_of(row, "paused_since").unwrap_or(now);
    let worked = upto.saturating_sub(started);
    if worked < AFTER_MS {
        return None;
    }
    row.insert("reestimate_asked".into(), json!(pass::stamp(now)));
    let key = row.get("key").and_then(Value::as_str).unwrap_or("?");
    let span = |s: u64| feed::fmt_span(s as i64);
    let estimate = match pass::u64_of(row, "eta_s") {
        Some(eta) => format!(
            "Its estimate was {}, so the pane shows about {} left.",
            span(eta),
            span(eta.saturating_sub(worked / 1000))
        ),
        None => "It has no estimate yet.".to_string(),
    };
    Some(format!(
        "Giverny: you have been on task `{key}` for {}. {estimate} Now that you have read \
         the code, re-estimate it once: run `giverny-pass eta {key} <minutes left> --note \
         \"<why>\"`, even if the figure stands. Then carry on.",
        span(worked / 1000)
    ))
}

/// The hook's whole run: `payload` is its stdin, `dir` the feed directory.
/// Returns what to print (the hook reply), or nothing.
pub fn run(payload: &Value, dir: &Path, now: u64) -> Option<String> {
    let caller = Caller::of(payload)?;
    let file = pass::file_for(dir, &caller.session);
    if !file.is_file() {
        return None;
    }
    let description = caller.description();
    let _lock = Lock::take(&file).ok()?;
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&file).ok()?).ok()?;
    if pass::writer_of(&doc) != Some(pass::WRITER) {
        return None;
    }
    let ask = check(&mut doc, &caller.agent_id, description.as_deref(), now)?;
    pass::write(&file, &doc).ok()?;
    Some(reply(&ask))
}

/// The `PostToolUse` reply carrying `text` into the caller's context.
pub fn reply(text: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": text
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_790_000_000_000;
    const MIN: u64 = 60_000;

    fn doc(rows: Value) -> Value {
        json!({"version": 1, "session": "s1", "writer": pass::WRITER, "rows": rows})
    }

    #[test]
    fn a_worker_is_asked_once_five_minutes_in() {
        let mut d = doc(json!([
            {"key": "auth-fix", "stage": "running", "started": pass::stamp(T0), "eta_s": 1800},
            {"key": "docs", "stage": "running", "started": pass::stamp(T0)}
        ]));
        let desc = Some("auth-fix: fix the token race");
        assert_eq!(check(&mut d, "w1", desc, T0 + 4 * MIN), None, "too early");
        let ask = check(&mut d, "w1", desc, T0 + 5 * MIN).unwrap();
        assert!(ask.contains("giverny-pass eta auth-fix"), "{ask}");
        assert!(ask.contains("30m") && ask.contains("25m left"), "{ask}");
        assert!(d["rows"][0].get("reestimate_asked").is_some());
        assert_eq!(check(&mut d, "w1", desc, T0 + 9 * MIN), None, "only once");
        // Another worker, on a row with no estimate.
        let ask = check(&mut d, "w2", Some("docs"), T0 + 6 * MIN).unwrap();
        assert!(ask.contains("no estimate"), "{ask}");
    }

    #[test]
    fn no_ask_for_a_re_estimated_paused_or_unknown_row() {
        let started = pass::stamp(T0);
        let mut d = doc(json!([
            {"key": "a", "stage": "running", "started": started, "eta_s": 600, "eta_first_s": 300},
            {"key": "b", "stage": "running", "started": started,
             "paused_since": pass::stamp(T0 + 2 * MIN)},
            {"key": "c", "stage": "planned", "eta_s": 600},
            {"key": "d", "stage": "running", "started": started, "agent_id": "other"}
        ]));
        assert_eq!(check(&mut d, "w", Some("a"), T0 + 10 * MIN), None);
        assert_eq!(
            check(&mut d, "w", Some("b"), T0 + 10 * MIN),
            None,
            "two minutes worked"
        );
        assert_eq!(check(&mut d, "w", Some("c"), T0 + 10 * MIN), None);
        assert_eq!(
            check(&mut d, "w", Some("d"), T0 + 10 * MIN),
            None,
            "d is another agent's"
        );
        assert!(check(&mut d, "other", None, T0 + 10 * MIN).is_some());
    }

    #[test]
    fn the_dispatchers_own_calls_read_nothing_and_the_hook_round_trips() {
        let dir = std::env::temp_dir().join(format!("giverny-nudge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // No agent_id: not a worker, and no file is even looked for.
        assert_eq!(run(&json!({"session_id": "s1"}), &dir, T0), None);
        assert_eq!(
            Caller::of(&json!({"session_id": "s1", "agent_id": ""})),
            None
        );

        // A transcript dir with the worker's spawn metadata.
        let t = dir.join("proj/s1.jsonl");
        std::fs::create_dir_all(dir.join("proj/s1/subagents")).unwrap();
        std::fs::write(
            dir.join("proj/s1/subagents/agent-abc.meta.json"),
            r#"{"description":"giverny#143: better estimates","agentType":"general-purpose"}"#,
        )
        .unwrap();
        let feeds = dir.join("feeds");
        std::fs::create_dir_all(&feeds).unwrap();
        let f = feed::feed_path(&feeds, "s1");
        let d = doc(json!([{"key": "giverny#143", "stage": "running",
                            "started": pass::stamp(T0), "eta_s": 4500}]));
        pass::write(&f, &d).unwrap();
        let payload = json!({"session_id": "s1", "agent_id": "abc",
                             "transcript_path": t.display().to_string(),
                             "hook_event_name": "PostToolUse"});
        assert_eq!(run(&payload, &feeds, T0 + MIN), None);
        let out = run(&payload, &feeds, T0 + 6 * MIN).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        let ctx = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains("giverny-pass eta giverny#143"), "{ctx}");
        assert_eq!(run(&payload, &feeds, T0 + 7 * MIN), None, "asked once");
        let back = feed::read(&f).unwrap();
        assert_eq!(back.rows.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

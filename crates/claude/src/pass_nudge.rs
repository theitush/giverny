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
//!
//! A subagent that holds no row at all (spawned outside a pass, or any
//! subagent at all: an Explore search, a one-off helper) would sit on the pane
//! as `no ETA` (giverny#140). On its first call the hook asks it, once, for a
//! first estimate (giverny#158): `giverny-pass eta <task> <min> --agent <id>`,
//! which starts its row with that estimate (#144), after which the five-minute
//! re-estimate above applies to it like any other. That it was asked is a
//! marker file under the feed directory's `eta-asked/`, not a feed: creating
//! the session's feed would claim it from another writer (coo#162).

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

/// Whether `row` is this worker's: its `agent_id`, else a key the worker's
/// spawn description names as a whole word.
fn is_mine(row: &Map<String, Value>, agent_id: &str, description: Option<&str>) -> bool {
    let key = row.get("key").and_then(Value::as_str).unwrap_or("");
    match row.get("agent_id").and_then(Value::as_str) {
        Some(a) => a == agent_id,
        None => description.is_some_and(|d| feed::names_key(d, key)),
    }
}

/// Whether this worker holds a row of any stage: one that landed is not
/// asked for a first estimate either.
fn holds_row(doc: &Value, agent_id: &str, description: Option<&str>) -> bool {
    doc.get("rows")
        .and_then(Value::as_array)
        .is_some_and(|rows| {
            rows.iter()
                .filter_map(Value::as_object)
                .any(|r| is_mine(r, agent_id, description))
        })
}

/// The task name a worker with no row is asked to report under: its spawn
/// description's first word when that reads as a task id (`inbar#613 …`,
/// `auth-fix: …`) and no other row has it, else `agent-<its id>`, which the
/// `--agent` join needs no description for.
pub fn first_key(description: Option<&str>, agent_id: &str, doc: Option<&Value>) -> String {
    let taken = |k: &str| {
        doc.and_then(|d| d.get("rows"))
            .and_then(Value::as_array)
            .is_some_and(|rows| {
                rows.iter()
                    .any(|r| r.get("key").and_then(Value::as_str) == Some(k))
            })
    };
    let word = description
        .and_then(|d| d.split_whitespace().next())
        .filter(|w| w.contains('#') || w.ends_with(':'))
        .map(|w| w.trim_end_matches([':', ',', ';', '.']))
        .filter(|w| !w.is_empty() && !w.contains(['"', '`', '\\', '$']));
    match word {
        Some(w) if !taken(w) => w.to_string(),
        _ => {
            let short: String = agent_id
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(8)
                .collect();
            format!("agent-{short}")
        }
    }
}

/// The request to a worker with no row: report a first estimate.
pub fn first_ask(key: &str, agent_id: &str, description: Option<&str>) -> String {
    let title = description
        .map(|d| d.replace(['"', '`', '\\', '$'], "'"))
        .filter(|d| !d.trim().is_empty())
        .map(|d| format!(" --title \"{}\"", d.trim()))
        .unwrap_or_default();
    format!(
        "Giverny: the agents pane shows you as a running worker with no ETA. Run \
         `giverny-pass eta {key} <minutes> --agent {agent_id}{title}` now, with your best \
         guess of how many minutes this will take you, then carry on. If the figure turns \
         out wrong, run the same `giverny-pass eta {key} <minutes left>` again."
    )
}

/// The marker that says this worker has had its first ask.
fn asked_marker(dir: &Path, agent_id: &str) -> Option<PathBuf> {
    let name: String = agent_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    (!name.is_empty()).then(|| dir.join(ASKED_DIR).join(name))
}

/// Where the first-ask markers live, under the feed directory.
pub const ASKED_DIR: &str = "eta-asked";

/// How long a first-ask marker is kept: a worker outlives no day of this.
const ASKED_KEEP: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Stamp the marker, once: `false` if it was there already (or cannot be
/// made, so the worker is not asked on every call). Old markers go now.
fn mark_asked(dir: &Path, agent_id: &str) -> bool {
    let Some(marker) = asked_marker(dir, agent_id) else {
        return false;
    };
    if marker.exists() {
        return false;
    }
    let parent = marker.parent().expect("joined");
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    if let Ok(entries) = std::fs::read_dir(parent) {
        for e in entries.flatten() {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > ASKED_KEEP);
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
        .is_ok()
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
    let row = rows
        .iter_mut()
        .filter_map(Value::as_object_mut)
        .filter(|r| pass::stage_of(r) == Some(feed::Stage::Running))
        .find(|r| is_mine(r, agent_id, description))?;
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
    let description = caller.description();
    let desc = description.as_deref();
    let file = pass::file_for(dir, &caller.session);
    let mut doc = None;
    if file.is_file() {
        let _lock = Lock::take(&file).ok()?;
        let mut d: Value = serde_json::from_slice(&std::fs::read(&file).ok()?).ok()?;
        if pass::writer_of(&d) != Some(pass::WRITER) {
            return None; // another writer's pass: its rows, its estimates
        }
        if holds_row(&d, &caller.agent_id, desc) {
            let ask = check(&mut d, &caller.agent_id, desc, now)?;
            pass::write(&file, &d).ok()?;
            return Some(reply(&ask));
        }
        doc = Some(d);
    }
    if !mark_asked(dir, &caller.agent_id) {
        return None;
    }
    let key = first_key(desc, &caller.agent_id, doc.as_ref());
    Some(reply(&first_ask(&key, &caller.agent_id, desc)))
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

    #[test]
    fn a_first_estimate_is_asked_under_a_task_word_or_the_agent() {
        let d = doc(json!([{"key": "inbar#614", "stage": "running", "agent_id": "x"}]));
        let k = |desc: Option<&str>| first_key(desc, "a1b2c3d4e5f6", Some(&d));
        assert_eq!(k(Some("inbar#613 market SD graph")), "inbar#613");
        assert_eq!(k(Some("auth-fix: fix the token race")), "auth-fix");
        assert_eq!(
            k(Some("Find the config loader")),
            "agent-a1b2c3d4",
            "not a task id"
        );
        assert_eq!(
            k(Some("inbar#614 again")),
            "agent-a1b2c3d4",
            "another row's key"
        );
        assert_eq!(k(None), "agent-a1b2c3d4");
        let ask = first_ask("inbar#613", "a1b2", Some("inbar#613 \"SD\" graph"));
        assert!(
            ask.contains("`giverny-pass eta inbar#613 <minutes> --agent a1b2 --title \"inbar#613 'SD' graph\"`"),
            "{ask}"
        );
        assert!(first_ask("agent-a1", "a1", None).contains("--agent a1` now"));
    }

    #[test]
    fn a_worker_with_no_row_is_asked_once_for_a_first_estimate() {
        let dir = std::env::temp_dir().join(format!("giverny-nudge-first-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let feeds = dir.join("feeds");
        // No pass in this session at all: asked on the first call, once, and
        // no feed is made.
        let p = json!({"session_id": "s1", "agent_id": "w1", "hook_event_name": "PostToolUse"});
        let out = run(&p, &feeds, T0).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let ctx = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(
            ctx.contains("giverny-pass eta agent-w1 <minutes> --agent w1"),
            "{ctx}"
        );
        assert_eq!(run(&p, &feeds, T0 + MIN), None, "asked once");
        assert!(!feed::feed_path(&feeds, "s1").exists(), "no feed claimed");

        // A pass whose rows are other workers': this one is asked too.
        let f = feed::feed_path(&feeds, "s2");
        pass::write(
            &f,
            &doc(json!([{"key": "a", "stage": "running",
                                     "started": pass::stamp(T0), "agent_id": "w9"}])),
        )
        .unwrap();
        let p2 = json!({"session_id": "s2", "agent_id": "w2"});
        assert!(run(&p2, &feeds, T0).is_some());
        // The pass's own worker is not: its row is its estimate.
        let p9 = json!({"session_id": "s2", "agent_id": "w9"});
        assert_eq!(run(&p9, &feeds, T0 + MIN), None);

        // Another writer's feed: none of ours to ask about.
        let g = feed::feed_path(&feeds, "s3");
        std::fs::write(
            &g,
            r#"{"version":1,"session":"s3","writer":"coo/orchestrate-status","rows":[]}"#,
        )
        .unwrap();
        assert_eq!(
            run(&json!({"session_id": "s3", "agent_id": "w3"}), &feeds, T0),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

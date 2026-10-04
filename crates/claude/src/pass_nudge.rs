//! `giverny pass nudge`: the plugin's `PostToolUse` hook, which asks a
//! worker for a fresh estimate once it has been on its task a few minutes.
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
//! as `no ETA`. On its first call the hook asks it, once, for a
//! first estimate: `giverny-pass eta <task> <min> --agent <id>`,
//! which starts its row with that estimate, after which the five-minute
//! re-estimate above applies to it like any other. That it was asked is a
//! marker file under the feed directory's `eta-asked/`, not a feed: creating
//! the session's feed would claim it from another writer.
//!
//! **Only where something tracks the worker.** Both asks are for the pane:
//! they go out only when the session runs in a Giverny tab
//! (`$GIVERNY_TAB_ID`, whose pane shows the worker) or has a registered pass
//! (its feed is this writer's). Anywhere else — a plain `claude` in another
//! terminal — the hook says nothing to a subagent, which would otherwise try
//! a `giverny-pass eta` nobody reads and raise a permission prompt for it.
//!
//! **Messages and heartbeats**. On every call the hook also
//! renews the calling session's ledger leases — `session_id` is the
//! orchestrator's for its own calls and its workers' alike, so a busy pass
//! keeps its leases though it runs no `giverny pass` command — at most every
//! [`resources::BEAT_EVERY_MS`], timed by a marker under
//! `resources/beats/` so the calls in between cost a `stat`. And on the
//! orchestrator's own calls (not a worker's: an ask is for whoever holds the
//! lease) it delivers the session's unread `ask`/`reply` messages
//! ([`crate::pass_inbox`]), a `stat` of the inbox when there are none. So a
//! call that is not a worker's costs three `stat`s at most, and reads no file.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::pass::{self, Lock};
use crate::{feed, pass_history, pass_inbox, resources};

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
/// description's first word when that reads as a task id (`acme#613 …`,
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
    history: Option<&Path>,
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
    // How this kind of re-estimate has fared, so the figure itself improves:
    // read only now, once per worker.
    let s = |k: &str| row.get(k).and_then(Value::as_str);
    let kind = s("title").and_then(pass_history::kind_of);
    let record = history
        .map(pass_history::load)
        .and_then(|h| {
            pass_history::track_record(
                &h,
                pass_history::Track::Reestimate,
                s("repo"),
                kind.as_deref(),
            )
        })
        .map(|r| format!(" For calibration, {r}: weigh that in your figure."))
        .unwrap_or_default();
    Some(format!(
        "Giverny: you have been on task `{key}` for {}. {estimate} Now that you have read \
         the code, re-estimate it once: run `giverny-pass eta {key} <minutes left> --note \
         \"<why>\"`, even if the figure stands.{record} Then carry on.",
        span(worked / 1000)
    ))
}

/// The few fields of a hook payload the hook reads, as a small JSON object:
/// a `PostToolUse` payload carries the whole tool response, which is not
/// built into a tree only to be dropped.
pub fn payload_of(input: &str) -> Option<Value> {
    #[derive(serde::Deserialize)]
    struct Hook {
        session_id: Option<String>,
        agent_id: Option<String>,
        transcript_path: Option<String>,
        hook_event_name: Option<String>,
    }
    let h: Hook = serde_json::from_str(input).ok()?;
    let mut m = Map::new();
    for (k, v) in [
        ("session_id", h.session_id),
        ("agent_id", h.agent_id),
        ("transcript_path", h.transcript_path),
        ("hook_event_name", h.hook_event_name),
    ] {
        if let Some(v) = v {
            m.insert(k.into(), Value::String(v));
        }
    }
    Some(Value::Object(m))
}

/// Where the hook's heartbeat markers live, beside the ledger.
pub const BEATS_DIR: &str = "beats";

/// Renew `session`'s leases, at most every [`resources::BEAT_EVERY_MS`]:
/// nothing at all without a ledger (one `stat`); between beats, a `stat` of
/// the session's marker, whose mtime is the last beat. True when it beat.
pub fn beat(ledger: &Path, session: &str, now: u64) -> bool {
    if std::fs::metadata(ledger).is_err() {
        return false;
    }
    let name: String = session
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if name.is_empty() {
        return false;
    }
    let dir = ledger.parent().unwrap_or(Path::new(".")).join(BEATS_DIR);
    let marker = dir.join(&name);
    let ms_of = |t: std::time::SystemTime| {
        t.duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    };
    let last = std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .map(ms_of)
        .ok();
    if last.is_some_and(|t| now.saturating_sub(t) < resources::BEAT_EVERY_MS && t <= now) {
        return false;
    }
    if last.is_none() {
        // A new session's first marker: sweep the ones a day old.
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let old = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|t| now.saturating_sub(ms_of(t)) > ASKED_KEEP.as_millis() as u64)
                    .unwrap_or(false);
                if old {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    let _ = resources::heartbeat(ledger, session, now);
    if let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&marker)
    {
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_millis(now);
        let _ = f.set_modified(at);
    }
    true
}

/// Whether this process runs in a Giverny tab: `$GIVERNY_TAB_ID`, which the
/// app sets in a tab's shell and a hook inherits.
pub fn in_giverny_tab() -> bool {
    std::env::var("GIVERNY_TAB_ID").is_ok_and(|t| !t.trim().is_empty())
}

/// The hook's whole run: `payload` is its stdin, `dir` the feed directory,
/// `in_tab` whether the session runs in a Giverny tab ([`in_giverny_tab`]).
/// Returns what to print (the hook reply), or nothing.
pub fn run(payload: &Value, dir: &Path, now: u64, in_tab: bool) -> Option<String> {
    let session = payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.contains(['/', '\\']) && !s.starts_with('.'))?;
    let ledger = resources::ledger_path(dir);
    beat(&ledger, session, now);
    let Some(caller) = Caller::of(payload) else {
        // The orchestrator's own call: its messages, if any.
        return pass_inbox::deliver(dir, &ledger, session, now).map(|t| reply(&t));
    };
    worker(&caller, dir, now, in_tab)
}

/// A worker's call: the estimate asks, when something tracks it — a Giverny
/// tab, or a registered pass (this writer's feed for the session).
fn worker(caller: &Caller, dir: &Path, now: u64, in_tab: bool) -> Option<String> {
    let file = pass::file_for(dir, &caller.session);
    let has_feed = file.is_file();
    if !in_tab && !has_feed {
        return None; // nothing would show this worker: ask it nothing
    }
    let description = caller.description();
    let desc = description.as_deref();
    let mut doc = None;
    if has_feed {
        let _lock = Lock::take(&file).ok()?;
        let mut d: Value = serde_json::from_slice(&std::fs::read(&file).ok()?).ok()?;
        if pass::writer_of(&d) != Some(pass::WRITER) {
            return None; // another writer's pass: its rows, its estimates
        }
        if holds_row(&d, &caller.agent_id, desc) {
            let ask = check(
                &mut d,
                &caller.agent_id,
                desc,
                now,
                pass_history::path(dir).as_deref(),
            )?;
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
        assert_eq!(
            check(&mut d, "w1", desc, T0 + 4 * MIN, None),
            None,
            "too early"
        );
        let ask = check(&mut d, "w1", desc, T0 + 5 * MIN, None).unwrap();
        assert!(ask.contains("giverny-pass eta auth-fix"), "{ask}");
        assert!(ask.contains("30m") && ask.contains("25m left"), "{ask}");
        assert!(d["rows"][0].get("reestimate_asked").is_some());
        assert_eq!(
            check(&mut d, "w1", desc, T0 + 9 * MIN, None),
            None,
            "only once"
        );
        // Another worker, on a row with no estimate.
        let ask = check(&mut d, "w2", Some("docs"), T0 + 6 * MIN, None).unwrap();
        assert!(ask.contains("no estimate"), "{ask}");
    }

    #[test]
    fn the_ask_shows_how_past_re_estimates_fared() {
        let dir = std::env::temp_dir().join(format!("giverny-nudge-rec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let h = dir.join(pass_history::FILE);
        for _ in 0..pass_history::MIN_SAMPLES {
            let rec = pass_history::Record {
                key: "g#1".into(),
                repo: Some("g".into()),
                kind: Some("BUG".into()),
                reest_s: Some(600),
                reest_at_s: Some(300),
                wall_s: 1500,
                work_s: 1500,
                ..pass_history::Record::default()
            };
            pass_history::append(&h, &rec).unwrap();
        }
        let mut d = doc(json!([{"key": "g#2", "stage": "running", "repo": "g",
            "title": "BUG: x", "started": pass::stamp(T0), "eta_s": 1800}]));
        let ask = check(&mut d, "w", Some("g#2"), T0 + 5 * MIN, Some(&h)).unwrap();
        assert!(
            ask.contains(
                "For calibration, your last 5 BUG re-estimates in g took ×2.00 of what was \
                 said (median): they run short: estimate higher"
            ),
            "{ask}"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(check(&mut d, "w", Some("a"), T0 + 10 * MIN, None), None);
        assert_eq!(
            check(&mut d, "w", Some("b"), T0 + 10 * MIN, None),
            None,
            "two minutes worked"
        );
        assert_eq!(check(&mut d, "w", Some("c"), T0 + 10 * MIN, None), None);
        assert_eq!(
            check(&mut d, "w", Some("d"), T0 + 10 * MIN, None),
            None,
            "d is another agent's"
        );
        assert!(check(&mut d, "other", None, T0 + 10 * MIN, None).is_some());
    }

    #[test]
    fn the_dispatchers_own_calls_read_nothing_and_the_hook_round_trips() {
        let dir = std::env::temp_dir().join(format!("giverny-nudge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // No agent_id: not a worker, and no file is even looked for.
        assert_eq!(run(&json!({"session_id": "s1"}), &dir, T0, false), None);
        assert_eq!(
            Caller::of(&json!({"session_id": "s1", "agent_id": ""})),
            None
        );

        // A transcript dir with the worker's spawn metadata.
        let t = dir.join("proj/s1.jsonl");
        std::fs::create_dir_all(dir.join("proj/s1/subagents")).unwrap();
        std::fs::write(
            dir.join("proj/s1/subagents/agent-abc.meta.json"),
            r#"{"description":"demo#143: better estimates","agentType":"general-purpose"}"#,
        )
        .unwrap();
        let feeds = dir.join("feeds");
        std::fs::create_dir_all(&feeds).unwrap();
        let f = feed::feed_path(&feeds, "s1");
        let d = doc(json!([{"key": "demo#143", "stage": "running",
                            "started": pass::stamp(T0), "eta_s": 4500}]));
        pass::write(&f, &d).unwrap();
        let payload = json!({"session_id": "s1", "agent_id": "abc",
                             "transcript_path": t.display().to_string(),
                             "hook_event_name": "PostToolUse"});
        assert_eq!(run(&payload, &feeds, T0 + MIN, false), None);
        let out = run(&payload, &feeds, T0 + 6 * MIN, false).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        let ctx = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains("giverny-pass eta demo#143"), "{ctx}");
        assert_eq!(
            run(&payload, &feeds, T0 + 7 * MIN, false),
            None,
            "asked once"
        );
        let back = feed::read(&f).unwrap();
        assert_eq!(back.rows.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_first_estimate_is_asked_under_a_task_word_or_the_agent() {
        let d = doc(json!([{"key": "acme#614", "stage": "running", "agent_id": "x"}]));
        let k = |desc: Option<&str>| first_key(desc, "a1b2c3d4e5f6", Some(&d));
        assert_eq!(k(Some("acme#613 market SD graph")), "acme#613");
        assert_eq!(k(Some("auth-fix: fix the token race")), "auth-fix");
        assert_eq!(
            k(Some("Find the config loader")),
            "agent-a1b2c3d4",
            "not a task id"
        );
        assert_eq!(
            k(Some("acme#614 again")),
            "agent-a1b2c3d4",
            "another row's key"
        );
        assert_eq!(k(None), "agent-a1b2c3d4");
        let ask = first_ask("acme#613", "a1b2", Some("acme#613 \"SD\" graph"));
        assert!(
            ask.contains(
                "`giverny-pass eta acme#613 <minutes> --agent a1b2 --title \"acme#613 'SD' graph\"`"
            ),
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
        let out = run(&p, &feeds, T0, true).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let ctx = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(
            ctx.contains("giverny-pass eta agent-w1 <minutes> --agent w1"),
            "{ctx}"
        );
        assert_eq!(run(&p, &feeds, T0 + MIN, true), None, "asked once");
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
        assert!(run(&p2, &feeds, T0, true).is_some());
        // The pass's own worker is not: its row is its estimate.
        let p9 = json!({"session_id": "s2", "agent_id": "w9"});
        assert_eq!(run(&p9, &feeds, T0 + MIN, true), None);

        // Another writer's feed: none of ours to ask about.
        let g = feed::feed_path(&feeds, "s3");
        std::fs::write(
            &g,
            r#"{"version":1,"session":"s3","writer":"other/status-writer","rows":[]}"#,
        )
        .unwrap();
        assert_eq!(
            run(
                &json!({"session_id": "s3", "agent_id": "w3"}),
                &feeds,
                T0,
                true
            ),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn outside_a_giverny_tab_only_a_pass_worker_is_asked() {
        let dir = std::env::temp_dir().join(format!("giverny-nudge-notab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let feeds = dir.join("feeds");
        // A plain `claude`, no pass: every call of every subagent is silent,
        // and leaves nothing behind.
        let p = json!({"session_id": "s1", "agent_id": "w1", "hook_event_name": "PostToolUse"});
        for k in 0..10 {
            assert_eq!(run(&p, &feeds, T0 + k * MIN, false), None);
        }
        assert!(!feeds.join(ASKED_DIR).exists(), "no marker made");
        assert!(!feed::feed_path(&feeds, "s1").exists(), "no feed made");
        // The same worker in a tab is asked.
        assert!(run(&p, &feeds, T0 + 11 * MIN, true).is_some());

        // A registered pass outside a tab: its row's worker gets the
        // re-estimate ask, a row-less one the first ask.
        let f = feed::feed_path(&feeds, "s2");
        pass::write(
            &f,
            &doc(json!([{"key": "a", "stage": "running",
                         "started": pass::stamp(T0), "agent_id": "w9"}])),
        )
        .unwrap();
        let p9 = json!({"session_id": "s2", "agent_id": "w9"});
        assert_eq!(run(&p9, &feeds, T0 + MIN, false), None, "too early");
        let ask = run(&p9, &feeds, T0 + 6 * MIN, false).unwrap();
        assert!(ask.contains("giverny-pass eta a <minutes left>"), "{ask}");
        let p2 = json!({"session_id": "s2", "agent_id": "w2"});
        let ask = run(&p2, &feeds, T0, false).unwrap();
        assert!(ask.contains("--agent w2"), "{ask}");

        // Another writer's feed is no pass of ours: still silent.
        let g = feed::feed_path(&feeds, "s3");
        std::fs::write(
            &g,
            r#"{"version":1,"session":"s3","writer":"other/status-writer","rows":[]}"#,
        )
        .unwrap();
        assert_eq!(
            run(
                &json!({"session_id": "s3", "agent_id": "w3"}),
                &feeds,
                T0,
                false
            ),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

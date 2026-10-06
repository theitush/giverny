//! An orchestrator session's part of the plugin's hook
//! ([`crate::plugin_hook`]): asking a worker that holds one of its tasks for
//! a fresh estimate, and keeping the session's leases alive.
//!
//! A first estimate is made before anyone has read the code. Five minutes in,
//! the worker has, so its own re-estimate is the better figure — and the
//! history learns more from it. The hook finds the Running row this worker
//! holds in the session's file: the row's `agent_id`, else a row whose key the
//! worker's spawn description names as a whole word (`agent-<id>.meta.json`
//! beside the transcript, the join the pane makes). Once that row has run
//! [`AFTER_MS`] of working time without a re-estimate, the hook's reply puts
//! the request in the worker's context, once: the row is stamped
//! `reestimate_asked`. A worker that holds no task is not this module's: its
//! estimate is an agent ETA ([`crate::agent_eta`]).
//!
//! **Heartbeats**. On every call the hook also renews the calling session's
//! ledger leases — `session_id` is the orchestrator's for its own calls and
//! its workers' alike, so a busy orchestrator session keeps its leases though
//! it runs no `giverny orchestrator-session` command — at most every
//! [`resources::BEAT_EVERY_MS`], timed by a marker under `resources/beats/` so
//! the calls in between cost a `stat`.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::orchestrator_session;
use crate::{feed, orchestrator_session_history, resources};

/// How long into its task a worker is asked to re-estimate.
pub const AFTER_MS: u64 = 5 * 60 * 1000;

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
pub(crate) fn holds_row(doc: &Value, agent_id: &str, description: Option<&str>) -> bool {
    doc.get("rows")
        .and_then(Value::as_array)
        .is_some_and(|rows| {
            rows.iter()
                .filter_map(Value::as_object)
                .any(|r| is_mine(r, agent_id, description))
        })
}

/// Put the worker's id on the Running rows its spawn description names that
/// carry none yet: a dispatcher that ran `start` before the spawn could not
/// give it, and a later `start <next> --agent <id>` finds the worker's
/// earlier task by it. True when a row changed.
pub fn stamp_agent(doc: &mut Value, agent_id: &str, description: Option<&str>) -> bool {
    let Some(rows) = doc.get_mut("rows").and_then(Value::as_array_mut) else {
        return false;
    };
    let mut changed = false;
    for row in rows.iter_mut().filter_map(Value::as_object_mut) {
        if !row.contains_key("agent_id")
            && orchestrator_session::stage_of(row) == Some(feed::Stage::Running)
            && is_mine(row, agent_id, description)
        {
            row.insert("agent_id".into(), json!(agent_id));
            changed = true;
        }
    }
    changed
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
        .filter(|r| orchestrator_session::stage_of(r) == Some(feed::Stage::Running))
        .find(|r| is_mine(r, agent_id, description))?;
    // Asked once; a worker that has re-estimated already needs no asking.
    if row.contains_key("reestimate_asked") || row.contains_key("eta_first_s") {
        return None;
    }
    let started = orchestrator_session::ms_of(row, "started")?;
    let upto = orchestrator_session::ms_of(row, "paused_since").unwrap_or(now);
    let worked = upto.saturating_sub(started);
    if worked < AFTER_MS {
        return None;
    }
    row.insert(
        "reestimate_asked".into(),
        json!(orchestrator_session::stamp(now)),
    );
    let key = row.get("key").and_then(Value::as_str).unwrap_or("?");
    let span = |s: u64| feed::fmt_span(s as i64);
    let estimate = match orchestrator_session::u64_of(row, "eta_s") {
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
    let kind = s("title").and_then(orchestrator_session_history::kind_of);
    let record = history
        .map(orchestrator_session_history::load)
        .and_then(|h| {
            orchestrator_session_history::track_record(
                &h,
                orchestrator_session_history::Track::Reestimate,
                s("repo"),
                kind.as_deref(),
            )
        })
        .map(|r| format!(" For calibration, {r}: weigh that in your figure."))
        .unwrap_or_default();
    Some(format!(
        "Giverny: you have been on task `{key}` for {}. {estimate} Now that you have read \
         the code, re-estimate it once: run `giverny-orchestrator-session eta {key} <minutes left> --note \
         \"<why>\"`, even if the figure stands.{record} Then carry on.",
        span(worked / 1000)
    ))
}

/// Where the hook's heartbeat markers live, beside the ledger.
pub const BEATS_DIR: &str = "beats";

/// How long a beat marker is kept after its session last beat.
const BEATS_KEEP_MS: u64 = 24 * 60 * 60 * 1000;

/// Renew `session`'s leases, at most every [`resources::BEAT_EVERY_MS`]:
/// nothing at all without a ledger (one `stat`); between beats, a `stat` of
/// the session's marker, whose mtime is the last beat. True when it beat.
/// With the feed dir, the beat also drops leases of tasks that have landed.
pub fn beat(ledger: &Path, feed_dir: Option<&Path>, session: &str, now: u64) -> bool {
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
                    .map(|t| now.saturating_sub(ms_of(t)) > BEATS_KEEP_MS)
                    .unwrap_or(false);
                if old {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    let _ = resources::heartbeat(ledger, feed_dir, session, now);
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

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_790_000_000_000;
    const MIN: u64 = 60_000;

    fn doc(rows: Value) -> Value {
        json!({"version": 1, "session": "s1", "writer": orchestrator_session::WRITER, "rows": rows})
    }

    #[test]
    fn a_worker_is_asked_once_five_minutes_in() {
        let mut d = doc(json!([
            {"key": "auth-fix", "stage": "running", "started": orchestrator_session::stamp(T0), "eta_s": 1800},
            {"key": "docs", "stage": "running", "started": orchestrator_session::stamp(T0)}
        ]));
        let desc = Some("auth-fix: fix the token race");
        assert_eq!(
            check(&mut d, "w1", desc, T0 + 4 * MIN, None),
            None,
            "too early"
        );
        let ask = check(&mut d, "w1", desc, T0 + 5 * MIN, None).unwrap();
        assert!(
            ask.contains("giverny-orchestrator-session eta auth-fix"),
            "{ask}"
        );
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
        let h = dir.join(orchestrator_session_history::FILE);
        for _ in 0..orchestrator_session_history::MIN_SAMPLES {
            let rec = orchestrator_session_history::Record {
                key: "g#1".into(),
                repo: Some("g".into()),
                kind: Some("BUG".into()),
                reest_s: Some(600),
                reest_at_s: Some(300),
                wall_s: 1500,
                work_s: 1500,
                ..orchestrator_session_history::Record::default()
            };
            orchestrator_session_history::append(&h, &rec).unwrap();
        }
        let mut d = doc(json!([{"key": "g#2", "stage": "running", "repo": "g",
            "title": "BUG: x", "started": orchestrator_session::stamp(T0), "eta_s": 1800}]));
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
        let started = orchestrator_session::stamp(T0);
        let mut d = doc(json!([
            {"key": "a", "stage": "running", "started": started, "eta_s": 600, "eta_first_s": 300},
            {"key": "b", "stage": "running", "started": started,
             "paused_since": orchestrator_session::stamp(T0 + 2 * MIN)},
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
}

//! A manager session's part of the plugin's hook
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
//! **The ask carries how far off the gut runs** (giverny#371, #469): "your
//! gut re-estimates on BUG tasks in giverny have usually run about 3.0× too
//! high (last 20, median); give your gut figure and the one you settle on:
//! `giverny-manage eta <key> <settled> --gut <gut> …`". Advice, not an
//! order: how much to allow for it is the worker's judgement, and the gut
//! figure it gives is what the history learns from. Only an ask carries it,
//! never a note between asks. A worker that runs `eta` before any ask, or
//! with no `--gut`, is asked by the `eta` reply itself (giverny#470,
//! [`crate::manage`]): the figure is kept as its gut figure, the line
//! told, and the figure it settles on with `--gut` soon after takes that
//! one's place; the five-minute ask then skips the row. The factor told is
//! stamped on the row (`track_x`), and the first `eta` keeps it beside the
//! figure (`reest_told_x`).
//!
//! **Near the end.** Each figure is asked about again as it runs out: once
//! when [`DEADLINE_LEFT_MS`] of it are left (only a figure that had more than
//! that when it was given — one given that short is asked about only past
//! it), and once if the work runs past it. The row is stamped
//! `deadline_asked` / `overdue_asked` with the `eta_s` the ask was for, so a
//! figure is asked about once each way and a fresh one starts over. No ask
//! within [`QUIET_MS`] of the worker's last re-estimate (`eta_at`) or of the
//! five-minute ask, nor on a paused row. Every ask tells the worker how far
//! off its gut on its kind's re-estimates runs; none changes its figure.
//!
//! **Heartbeats**. On every call the hook also renews the calling session's
//! ledger leases — `session_id` is the manager's for its own calls and
//! its workers' alike, so a busy manager session keeps its leases though
//! it runs no `giverny manage` command — at most every
//! [`resources::BEAT_EVERY_MS`], timed by a marker under `resources/beats/` so
//! the calls in between cost a `stat`.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::manage;
use crate::{feed, manage_history, resources};

/// How long into its task a worker is asked to re-estimate.
pub const AFTER_MS: u64 = 5 * 60 * 1000;

/// How much of an estimate is left when the worker is asked about it again.
pub const DEADLINE_LEFT_MS: u64 = 5 * 60 * 1000;

/// No near-the-end ask this soon after a re-estimate or another ask.
pub const QUIET_MS: u64 = 3 * 60 * 1000;

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

/// The key of the row this worker holds: its Running one, else any
/// ([`holds_row`]). What its background shells are recorded against
/// ([`crate::bg_shells`], giverny#472).
pub(crate) fn row_key(doc: &Value, agent_id: &str, description: Option<&str>) -> Option<String> {
    let rows: Vec<&Map<String, Value>> = doc
        .get("rows")?
        .as_array()?
        .iter()
        .filter_map(Value::as_object)
        .filter(|r| is_mine(r, agent_id, description))
        .collect();
    let row = rows
        .iter()
        .find(|r| manage::stage_of(r) == Some(feed::Stage::Running))
        .or(rows.first())?;
    row.get("key").and_then(Value::as_str).map(String::from)
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
            && manage::stage_of(row) == Some(feed::Stage::Running)
            && is_mine(row, agent_id, description)
        {
            row.insert("agent_id".into(), json!(agent_id));
            changed = true;
        }
    }
    changed
}

/// The Running row this worker holds: the one its asks are about.
fn running_row<'a>(
    doc: &'a mut Value,
    agent_id: &str,
    description: Option<&str>,
) -> Option<&'a mut Map<String, Value>> {
    doc.get_mut("rows")?
        .as_array_mut()?
        .iter_mut()
        .filter_map(Value::as_object_mut)
        .filter(|r| manage::stage_of(r) == Some(feed::Stage::Running))
        .find(|r| is_mine(r, agent_id, description))
}

/// On a worker's call: leave its manager's progress check pending when the
/// Running row it holds has run out of its figure (giverny#473,
/// [`crate::manage_progress::overdue`]). True when the row changed.
pub fn overdue(doc: &mut Value, agent_id: &str, description: Option<&str>, now: u64) -> bool {
    running_row(doc, agent_id, description)
        .is_some_and(|row| crate::manage_progress::overdue(row, now))
}

/// The row this worker holds and whether it is due an ask at `now`: the
/// five-minute one, else one near or past the end of its estimate
/// ([`deadline`]). On a match the row is stamped and the request returned.
/// Nothing is said between asks (giverny#469).
pub fn check(
    doc: &mut Value,
    agent_id: &str,
    description: Option<&str>,
    now: u64,
    history: Option<&Path>,
) -> Option<String> {
    let row = running_row(doc, agent_id, description)?;
    ask(row, now, history)
}

/// The re-estimate ask due on this worker's Running `row` at `now`, if any:
/// the five-minute one, else one near or past the end of its estimate.
/// Stamps the row when it asks.
fn ask(row: &mut Map<String, Value>, now: u64, history: Option<&Path>) -> Option<String> {
    if row.contains_key("paused_since") {
        return None;
    }
    let started = manage::ms_of(row, "started")?;
    let worked = now.saturating_sub(started);
    // Asked once; a worker that has re-estimated already needs no asking:
    // its `eta` reply was its ask (giverny#470).
    if !row.contains_key("reestimate_asked")
        && !row.contains_key("eta_first_s")
        && !row.contains_key("reest_s")
    {
        if worked < AFTER_MS {
            return None;
        }
        row.insert("reestimate_asked".into(), json!(manage::stamp(now)));
        let key = row
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let estimate = match manage::u64_of(row, "eta_s") {
            Some(eta) => format!(
                "Its estimate was {}, so the pane shows about {} left.",
                span(eta),
                span(eta.saturating_sub(worked / 1000))
            ),
            None => "It has no estimate yet.".to_string(),
        };
        return Some(format!(
            "Giverny: you have been on task `{key}` for {}. {estimate} Now that you have read \
             the code, re-estimate it once, even if the figure stands: {}. Then carry on.",
            span(worked / 1000),
            how(row, &key, history)
        ));
    }
    deadline(row, started, now, history)
}

/// `s` seconds as the pane writes them.
fn span(s: u64) -> String {
    feed::fmt_span(s as i64)
}

/// How to give the figure an ask asks for: how far off the gut on this
/// kind of re-estimate runs, and the command with both figures
/// (giverny#469); with too little history, the command alone. The figure is
/// the worker's to settle on, never scaled for it. Stamps the row with the
/// factor told ([`tell`]).
fn how(row: &mut Map<String, Value>, key: &str, history: Option<&Path>) -> String {
    match tell(row, history) {
        Some(line) => format!(
            "{}. {} for the minutes left: `giverny-manage eta {key} <settled> --gut <gut> \
             --note \"<why>\"`",
            capitalised(&line),
            capitalised(manage_history::GIVE_BOTH),
        ),
        None => format!("run `giverny-manage eta {key} <minutes left> --note \"<why>\"`"),
    }
}

/// `s` with its first letter upper-case.
pub(crate) fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// The row's kind's re-estimate line ([`manage_history::Instinct::line`]),
/// `None` with too little history. The factor told is stamped on the row
/// (`track_x`, the latest one told): the worker's first `eta` keeps it as
/// the factor it had been shown (giverny#371). The hook replies after the
/// call it rides on (`PostToolUse`), so a stamp is always one the worker
/// had read before any later `eta`.
fn tell(row: &mut Map<String, Value>, history: Option<&Path>) -> Option<String> {
    let s = |k: &str| row.get(k).and_then(Value::as_str);
    let kind = s("title").and_then(manage_history::kind_of);
    let told = history.map(manage_history::load).and_then(|h| {
        manage_history::instinct(
            &h,
            manage_history::Track::Reestimate,
            s("repo"),
            kind.as_deref(),
        )
    })?;
    row.insert("track_x".into(), json!(told.x));
    Some(told.line())
}

/// The near-the-end ask on a Running row that is not paused: see the
/// module's doc. Stamps the row when it asks.
fn deadline(
    row: &mut Map<String, Value>,
    started: u64,
    now: u64,
    history: Option<&Path>,
) -> Option<String> {
    let eta = manage::u64_of(row, "eta_s").filter(|e| *e > 0)?;
    let recent = |k: &str| manage::ms_of(row, k).is_some_and(|t| now.saturating_sub(t) < QUIET_MS);
    if recent("eta_at") || recent("reestimate_asked") {
        return None;
    }
    let worked = now.saturating_sub(started);
    let left = (eta * 1000) as i64 - worked as i64;
    // What was left when this figure was given: at `eta_at`, else the start.
    let given_at = manage::ms_of(row, "eta_at").unwrap_or(started).max(started);
    let given_left = (eta * 1000).saturating_sub(given_at - started);
    let asked = |k: &str| manage::u64_of(row, k) == Some(eta);
    let (stamp, said) = if left < 0 {
        if asked("overdue_asked") {
            return None;
        }
        (
            "overdue_asked",
            format!(
                "has run {} past its estimate",
                span(left.unsigned_abs() / 1000)
            ),
        )
    } else if left as u64 <= DEADLINE_LEFT_MS && given_left > DEADLINE_LEFT_MS {
        if asked("deadline_asked") {
            return None;
        }
        (
            "deadline_asked",
            format!(
                "has about {} left of its estimate",
                span(left as u64 / 1000)
            ),
        )
    } else {
        return None;
    };
    row.insert(stamp.into(), json!(eta));
    let key = row
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    Some(format!(
        "Giverny: task `{key}` {said} ({} in all, {} so far). Re-estimate it once, even if \
         the figure stands: {}. Then carry on.",
        span(eta),
        span(worked / 1000),
        how(row, &key, history)
    ))
}

/// How long a task has been paused, with a `run` command of its still
/// going, before its manager hears of it (giverny#471).
pub const PAUSED_RUN_MS: u64 = 10 * 60 * 1000;

/// On the dispatcher's own call: each paused task of `session` whose `run`
/// commands still go [`PAUSED_RUN_MS`] into its pause is told once per
/// pause (the row stamped `pause_runs_told` with the pause's start), with
/// what runs and how to stop it. A suggestion: Giverny stops nothing, the
/// manager decides (giverny#471). With no live run of the session's, the
/// feed is not read: a read of the small runs directory is all it costs.
pub fn paused_runs(dir: &Path, session: &str, now: u64) -> Option<String> {
    let runs_dir = crate::run_live::runs_dir(&resources::ledger_path(dir));
    let live = crate::run_live::session_runs(&runs_dir, session);
    if live.is_empty() {
        return None;
    }
    let file = manage::file_for(dir, session);
    let _lock = manage::Lock::take(&file).ok()?;
    let mut doc = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(|d| manage::writer_of(d) == Some(manage::WRITER))?;
    let rows = doc.get_mut("rows").and_then(Value::as_array_mut)?;
    let mut lines = Vec::new();
    for row in rows.iter_mut().filter_map(Value::as_object_mut) {
        let Some(since) = manage::ms_of(row, "paused_since") else {
            continue;
        };
        let told = manage::stamp(since);
        if manage::stage_of(row) != Some(feed::Stage::Running)
            || now.saturating_sub(since) < PAUSED_RUN_MS
            || row.get("pause_runs_told").and_then(Value::as_str) == Some(told.as_str())
        {
            continue;
        }
        let key = row
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let runs: Vec<_> = live
            .iter()
            .filter(|(t, _)| *t == key)
            .map(|(_, r)| r.clone())
            .collect();
        if runs.is_empty() {
            continue;
        }
        row.insert("pause_runs_told".into(), json!(told));
        let (what, it) = if runs.len() == 1 {
            (
                "a `giverny-manage run` command of it still runs".to_string(),
                "it",
            )
        } else {
            let n = runs.len();
            (
                format!("{n} `giverny-manage run` commands of it still run"),
                "them",
            )
        };
        lines.push(format!(
            "`{key}` has been paused {}, but {what} (Giverny stops nothing): {}. Stop {it} if \
             the pause is meant to stop the work, or let {it} run on if needed.",
            span(now.saturating_sub(since) / 1000),
            manage::run_lines(&runs, now).join("; ")
        ));
    }
    if lines.is_empty() {
        return None;
    }
    manage::write(&file, &doc).ok()?;
    Some(lines.join("\n\n"))
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
    let _ = resources::heartbeat(ledger, feed_dir, session, now, None);
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
        json!({"version": 1, "session": "s1", "writer": manage::WRITER, "rows": rows})
    }

    #[test]
    fn a_worker_is_asked_once_five_minutes_in() {
        let mut d = doc(json!([
            {"key": "auth-fix", "stage": "running", "started": manage::stamp(T0), "eta_s": 1800},
            {"key": "docs", "stage": "running", "started": manage::stamp(T0)}
        ]));
        let desc = Some("auth-fix: fix the token race");
        assert_eq!(
            check(&mut d, "w1", desc, T0 + 4 * MIN, None),
            None,
            "too early"
        );
        let ask = check(&mut d, "w1", desc, T0 + 5 * MIN, None).unwrap();
        assert!(ask.contains("giverny-manage eta auth-fix"), "{ask}");
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

    /// A history file under `name` with [`manage_history::MIN_SAMPLES`] BUG
    /// re-estimates in repo `g` that each took twice the figure.
    fn bug_history(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("giverny-nudge-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let h = dir.join(manage_history::FILE);
        for _ in 0..manage_history::MIN_SAMPLES {
            let rec = manage_history::Record {
                key: "g#1".into(),
                repo: Some("g".into()),
                kind: Some("BUG".into()),
                reest_s: Some(600),
                reest_at_s: Some(300),
                wall_s: 1500,
                work_s: 1500,
                ..manage_history::Record::default()
            };
            manage_history::append(&h, &rec).unwrap();
        }
        h
    }

    #[test]
    fn the_ask_shows_how_far_off_the_gut_runs() {
        let h = bug_history("rec");
        let dir = h.parent().unwrap().to_path_buf();
        let mut d = doc(json!([{"key": "g#2", "stage": "running", "repo": "g",
            "title": "BUG: x", "started": manage::stamp(T0), "eta_s": 1800}]));
        let ask = check(&mut d, "w", Some("g#2"), T0 + 5 * MIN, Some(&h)).unwrap();
        assert_eq!(
            ask,
            "Giverny: you have been on task `g#2` for 5m. Its estimate was 30m, so the pane \
             shows about 25m left. Now that you have read the code, re-estimate it once, even \
             if the figure stands: Your gut re-estimates on BUG tasks in g have usually run \
             about 2.0× too low (last 5, median). Give your gut figure and the one you settle \
             on for the minutes left: `giverny-manage eta g#2 <settled> --gut <gut> --note \
             \"<why>\"`. Then carry on."
        );
        // The factor told is on the row, for the worker's first `eta`.
        assert_eq!(d["rows"][0]["track_x"], 2.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// giverny#469: how far off the gut runs reaches a worker only in an
    /// ask, never as a note between asks (giverny#366's first-call note is
    /// gone).
    #[test]
    fn nothing_is_said_before_the_ask() {
        let h = bug_history("told");
        let mut d = doc(json!([{"key": "g#2", "stage": "running", "repo": "g",
            "title": "BUG: x", "started": manage::stamp(T0), "eta_s": 1800, "agent_id": "w"}]));
        assert_eq!(check(&mut d, "w", None, T0 + 10_000, Some(&h)), None);
        assert_eq!(check(&mut d, "w", None, T0 + MIN, Some(&h)), None);
        assert!(d["rows"][0].get("track_x").is_none(), "no factor told yet");
        let ask = check(&mut d, "w", None, T0 + 5 * MIN, Some(&h)).unwrap();
        assert!(ask.contains("about 2.0× too low (last 5, median)"), "{ask}");
        // Too little history: the ask has the plain command.
        let mut d = doc(json!([{"key": "z#1", "stage": "running", "repo": "z",
            "title": "BUG: z", "started": manage::stamp(T0), "agent_id": "w"}]));
        let few = h.with_file_name("none.jsonl");
        let ask = check(&mut d, "w", None, T0 + 5 * MIN, Some(&few)).unwrap();
        assert!(
            ask.contains("run `giverny-manage eta z#1 <minutes left> --note"),
            "{ask}"
        );
        assert!(!ask.contains("--gut"), "{ask}");
        assert!(d["rows"][0].get("track_x").is_none(), "no factor told");
        let _ = std::fs::remove_dir_all(h.parent().unwrap());
    }

    #[test]
    fn each_figure_is_asked_about_near_its_end_and_past_it() {
        let mut d = doc(json!([{"key": "g#7", "stage": "running",
            "started": manage::stamp(T0), "eta_s": 1800,
            "reestimate_asked": manage::stamp(T0 + 5 * MIN)}]));
        let mut ask = |at: u64| check(&mut d, "w", Some("g#7"), T0 + at, None);
        assert_eq!(ask(24 * MIN), None, "6m left");
        let a = ask(25 * MIN).unwrap();
        assert!(
            a.contains("task `g#7` has about 5m left of its estimate (30m in all, 25m so far)"),
            "{a}"
        );
        assert!(a.contains("giverny-manage eta g#7 <minutes left>"));
        assert_eq!(ask(26 * MIN), None, "asked once");
        let a = ask(31 * MIN).unwrap();
        assert!(a.contains("has run 1m past its estimate"), "{a}");
        assert_eq!(ask(33 * MIN), None, "once");
        assert_eq!(d["rows"][0]["deadline_asked"], 1800);
        assert_eq!(d["rows"][0]["overdue_asked"], 1800);

        // A fresh figure at 33m: 15m more. Quiet a while, then its own asks.
        d["rows"][0]["eta_s"] = json!(48 * 60);
        d["rows"][0]["eta_at"] = json!(manage::stamp(T0 + 33 * MIN));
        let mut ask = |at: u64| check(&mut d, "w", Some("g#7"), T0 + at, None);
        assert_eq!(ask(34 * MIN), None);
        assert_eq!(ask(42 * MIN), None, "6m left");
        assert!(ask(43 * MIN).unwrap().contains("about 5m left"));
        assert!(ask(49 * MIN).unwrap().contains("1m past"));
        assert_eq!(ask(50 * MIN), None);
        assert_eq!(d["rows"][0]["deadline_asked"], 48 * 60);
    }

    #[test]
    fn a_short_or_fresh_figure_is_let_be_until_it_runs_out() {
        let t = manage::stamp;
        // 4m left given ten minutes in: never "near", only past it.
        let mut d = doc(json!([{"key": "s", "stage": "running", "started": t(T0),
            "eta_s": 14 * 60, "eta_first_s": 14 * 60, "eta_at": t(T0 + 10 * MIN)}]));
        let mut ask = |at: u64| check(&mut d, "w", Some("s"), T0 + at, None);
        assert_eq!(ask(11 * MIN), None, "just given");
        assert_eq!(ask(13 * MIN), None, "short figure, not near");
        assert!(ask(15 * MIN).unwrap().contains("1m past"));
        // A 9m start figure: the five-minute ask, then quiet, then near.
        let mut d = doc(json!([{"key": "q", "stage": "running", "started": t(T0),
            "eta_s": 9 * 60}]));
        let mut ask = |at: u64| check(&mut d, "w", Some("q"), T0 + at, None);
        assert!(ask(5 * MIN).unwrap().contains("Now that you have read"));
        assert_eq!(ask(7 * MIN), None, "2m left, but just asked");
        assert!(ask(8 * MIN).unwrap().contains("about 1m left"));
        // A paused row is asked nothing.
        let mut d = doc(json!([{"key": "p", "stage": "running", "started": t(T0),
            "eta_s": 600, "eta_first_s": 600, "paused_since": t(T0 + 9 * MIN)}]));
        assert_eq!(check(&mut d, "w", Some("p"), T0 + 20 * MIN, None), None);
    }

    #[test]
    fn no_ask_for_a_re_estimated_paused_or_unknown_row() {
        let started = manage::stamp(T0);
        let mut d = doc(json!([
            {"key": "a", "stage": "running", "started": started, "eta_s": 3600, "eta_first_s": 300},
            {"key": "b", "stage": "running", "started": started,
             "paused_since": manage::stamp(T0 + 2 * MIN)},
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

    /// A task paused ten minutes with its `run` still going is told to its
    /// manager once per pause, with the command that stops it; nothing is
    /// stopped, and a row with no live run, or paused less, is not told
    /// (giverny#471).
    #[test]
    fn a_paused_tasks_live_run_is_told_once_per_pause() {
        let dir = std::env::temp_dir().join(format!("giverny-nudge-paused-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = manage::file_for(&dir, "s1");
        let paused = manage::stamp(T0);
        let write = |rows: Value| manage::write(&file, &doc(rows)).unwrap();
        write(json!([
            {"key": "a", "stage": "running", "started": manage::stamp(T0 - MIN), "paused_since": paused},
            {"key": "b", "stage": "running", "started": manage::stamp(T0 - MIN), "paused_since": paused},
            {"key": "c", "stage": "running", "started": manage::stamp(T0 - MIN)}
        ]));
        assert_eq!(paused_runs(&dir, "s1", T0 + 20 * MIN), None, "nothing runs");
        let runs = crate::run_live::runs_dir(&resources::ledger_path(&dir));
        std::fs::create_dir_all(&runs).unwrap();
        let cmd = ["sleep".to_string(), "300".to_string()];
        let _a = crate::run_live::register_bound(
            &runs.join("1-0.stats"),
            "a",
            "s1",
            T0 - MIN,
            None,
            &cmd,
        )
        .unwrap();
        let _c = crate::run_live::register_bound(
            &runs.join("1-1.stats"),
            "c",
            "s1",
            T0 - MIN,
            None,
            &cmd,
        )
        .unwrap();
        assert_eq!(paused_runs(&dir, "s1", T0 + 9 * MIN), None, "too soon");
        let me = std::process::id();
        let told = paused_runs(&dir, "s1", T0 + 10 * MIN).expect("told");
        assert_eq!(
            told,
            format!(
                "`a` has been paused 10m, but a `giverny-manage run` command of it still runs \
                 (Giverny stops nothing): `sleep 300`, 11m in, pid {me}: `kill {me}` stops it \
                 and all it started. Stop it if the pause is meant to stop the work, or let it \
                 run on if needed."
            )
        );
        assert!(crate::run_live::alive(me));
        assert_eq!(paused_runs(&dir, "s1", T0 + 30 * MIN), None, "once");
        // A new pause is told again.
        write(json!([
            {"key": "a", "stage": "running", "started": manage::stamp(T0), "paused_since": manage::stamp(T0 + 40 * MIN)}
        ]));
        assert_eq!(paused_runs(&dir, "s1", T0 + 45 * MIN), None);
        assert!(paused_runs(&dir, "s1", T0 + 50 * MIN).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

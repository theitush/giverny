//! The progress check (giverny#473): a task's worker that runs out of its
//! figure, or asks for more time, is put before its manager for a look.
//!
//! **Giverny suggests, managers decide.** The check goes into the manager's
//! context on its next own tool call, once per event: what the worker asked
//! for or how far past its figure it is, its note, what it has done so far
//! (working time, what it is doing now, its `run` commands and background
//! shells, its transcript), and the ask — does the work make sense, is it all
//! needed, could it be done faster? Giverny stops, pauses and re-estimates
//! nothing; the manager reads the worker's work and answers it, or lets it
//! go on.
//!
//! **Two triggers, each once.**
//! - **More time.** A worker's `eta` whose finish (now + the minutes left)
//!   is later than the finish its figure before implied by at least
//!   [`MORE_MIN_MS`] and at least [`MORE_FRAC`] of that figure
//!   ([`more_time`]): a small slip does not nag. The figure before is the
//!   latest one given (`left_s`, else the whole `eta_s`), counted down by the
//!   work since as the pane counts it. The row keeps the ask pending
//!   (`progress_more`: the finish before, the finish asked for, how many
//!   asks); a later `eta` before it is told folds in, against the same
//!   finish before, and one that takes it back under the threshold drops it.
//! - **Overdue.** The worker's figure has run out: its row's clock (the same
//!   wall clock the worker's own overdue ask counts by) is past `eta_s`. Seen
//!   on the worker's own hook calls, which read its row anyway; the row keeps
//!   the figure it is for (`progress_overdue`), and once told
//!   (`overdue_told`) the same figure is not told again.
//!
//! **One notice per [`QUIET_MS`] per task.** An event that comes sooner after
//! the task's last notice (`progress_told`) waits, pending, and is told when
//! the quiet is up, folded with any that came meanwhile. One that no longer
//! stands by then — the figure extended since, the row paused or landed — is
//! dropped, or kept for a paused row until it runs again.
//!
//! **The manager's own `eta`** on a task is its decision, not an ask: the
//! dispatcher's own Bash call running `eta <task>` drops that task's pending
//! more-time ask ([`own_eta`]).
//!
//! **Cheap.** Whoever leaves an event pending touches
//! `<feed dir>/progress/<session>`, so the manager's calls cost a `stat` when
//! none is pending; the marker goes once nothing is.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::manage::{self, Lock};
use crate::{feed, sleeps};

/// A more-time ask that moves the finish less than this is not told.
pub const MORE_MIN_MS: u64 = 10 * 60 * 1000;

/// Nor one that moves it less than this share of the figure before.
pub const MORE_FRAC: f64 = 0.25;

/// At most one progress check per task in this span.
pub const QUIET_MS: u64 = 10 * 60 * 1000;

/// Where the pending markers live, under the feed directory.
pub const DIR: &str = "progress";

/// The fields a pending event keeps on its row.
const MORE: &str = "progress_more";
const OVERDUE: &str = "progress_overdue";

/// `<feed dir>/progress/<session>`: there while an event is pending.
pub fn marker(feed_dir: &Path, session: &str) -> PathBuf {
    feed_dir.join(DIR).join(session)
}

fn safe(session: &str) -> bool {
    !session.is_empty()
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Whether a finish `by_s` later than the one before passes the threshold,
/// for a figure before of `figure_s`.
pub fn passes(by_s: i64, figure_s: u64) -> bool {
    by_s >= (MORE_MIN_MS / 1000) as i64 && by_s as f64 >= MORE_FRAC * figure_s as f64
}

/// A worker's `eta` on its Running `row`, after the new figure is written:
/// `before` is what the figure before still had left at `now` and that
/// figure itself, in seconds ([`manage::figure_left`]); `left` the new
/// figure. Leaves a more-time ask pending on the row when it passes the
/// threshold ([`passes`]), folds it into one already pending, and drops a
/// pending one this figure takes back under it. True when one is pending.
pub fn more_time(
    row: &mut Map<String, Value>,
    before: Option<(i64, u64)>,
    left: u64,
    now: u64,
) -> bool {
    let to = now + left * 1000;
    let pending = row.get(MORE).and_then(Value::as_object).cloned();
    let (from, figure, asks) = match &pending {
        Some(p) => (
            p.get("from").and_then(Value::as_u64),
            p.get("figure_s").and_then(Value::as_u64),
            p.get("asks").and_then(Value::as_u64).unwrap_or(1) + 1,
        ),
        None => match before {
            Some((rem, fig)) => (Some((now as i64 + rem * 1000).max(0) as u64), Some(fig), 1),
            None => (None, None, 1),
        },
    };
    let (Some(from), Some(figure)) = (from, figure) else {
        return false;
    };
    let by_s = (to as i64 - from as i64) / 1000;
    if !passes(by_s, figure) {
        row.remove(MORE);
        return false;
    }
    row.insert(
        MORE.into(),
        json!({"from": from, "to": to, "figure_s": figure, "asks": asks, "at": now}),
    );
    true
}

/// On a worker's own hook call, its Running `row`: leave the overdue event
/// pending when its figure has run out and has not been told. True when it
/// did (the row changed).
pub fn overdue(row: &mut Map<String, Value>, now: u64) -> bool {
    if row.contains_key("paused_since") {
        return false;
    }
    let Some(eta) = manage::u64_of(row, "eta_s").filter(|e| *e > 0) else {
        return false;
    };
    let Some(started) = manage::ms_of(row, "started") else {
        return false;
    };
    if now <= started + eta * 1000
        || manage::u64_of(row, "overdue_told") == Some(eta)
        || manage::u64_of(row, OVERDUE) == Some(eta)
    {
        return false;
    }
    row.insert(OVERDUE.into(), json!(eta));
    true
}

/// Whether any row of `doc` has an event pending.
pub fn any_pending(doc: &Value) -> bool {
    doc.get("rows")
        .and_then(Value::as_array)
        .is_some_and(|rows| {
            rows.iter()
                .filter_map(Value::as_object)
                .any(|r| r.contains_key(MORE) || r.contains_key(OVERDUE))
        })
}

/// Touch `session`'s marker when `doc` has an event pending.
pub fn mark(feed_dir: &Path, session: &str, doc: &Value) {
    if !safe(session) || !any_pending(doc) {
        return;
    }
    let m = marker(feed_dir, session);
    let _ = std::fs::create_dir_all(m.parent().unwrap_or(feed_dir));
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&m);
}

/// The task a Bash command of the dispatcher's own re-estimates, if it
/// runs `giverny-manage eta <task>` (or `giverny manage eta <task>`). The
/// hook's payload carries it as `own_eta` ([`crate::plugin_hook::payload_of`]).
pub fn own_eta(cmd: &str) -> Option<String> {
    let words: Vec<&str> = cmd.split_whitespace().collect();
    words.windows(3).find_map(|w| {
        let manage = w[0].ends_with("giverny-manage") || w[0] == "manage";
        (manage && w[1] == "eta").then(|| {
            w[2].trim_matches(|c| c == '"' || c == '\'' || c == ';')
                .to_string()
        })
    })
}

/// What the notice says of a task's worker, read only when one is told.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Work {
    /// Its working time so far, seconds.
    pub worked_s: u64,
    /// What it is doing now: its last tool call.
    pub doing: Option<String>,
    /// When it last wrote.
    pub last_ms: Option<u64>,
    /// Its `run` commands still going: command and start.
    pub runs: Vec<(String, Option<u64>)>,
    /// Its background shells still going: command, start, shell id.
    pub shells: Vec<(String, u64, String)>,
    /// Its transcript, when there is one.
    pub transcript: Option<PathBuf>,
}

/// How the notice reads a task's worker: the system, or a test's.
pub trait Look {
    fn work(&self, row: &Map<String, Value>, key: &str, now: u64) -> Work;
}

/// The real look: the row's clocks, the worker's transcript under the
/// dispatcher's (`transcript`), the session's live runs and shells.
pub struct System<'a> {
    pub feed_dir: &'a Path,
    pub session: &'a str,
    pub transcript: Option<&'a Path>,
}

impl Look for System<'_> {
    fn work(&self, row: &Map<String, Value>, key: &str, now: u64) -> Work {
        let sleeps = sleeps::load(self.feed_dir);
        let agent = row.get("agent_id").and_then(Value::as_str);
        let transcript = self.transcript.zip(agent).map(|(t, a)| {
            crate::subagents::agent_transcript(&t.with_extension("").join("subagents"), a)
        });
        let transcript = transcript.filter(|t| t.is_file());
        let act = transcript
            .as_deref()
            .map(crate::subagents::read_activity)
            .unwrap_or_default();
        let runs_dir = crate::run_live::runs_dir(&crate::resources::ledger_path(self.feed_dir));
        let runs = crate::run_live::task_runs(&runs_dir, self.session, key)
            .into_iter()
            .map(|r| (r.cmd.unwrap_or_default(), r.started_ms))
            .collect();
        let shells = crate::bg_shells::task_shells(
            self.feed_dir,
            self.session,
            key,
            &crate::bg_shells::System,
        )
        .into_iter()
        .map(|l| (l.shell.cmd, l.shell.started_ms, l.shell.id))
        .collect();
        Work {
            worked_s: manage::worked_s(row, now, &sleeps),
            doing: act.doing,
            last_ms: act.last_turn_ms,
            runs,
            shells,
            transcript,
        }
    }
}

/// On the dispatcher's own call: each task's pending event told once, when
/// its quiet is up ([`QUIET_MS`]). A `stat` when none is pending.
pub fn deliver(
    feed_dir: &Path,
    session: &str,
    payload: &Value,
    now: u64,
    look: &dyn Look,
) -> Option<String> {
    if !safe(session) {
        return None;
    }
    let m = marker(feed_dir, session);
    std::fs::metadata(&m).ok()?;
    let file = manage::file_for(feed_dir, session);
    let _lock = Lock::take(&file).ok()?;
    let Some(mut doc) = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(|d| manage::writer_of(d) == Some(manage::WRITER))
    else {
        let _ = std::fs::remove_file(&m);
        return None;
    };
    let mine = payload.get("own_eta").and_then(Value::as_str);
    let mut lines = Vec::new();
    let mut changed = false;
    for row in doc
        .get_mut("rows")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object_mut)
    {
        if !row.contains_key(MORE) && !row.contains_key(OVERDUE) {
            continue;
        }
        let key = row
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        if manage::stage_of(row) != Some(feed::Stage::Running) {
            changed |= row.remove(MORE).is_some() | row.remove(OVERDUE).is_some();
            continue;
        }
        if mine == Some(key.as_str()) && row.remove(MORE).is_some() {
            changed = true; // the manager's own re-estimate: its decision
        }
        // A figure extended since is not overdue any more.
        let late = late_s(row, now);
        if row.contains_key(OVERDUE)
            && (late.is_none() || manage::u64_of(row, OVERDUE) != manage::u64_of(row, "eta_s"))
        {
            row.remove(OVERDUE);
            changed = true;
        }
        if !row.contains_key(MORE) && !row.contains_key(OVERDUE) {
            continue;
        }
        let quiet =
            manage::ms_of(row, "progress_told").is_some_and(|t| now.saturating_sub(t) < QUIET_MS);
        if row.contains_key("paused_since") || quiet {
            continue; // kept: told when it runs, or when the quiet is up
        }
        let work = look.work(row, &key, now);
        lines.push(notice(row, &key, late, &work, now));
        if let Some(eta) = row.remove(OVERDUE) {
            row.insert("overdue_told".into(), eta);
        }
        row.remove(MORE);
        row.insert("progress_told".into(), json!(manage::stamp(now)));
        changed = true;
    }
    if changed {
        manage::write(&file, &doc).ok()?;
    }
    if !any_pending(&doc) {
        let _ = std::fs::remove_file(&m);
    }
    (!lines.is_empty()).then(|| lines.join("\n\n"))
}

/// How far past its figure a Running row is, seconds, on the clock the
/// worker's own overdue ask counts by.
fn late_s(row: &Map<String, Value>, now: u64) -> Option<u64> {
    let eta = manage::u64_of(row, "eta_s").filter(|e| *e > 0)?;
    let end = manage::ms_of(row, "started")? + eta * 1000;
    (now > end).then(|| (now - end) / 1000)
}

fn span(s: u64) -> String {
    feed::fmt_span(s as i64)
}

/// `40m` or `12s` since `t`.
fn ago(t: u64, now: u64) -> String {
    match now.saturating_sub(t) / 1000 {
        s @ 0..60 => format!("{s}s"),
        s => span(s),
    }
}

/// The progress check for `key`: see the module's doc.
pub fn notice(
    row: &Map<String, Value>,
    key: &str,
    late: Option<u64>,
    work: &Work,
    now: u64,
) -> String {
    let clock = |ms: u64| crate::away::local_clock(ms, now);
    let mut why = Vec::new();
    if let Some(m) = row.get(MORE).and_then(Value::as_object) {
        let get = |k: &str| m.get(k).and_then(Value::as_u64).unwrap_or(0);
        let (from, to) = (get("from"), get("to"));
        let asks = match get("asks") {
            0 | 1 => String::new(),
            n => format!(" ({n} asks)"),
        };
        why.push(format!(
            "its worker asked for {} more{asks}: finish {} → {}",
            span(to.saturating_sub(from) / 1000),
            clock(from),
            clock(to)
        ));
    }
    if row.contains_key(OVERDUE)
        && let Some(late) = late
    {
        let eta = manage::u64_of(row, "eta_s").unwrap_or(0);
        why.push(format!(
            "its worker is overdue by {} (its figure, {} in all, ran out at {})",
            span(late),
            span(eta),
            clock(now.saturating_sub(late * 1000))
        ));
    }
    let mut out = format!("Giverny: progress check on `{key}`: {}.", why.join("; "));
    if let Some(note) = row
        .get("note")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
    {
        out.push_str(&format!(" Its note: \"{note}\"."));
    }
    let mut done = vec![format!("{} of work", span(work.worked_s))];
    let waiting = manage::ms_of(row, "waiting_since");
    match (waiting, &work.doing) {
        (Some(w), _) => done.push(format!("waiting since {}", clock(w))),
        (None, Some(d)) => done.push(format!("now `{}`", crate::strays::quote(d))),
        (None, None) => {}
    }
    if let Some(t) = work.last_ms {
        done.push(format!("last wrote {} ago", ago(t, now)));
    }
    if !work.runs.is_empty() {
        let runs: Vec<String> = work
            .runs
            .iter()
            .map(|(cmd, at)| {
                let ran = at
                    .map(|t| format!(" ({} in)", ago(t, now)))
                    .unwrap_or_default();
                format!("{}{ran}", manage::short_cmd(cmd))
            })
            .collect();
        done.push(format!("runs {}", runs.join(", ")));
    }
    if !work.shells.is_empty() {
        let shells: Vec<String> = work
            .shells
            .iter()
            .map(|(cmd, at, id)| {
                format!("{} ({}, shell {id})", manage::short_cmd(cmd), ago(*at, now))
            })
            .collect();
        done.push(format!("background shells {}", shells.join(", ")));
    }
    out.push_str(&format!("\nSo far: {}.", done.join("; ")));
    if let Some(t) = &work.transcript {
        out.push_str(&format!(" Its transcript: {}", t.display()));
    }
    let answer = match row.get("agent_id").and_then(Value::as_str) {
        Some(a) => {
            format!("Answer it (SendMessage `{a}`): tell it to cut, change course or carry on")
        }
        None => "Answer its worker: tell it to cut, change course or carry on".to_string(),
    };
    out.push_str(&format!(
        "\nLook at what it has done: does it make sense, is it all needed, could it be done \
         faster? {answer}; or let it go on. Giverny stops and re-estimates nothing."
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_790_000_000_000;
    const MIN: u64 = 60_000;

    struct Fake(Work);
    impl Look for Fake {
        fn work(&self, _: &Map<String, Value>, _: &str, _: u64) -> Work {
            self.0.clone()
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("giverny-progress-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn row(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn the_threshold_is_ten_minutes_and_a_quarter_of_the_figure() {
        assert!(!passes(9 * 60, 10 * 60), "under 10m");
        assert!(passes(10 * 60, 30 * 60), "10m of 30m");
        assert!(passes(10 * 60, 40 * 60), "10m is a quarter of 40m");
        assert!(!passes(14 * 60, 60 * 60), "14m of 1h is under a quarter");
        assert!(passes(15 * 60, 60 * 60));
        assert!(!passes(-20 * 60, 30 * 60), "sooner is no ask");
    }

    #[test]
    fn a_later_finish_past_the_threshold_is_pending_and_a_small_one_is_not() {
        let now = T0 + 20 * MIN;
        // 30m figure, 20m in: 10m left. 15m asked: 5m later, let be.
        let mut r = row(json!({"key": "g1"}));
        assert!(!more_time(&mut r, Some((600, 1800)), 15 * 60, now));
        assert!(!r.contains_key(MORE));
        // 45m asked: 35m later.
        assert!(more_time(&mut r, Some((600, 1800)), 45 * 60, now));
        let m = &r[MORE];
        assert_eq!(m["from"], now + 10 * MIN);
        assert_eq!(m["to"], now + 45 * MIN);
        assert_eq!(m["asks"], 1);
        // A second ask before it is told folds in, against the same finish.
        let later = now + 2 * MIN;
        assert!(more_time(
            &mut r,
            Some((45 * 60 - 120, 45 * 60)),
            60 * 60,
            later
        ));
        assert_eq!(r[MORE]["from"], now + 10 * MIN);
        assert_eq!(r[MORE]["to"], later + 60 * MIN);
        assert_eq!(r[MORE]["asks"], 2);
        // One that takes it back under drops it.
        assert!(!more_time(&mut r, Some((3480, 3600)), 10 * 60, later + MIN));
        assert!(!r.contains_key(MORE));
        // Past the figure already: 12m over, 5m asked is 17m later.
        let mut r = row(json!({"key": "g1"}));
        assert!(more_time(&mut r, Some((-12 * 60, 1800)), 5 * 60, now));
    }

    #[test]
    fn overdue_is_pending_once_per_figure() {
        let mut r = row(json!({"key": "g1", "stage": "running",
            "started": manage::stamp(T0), "eta_s": 1800}));
        assert!(!overdue(&mut r, T0 + 30 * MIN), "not yet");
        assert!(overdue(&mut r, T0 + 31 * MIN));
        assert!(!overdue(&mut r, T0 + 32 * MIN), "pending already");
        r.remove(OVERDUE);
        r.insert("overdue_told".into(), json!(1800));
        assert!(!overdue(&mut r, T0 + 40 * MIN), "told already");
        r.insert("eta_s".into(), json!(3000));
        assert!(overdue(&mut r, T0 + 51 * MIN), "a new figure runs out too");
        let mut p = row(json!({"started": manage::stamp(T0), "eta_s": 60,
            "paused_since": manage::stamp(T0)}));
        assert!(!overdue(&mut p, T0 + 60 * MIN), "paused");
    }

    #[test]
    fn the_dispatchers_own_eta_is_found() {
        assert_eq!(
            own_eta("giverny-manage eta g473 30 --note x").as_deref(),
            Some("g473")
        );
        assert_eq!(
            own_eta("~/.local/bin/giverny-manage eta \"g#7\" 5").as_deref(),
            Some("g#7")
        );
        assert_eq!(own_eta("giverny manage eta k 5").as_deref(), Some("k"));
        assert_eq!(own_eta("giverny-manage show"), None);
        // The hook's payload carries it, from the dispatcher's own Bash
        // call only.
        let raw = |agent: &str| {
            format!(
                r#"{{"session_id":"s1",{agent}"tool_name":"Bash","tool_input":{{"command":"giverny-manage eta g1 20"}},"tool_response":{{"stdout":"g1: ~20m left"}}}}"#
            )
        };
        let p = crate::plugin_hook::payload_of(&raw("")).unwrap();
        assert_eq!(p["own_eta"], "g1");
        let p = crate::plugin_hook::payload_of(&raw(r#""agent_id":"w","#)).unwrap();
        assert!(p.get("own_eta").is_none(), "a worker's eta is an ask");
    }

    fn work() -> Work {
        Work {
            worked_s: 38 * 60,
            doing: Some("Bash: cargo test -p giverny-claude".into()),
            last_ms: Some(T0 + 40 * MIN - MIN),
            runs: vec![("cargo test -p giverny-claude".into(), Some(T0 + 34 * MIN))],
            shells: vec![("sleep 600".into(), T0 + 37 * MIN, "bq1".into())],
            transcript: Some(PathBuf::from("/t/subagents/agent-a1.jsonl")),
        }
    }

    #[test]
    fn the_notice_says_what_was_asked_what_was_done_and_asks_to_judge() {
        let now = T0 + 40 * MIN;
        let c = |ms| crate::away::local_clock(ms, now);
        let r = row(json!({"key": "g473", "stage": "running", "agent_id": "a1",
            "note": "tests are slow", "started": manage::stamp(T0), "eta_s": 85 * 60,
            MORE: {"from": now - 5 * MIN, "to": now + 40 * MIN, "figure_s": 1800, "asks": 1}}));
        let n = notice(&r, "g473", None, &work(), now);
        assert_eq!(
            n,
            format!(
                "Giverny: progress check on `g473`: its worker asked for 45m more: finish {} → \
                 {}. Its note: \"tests are slow\".\nSo far: 38m of work; now `Bash: cargo test \
                 -p giverny-claude`; last wrote 1m ago; runs `cargo test -p giverny-claude` (6m \
                 in); background shells `sleep 600` (3m, shell bq1). Its transcript: \
                 /t/subagents/agent-a1.jsonl\nLook at what it has done: does it make sense, is it \
                 all needed, could it be done faster? Answer it (SendMessage `a1`): tell it to \
                 cut, change course or carry on; or let it go on. Giverny stops and re-estimates nothing.",
                c(now - 5 * MIN),
                c(now + 40 * MIN)
            )
        );
        let r = row(
            json!({"key": "g9", "stage": "running", "started": manage::stamp(T0),
            "eta_s": 1800, "waiting_since": manage::stamp(T0 + 35 * MIN), OVERDUE: 1800}),
        );
        let n = notice(
            &r,
            "g9",
            Some(600),
            &Work {
                worked_s: 1500,
                ..Work::default()
            },
            now,
        );
        assert!(
            n.starts_with(&format!(
                "Giverny: progress check on `g9`: its worker is overdue by 10m (its figure, 30m \
                 in all, ran out at {}).\nSo far: 25m of work; waiting since {}.\n",
                c(T0 + 30 * MIN),
                c(T0 + 35 * MIN)
            )),
            "{n}"
        );
        assert!(n.contains("Answer its worker: tell it to cut"), "{n}");
    }

    fn write_rows(dir: &Path, rows: Value) {
        let doc = json!({"version": 1, "session": "s1", "writer": manage::WRITER, "rows": rows});
        manage::write(&manage::file_for(dir, "s1"), &doc).unwrap();
        mark(dir, "s1", &doc);
    }

    fn rows(dir: &Path) -> Value {
        let b = std::fs::read(manage::file_for(dir, "s1")).unwrap();
        serde_json::from_slice::<Value>(&b).unwrap()["rows"].clone()
    }

    #[test]
    fn each_event_is_told_once_at_most_every_ten_minutes() {
        let dir = scratch("once");
        let look = Fake(work());
        let call = json!({"tool_name": "Bash", "tool_input": {"command": "echo next"}});
        let now = T0 + 40 * MIN;
        assert_eq!(deliver(&dir, "s1", &call, now, &look), None, "no marker");
        write_rows(
            &dir,
            json!([{"key": "g1", "stage": "running", "agent_id": "a1",
                "started": manage::stamp(T0), "eta_s": 85 * 60,
                MORE: {"from": now - 5 * MIN, "to": now + 45 * MIN, "figure_s": 1800, "asks": 1}}]),
        );
        assert!(marker(&dir, "s1").exists());
        let n = deliver(&dir, "s1", &call, now, &look).unwrap();
        assert!(n.contains("asked for 50m more"), "{n}");
        assert!(!marker(&dir, "s1").exists(), "nothing pending");
        assert_eq!(deliver(&dir, "s1", &call, now + MIN, &look), None, "once");
        // A second ask five minutes on waits out the quiet, then is told.
        let mut r = rows(&dir);
        r[0][MORE] = json!({"from": now + 45 * MIN, "to": now + 75 * MIN,
            "figure_s": 3000, "asks": 1});
        write_rows(&dir, r);
        assert_eq!(
            deliver(&dir, "s1", &call, now + 5 * MIN, &look),
            None,
            "quiet"
        );
        assert!(marker(&dir, "s1").exists(), "still pending");
        let n = deliver(&dir, "s1", &call, now + 10 * MIN, &look).unwrap();
        assert!(n.contains("asked for 30m more"), "{n}");
        assert!(!marker(&dir, "s1").exists());
        // Overdue: told once for the figure, not again.
        let mut r = rows(&dir);
        r[0]["eta_s"] = json!(60 * 60);
        r[0][OVERDUE] = json!(60 * 60);
        write_rows(&dir, r);
        let at = now + 25 * MIN; // T0 + 65m: 5m past
        let n = deliver(&dir, "s1", &call, at, &look).unwrap();
        assert!(n.contains("overdue by 5m"), "{n}");
        assert_eq!(rows(&dir)[0]["overdue_told"], 3600);
        let mut r = rows(&dir)[0].as_object().unwrap().clone();
        assert!(!overdue(&mut r, at + 20 * MIN), "same figure");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_moot_event_is_dropped_and_a_paused_one_kept() {
        let dir = scratch("moot");
        let look = Fake(work());
        let call = json!({"tool_name": "Bash", "tool_input": {"command": "ls"}});
        let now = T0 + 40 * MIN;
        let more = json!({"from": now, "to": now + 30 * MIN, "figure_s": 1800, "asks": 1});
        write_rows(
            &dir,
            json!([
                // Extended since it ran out: not overdue now.
                {"key": "a", "stage": "running", "started": manage::stamp(T0),
                 "eta_s": 3600, OVERDUE: 1800},
                // Landed.
                {"key": "b", "stage": "done", "started": manage::stamp(T0), MORE: more},
                // Paused: kept until it runs.
                {"key": "c", "stage": "running", "started": manage::stamp(T0), "eta_s": 4000,
                 "paused_since": manage::stamp(now - MIN), MORE: more}
            ]),
        );
        assert_eq!(deliver(&dir, "s1", &call, now, &look), None);
        let r = rows(&dir);
        assert!(r[0].get(OVERDUE).is_none() && r[1].get(MORE).is_none());
        assert!(r[2].get(MORE).is_some());
        assert!(marker(&dir, "s1").exists());
        // The manager's own `eta` on it is its decision: dropped.
        let own = json!({"tool_name": "Bash", "own_eta": "c"});
        let mut r = rows(&dir);
        r[2].as_object_mut().unwrap().remove("paused_since");
        write_rows(&dir, r);
        assert_eq!(deliver(&dir, "s1", &own, now, &look), None);
        assert!(rows(&dir)[2].get(MORE).is_none());
        assert!(!marker(&dir, "s1").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

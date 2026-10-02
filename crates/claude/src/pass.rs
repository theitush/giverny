//! `giverny pass`: the agents-pane feed's own writer.
//!
//! The pane draws what a feed file says (`docs/agents-pane.md`); this is the
//! writer that ships with Giverny, so an orchestrating Claude session needs
//! nothing else to show Running, Next up with ETAs, and Done. A task is any
//! short name. Every stamp is the machine's clock at the moment the command
//! runs, so ELAPSED and the Done row's `(+5m)` are measured, not reported.
//!
//! The feed file *is* the state: each command reads it, changes one row and
//! writes it back atomically, under a lock, since workers re-estimate their
//! own rows while the dispatcher stamps others. A feed some other writer owns
//! (its `writer` field names someone else, e.g. `coo/orchestrate-status`) is
//! never touched.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};

use crate::{feed, pass_history};

/// Marks the files this writer owns, so it never rewrites another's.
pub const WRITER: &str = "giverny/pass";

/// Claude Code exports the session id into every Bash command it runs, and a
/// subagent inherits its dispatcher's: a worker re-estimating its own row
/// writes into its dispatcher's file.
pub const SESSION_ENV: &str = "CLAUDE_CODE_SESSION_ID";

pub const USAGE: &str = "\
usage: giverny pass <command> [args] [--session <id>]

  plan  <task> --eta <dur> [--title T] [--note N] [--brief FILE] [--repo R]
                              queue a task (a Next up row) with its estimate
  start <task> [--eta <dur>] [--title T] [--agent <id>] [--note N] [--repo R]
                              the task's worker is starting now (Running)
  eta   <task> <dur left> [--note N] [--why wait|blocked|scope|load|ready]
                              [--title T] [--agent <id>] [--repo R]
                              re-estimate: this much is left from now (a task
                              with no row is started now, Running, with it);
                              --why wait (or blocked) marks the worker waiting
                              until its next eta, and that span is not work
  land  <task> [--outcome Done|Blocked|...] [--review TEXT] [--note N]
                              the task landed now (Done)
  pause <task> [--note N]     stop the task's clock; `resume <task>` restarts it
  drop  <task>                remove the task's row
  show                        print the rows
  path                        print the feed file's path
  clear-done                  clear the Done rows: from this session's feed,
                              and from the agents pane of the Giverny tab it runs in
  clear                       delete this session's feed

<task> is any short name (`auth-fix`, `#12`); name it, as a whole word, in the
worker's spawn description too. <dur> is minutes (`25`) or `25m`, `1h30m`, `1.5h`.
The session is --session, else $CLAUDE_CODE_SESSION_ID (set inside Claude Code).
The feed goes to $GIVERNY_FEED_DIR, else <config>/giverny/feeds.

Estimates learn (giverny#143): every landed task appends its estimate, wall time
and working time (wall minus pauses and waits) to history.jsonl beside the feeds
($GIVERNY_PASS_HISTORY overrides; empty turns it off). `plan`/`start --eta N`
scale N by the median working-time/estimate ratio of recent tasks of the same
kind (repo + the title's type word, as `BUG:`), else the repo, else all; the
pane counts down from that, and both figures are printed. `nudge` is the
plugin's hook: it asks a worker to re-estimate five minutes into its task.";

/// One `giverny pass` command, parsed.
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Plan(String),
    Start(String),
    Eta(String, u64),
    Land(String),
    Pause(String),
    Resume(String),
    Drop(String),
    Show,
    Path,
    ClearDone,
    Clear,
    /// The plugin's `PostToolUse` hook: a hook payload on stdin.
    Nudge,
}

/// The flags any command may carry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Flags {
    pub eta_s: Option<u64>,
    pub title: Option<String>,
    pub note: Option<String>,
    pub brief: Option<String>,
    pub agent: Option<String>,
    pub outcome: Option<String>,
    pub review: Option<String>,
    pub session: Option<String>,
    /// `eta --why`: `wait` / `blocked` open a waiting span.
    pub why: Option<String>,
    /// The repo the task is from, when the key and directory do not say.
    pub repo: Option<String>,
    /// Set by [`run_in`], not parsed: the guess as given, before correction,
    /// and where the correction came from.
    pub guess_s: Option<u64>,
    pub basis: Option<String>,
}

/// `25` (minutes), `25m`, `90s`, `1h30m`, `1.5h` → seconds.
pub fn parse_dur(s: &str) -> Option<u64> {
    let s = s.trim().trim_start_matches('~').to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    if let Ok(m) = s.parse::<f64>() {
        return (m.is_finite() && m >= 0.0).then(|| (m * 60.0).round() as u64);
    }
    let (mut total, mut num) = (0.0f64, String::new());
    for c in s.chars() {
        match c {
            '0'..='9' | '.' => num.push(c),
            'h' | 'm' | 's' | 'd' => {
                let n: f64 = num.parse().ok()?;
                num.clear();
                total += n * match c {
                    'd' => 86400.0,
                    'h' => 3600.0,
                    'm' => 60.0,
                    _ => 1.0,
                };
            }
            ' ' => {}
            _ => return None,
        }
    }
    (num.is_empty() && total.is_finite()).then(|| total.round() as u64)
}

/// Parse `giverny pass …`'s arguments (after `pass`).
pub fn parse_args(args: &[String]) -> Result<(Cmd, Flags), String> {
    let mut flags = Flags::default();
    let mut pos: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a.as_str() {
            "--eta" => {
                let v = val("--eta")?;
                flags.eta_s = Some(parse_dur(&v).ok_or_else(|| format!("bad duration: {v}"))?);
            }
            "--title" => flags.title = Some(val("--title")?),
            "--note" => flags.note = Some(val("--note")?),
            "--brief" => flags.brief = Some(val("--brief")?),
            "--agent" => flags.agent = Some(val("--agent")?),
            "--outcome" => flags.outcome = Some(val("--outcome")?),
            "--review" => flags.review = Some(val("--review")?),
            "--session" => flags.session = Some(val("--session")?),
            "--why" => flags.why = Some(val("--why")?),
            "--repo" => flags.repo = Some(val("--repo")?),
            "-h" | "--help" => return Err(USAGE.into()),
            s if s.starts_with("--") => return Err(format!("unknown flag {s}\n\n{USAGE}")),
            _ => pos.push(a.clone()),
        }
    }
    let mut pos = pos.into_iter();
    let verb = pos.next().ok_or_else(|| USAGE.to_string())?;
    let mut task = || {
        pos.next()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .ok_or_else(|| format!("`{verb}` needs a task name\n\n{USAGE}"))
    };
    let cmd = match verb.as_str() {
        "plan" => Cmd::Plan(task()?),
        "start" => Cmd::Start(task()?),
        "eta" => {
            let t = task()?;
            let left = pos
                .next()
                .or_else(|| flags.eta_s.map(|s| format!("{}s", s)))
                .ok_or("`eta` needs the time left, e.g. `giverny pass eta auth-fix 20`")?;
            Cmd::Eta(
                t,
                parse_dur(&left).ok_or_else(|| format!("bad duration: {left}"))?,
            )
        }
        "land" | "done" => Cmd::Land(task()?),
        "pause" => Cmd::Pause(task()?),
        "resume" => Cmd::Resume(task()?),
        "drop" => Cmd::Drop(task()?),
        "show" => Cmd::Show,
        "path" => Cmd::Path,
        "clear-done" | "clear_done" | "cleardone" => Cmd::ClearDone,
        "clear" => Cmd::Clear,
        "nudge" => Cmd::Nudge,
        other => return Err(format!("unknown command {other}\n\n{USAGE}")),
    };
    if matches!(cmd, Cmd::Plan(_)) && flags.eta_s.is_none() {
        return Err("`plan` needs --eta: the pane's Next up rows show it".into());
    }
    Ok((cmd, flags))
}

pub(crate) fn stamp(ms: u64) -> String {
    jiff::Timestamp::from_second((ms / 1000) as i64)
        .map(|t| t.to_string())
        .unwrap_or_default()
}

pub(crate) fn ms_of(row: &Map<String, Value>, key: &str) -> Option<u64> {
    match row.get(key)? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s
            .trim()
            .parse::<jiff::Timestamp>()
            .ok()
            .map(|t| t.as_millisecond().max(0) as u64),
        _ => None,
    }
}

pub(crate) fn u64_of(row: &Map<String, Value>, key: &str) -> Option<u64> {
    row.get(key).and_then(Value::as_u64)
}

pub(crate) fn stage_of(row: &Map<String, Value>) -> Option<feed::Stage> {
    row.get("stage")
        .and_then(Value::as_str)
        .and_then(feed::Stage::parse)
}

fn stage_word(s: feed::Stage) -> &'static str {
    match s {
        feed::Stage::Running => "running",
        feed::Stage::Planned => "planned",
        feed::Stage::Done => "done",
    }
}

/// A fresh feed document for `session`.
pub fn new_doc(session: &str) -> Value {
    json!({ "version": feed::FEED_VERSION, "session": session, "writer": WRITER, "rows": [] })
}

/// Who wrote this document, when it says.
pub fn writer_of(doc: &Value) -> Option<&str> {
    doc.get("writer").and_then(Value::as_str)
}

fn rows_mut(doc: &mut Value) -> Result<&mut Vec<Value>, String> {
    let obj = doc.as_object_mut().ok_or("the feed is not a JSON object")?;
    let rows = obj.entry("rows").or_insert_with(|| json!([]));
    if !rows.is_array() {
        *rows = json!([]);
    }
    Ok(rows.as_array_mut().expect("just made an array"))
}

fn find(rows: &[Value], key: &str) -> Option<usize> {
    rows.iter()
        .position(|r| r.get("key").and_then(Value::as_str) == Some(key))
}

fn set_str(row: &mut Map<String, Value>, key: &str, v: &Option<String>) {
    if let Some(v) = v {
        row.insert(key.into(), Value::String(v.clone()));
    }
}

/// Close an open pause at `now`: `started` moves on by the span, which is
/// added to `paused_s`; `spawned` keeps the true start (coo#170's fields).
fn close_pause(row: &mut Map<String, Value>, now: u64) {
    let Some(since) = ms_of(row, "paused_since") else {
        return;
    };
    row.remove("paused_since");
    let span = now.saturating_sub(since);
    if let Some(started) = ms_of(row, "started") {
        if !row.contains_key("spawned") {
            row.insert("spawned".into(), Value::String(stamp(started)));
        }
        row.insert("started".into(), Value::String(stamp(started + span)));
    }
    let total = u64_of(row, "paused_s").unwrap_or(0) + span / 1000;
    row.insert("paused_s".into(), json!(total));
}

/// Is this `--why` the worker saying it is waiting rather than working?
fn is_wait(why: Option<&str>) -> bool {
    why.is_some_and(|w| {
        let w = w.trim().to_ascii_lowercase();
        w == "wait" || w == "waiting" || w == "blocked"
    })
}

/// Close an open waiting span at `now`: its length goes to `wait_s`, which
/// the history takes off the task's working time (giverny#143). The pane's
/// clock is not moved: a wait is still wall time.
fn close_wait(row: &mut Map<String, Value>, now: u64) {
    let Some(since) = ms_of(row, "waiting_since") else {
        return;
    };
    row.remove("waiting_since");
    let total = u64_of(row, "wait_s").unwrap_or(0) + now.saturating_sub(since) / 1000;
    row.insert("wait_s".into(), json!(total));
}

/// Record the guess as given beside the (perhaps corrected) `eta_s`.
fn set_guess(row: &mut Map<String, Value>, f: &Flags) {
    let Some(eta) = f.eta_s else { return };
    row.insert("eta_guess_s".into(), json!(f.guess_s.unwrap_or(eta)));
    match &f.basis {
        Some(b) => row.insert("eta_basis".into(), json!(b)),
        None => row.remove("eta_basis"),
    };
}

/// `start <task> --agent <worker>` on a worker already running another
/// task: the dispatcher has handed it the next one (giverny#141), so the
/// worker's earlier Running rows land now, each with its own measured span,
/// and the pane gives the new task its own clock and its own tokens. A row
/// started less than [`feed::LATER_TASK_MS`] before is not an earlier task
/// but one of a batch the worker was spawned with (`Work #144 #145`), and
/// keeps running. Returns the keys landed.
fn hand_off(rows: &mut [Value], new: usize, agent: &str, now: u64) -> Vec<String> {
    let mut landed = Vec::new();
    for (i, row) in rows.iter_mut().enumerate() {
        let Some(row) = row.as_object_mut() else {
            continue;
        };
        let earlier = i != new
            && row.get("agent_id").and_then(Value::as_str) == Some(agent)
            && stage_of(row) == Some(feed::Stage::Running)
            && ms_of(row, "started").is_some_and(|s| s.saturating_add(feed::LATER_TASK_MS) <= now);
        if !earlier {
            continue;
        }
        close_pause(row, now);
        close_wait(row, now);
        row.insert("stage".into(), json!("done"));
        row.insert("ended".into(), json!(stamp(now)));
        row.entry("landing").or_insert(json!("Done"));
        if let Some(k) = row.get("key").and_then(Value::as_str) {
            landed.push(k.to_string());
        }
    }
    landed
}

/// Apply one command to a feed document at `now` (epoch ms). Returns the
/// line to print. `Show`, `Path`, `Clear` and `ClearDone` are the caller's.
pub fn apply(doc: &mut Value, cmd: &Cmd, f: &Flags, now: u64) -> Result<String, String> {
    let rows = rows_mut(doc)?;
    let key = match cmd {
        Cmd::Plan(k)
        | Cmd::Start(k)
        | Cmd::Eta(k, _)
        | Cmd::Land(k)
        | Cmd::Pause(k)
        | Cmd::Resume(k)
        | Cmd::Drop(k) => k.clone(),
        Cmd::Show | Cmd::Path | Cmd::Clear | Cmd::ClearDone | Cmd::Nudge => {
            return Ok(String::new());
        }
    };
    let at = find(rows, &key);
    let stage = at.and_then(|i| rows[i].as_object().and_then(stage_of));
    let missing = || format!("no task `{key}` in this pass (`giverny pass show` lists them)");

    match cmd {
        Cmd::Plan(_) => {
            if let Some(s) = stage.filter(|s| *s != feed::Stage::Planned) {
                return Err(format!(
                    "`{key}` is already {}; `giverny pass eta` re-estimates it",
                    stage_word(s)
                ));
            }
            let i = at.unwrap_or_else(|| {
                rows.push(json!({ "key": key }));
                rows.len() - 1
            });
            let row = rows[i].as_object_mut().ok_or("row is not an object")?;
            row.insert("stage".into(), json!("planned"));
            row.insert("eta_s".into(), json!(f.eta_s.unwrap_or(0)));
            set_guess(row, f);
            set_str(row, "title", &f.title);
            set_str(row, "note", &f.note);
            set_str(row, "brief", &f.brief);
            set_str(row, "repo", &f.repo);
            Ok(format!("planned {key}"))
        }
        Cmd::Start(_) => {
            if stage == Some(feed::Stage::Done) {
                return Err(format!("`{key}` has landed already"));
            }
            let i = at.unwrap_or_else(|| {
                rows.push(json!({ "key": key }));
                rows.len() - 1
            });
            let row = rows[i].as_object_mut().ok_or("row is not an object")?;
            let msg = if stage == Some(feed::Stage::Running) {
                close_pause(row, now);
                format!("{key} is running already (since {})", {
                    ms_of(row, "started").map(stamp).unwrap_or_default()
                })
            } else {
                row.insert("stage".into(), json!("running"));
                row.insert("started".into(), json!(stamp(now)));
                for k in [
                    "ended",
                    "paused_since",
                    "paused_s",
                    "spawned",
                    "waiting_since",
                    "wait_s",
                    "reestimate_asked",
                ] {
                    row.remove(k);
                }
                format!("started {key}")
            };
            if let Some(eta) = f.eta_s {
                row.insert("eta_s".into(), json!(eta));
            }
            set_guess(row, f);
            set_str(row, "repo", &f.repo);
            set_str(row, "title", &f.title);
            set_str(row, "agent_id", &f.agent);
            set_str(row, "note", &f.note);
            let handed = match &f.agent {
                Some(agent) => hand_off(rows, i, agent, now),
                None => Vec::new(),
            };
            Ok(match handed.as_slice() {
                [] => msg,
                done => format!(
                    "{msg}; {agent} handed on from {}",
                    done.join(", "),
                    agent = f.agent.as_deref().unwrap_or("")
                ),
            })
        }
        Cmd::Eta(_, left) if at.is_none() => {
            // A worker spawned outside a pass (giverny#140, #144) has no row
            // for `eta` to re-estimate, and an error would teach nothing:
            // start one now, with what is left as its estimate. The time
            // the worker spent before this is not known, so the clock starts
            // here; and the figure is a re-estimate, which (like every `eta`)
            // is taken as given, not scaled by the history.
            let mut sf = f.clone();
            sf.eta_s = Some(*left);
            sf.guess_s = None;
            sf.basis = None;
            apply(doc, &Cmd::Start(key.clone()), &sf, now)?;
            let rows = rows_mut(doc)?;
            if let Some(row) = find(rows, &key).and_then(|i| rows[i].as_object_mut())
                && is_wait(f.why.as_deref())
            {
                row.insert("waiting_since".into(), json!(stamp(now)));
            }
            Ok(format!(
                "{key}: no row in this pass, so started it now with ~{} left \
                 (as given; `giverny pass start {key} --eta <min> --agent <id>` \
                 before the spawn gives a row its whole time and a corrected estimate)",
                feed::fmt_span(*left as i64)
            ))
        }
        Cmd::Eta(_, left) => {
            let i = at.ok_or_else(missing)?;
            let row = rows[i].as_object_mut().ok_or("row is not an object")?;
            let eta = match stage {
                Some(feed::Stage::Running) => {
                    let started = ms_of(row, "started").unwrap_or(now);
                    let upto = ms_of(row, "paused_since").unwrap_or(now);
                    upto.saturating_sub(started) / 1000 + left
                }
                Some(feed::Stage::Done) => return Err(format!("`{key}` has landed already")),
                _ => *left,
            };
            if stage == Some(feed::Stage::Running)
                && !row.contains_key("eta_first_s")
                && let Some(first) = u64_of(row, "eta_s")
            {
                row.insert("eta_first_s".into(), json!(first));
            }
            row.insert("eta_s".into(), json!(eta));
            set_str(row, "note", &f.note);
            if stage == Some(feed::Stage::Running) {
                if is_wait(f.why.as_deref()) {
                    if !row.contains_key("waiting_since") && !row.contains_key("paused_since") {
                        row.insert("waiting_since".into(), json!(stamp(now)));
                    }
                } else {
                    close_wait(row, now);
                }
            }
            Ok(format!("{key}: ~{} left", feed::fmt_span(*left as i64)))
        }
        Cmd::Land(_) => {
            let i = at.ok_or_else(missing)?;
            let row = rows[i].as_object_mut().ok_or("row is not an object")?;
            close_pause(row, now);
            close_wait(row, now);
            if !row.contains_key("started") {
                // Landed without ever being started: its span is unknown, so
                // it gets no delta rather than a made-up one.
                row.remove("eta_s");
            }
            let outcome = f
                .outcome
                .clone()
                .unwrap_or_else(|| if f.review.is_some() { "Review" } else { "Done" }.to_string());
            let who = f
                .review
                .as_deref()
                .and_then(|r| r.split(" — ").next())
                .map(str::trim)
                .filter(|w| !w.is_empty() && f.outcome.is_none());
            let mut landing = match who {
                Some(w) => format!("{outcome} — {w}"),
                None => outcome,
            };
            if let Some(n) = f.note.as_deref().filter(|n| !n.trim().is_empty()) {
                landing = format!("{landing} — {n}");
            }
            row.insert("stage".into(), json!("done"));
            row.insert("ended".into(), json!(stamp(now)));
            row.insert("landing".into(), json!(landing));
            set_str(row, "review", &f.review);
            set_str(row, "agent_id", &f.agent);
            Ok(format!("landed {key}: {landing}"))
        }
        Cmd::Pause(_) => {
            let i = at.ok_or_else(missing)?;
            if stage != Some(feed::Stage::Running) {
                return Err(format!("`{key}` is not running"));
            }
            let row = rows[i].as_object_mut().ok_or("row is not an object")?;
            // A pause supersedes a wait: the span is counted once, as paused.
            close_wait(row, now);
            if !row.contains_key("paused_since") {
                row.insert("paused_since".into(), json!(stamp(now)));
                row.entry("paused_s").or_insert(json!(0));
            }
            set_str(row, "note", &f.note);
            Ok(format!("paused {key}"))
        }
        Cmd::Resume(_) => {
            let i = at.ok_or_else(missing)?;
            let row = rows[i].as_object_mut().ok_or("row is not an object")?;
            close_pause(row, now);
            set_str(row, "note", &f.note);
            Ok(format!("resumed {key}"))
        }
        Cmd::Drop(_) => {
            let i = at.ok_or_else(missing)?;
            rows.remove(i);
            Ok(format!("dropped {key}"))
        }
        Cmd::Show | Cmd::Path | Cmd::Clear | Cmd::ClearDone | Cmd::Nudge => unreachable!(),
    }
}

/// The rows as text, in the pane's section order.
pub fn show(doc: &Value, now: u64) -> String {
    let rows = doc
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = String::new();
    for want in [
        feed::Stage::Running,
        feed::Stage::Planned,
        feed::Stage::Done,
    ] {
        for r in rows.iter().filter_map(Value::as_object) {
            if stage_of(r) != Some(want) {
                continue;
            }
            let key = r.get("key").and_then(Value::as_str).unwrap_or("?");
            let title = r.get("title").and_then(Value::as_str).unwrap_or("");
            let started = ms_of(r, "started");
            let eta = u64_of(r, "eta_s");
            let cell = match want {
                feed::Stage::Running => {
                    let upto = ms_of(r, "paused_since").unwrap_or(now);
                    let el = started.map(|s| upto.saturating_sub(s) / 1000);
                    let left = eta.zip(el).map(|(e, el)| e as i64 - el as i64);
                    format!(
                        "{} elapsed, {}{}",
                        el.map(|e| feed::fmt_span(e as i64)).unwrap_or("?".into()),
                        left.map(|l| format!("~{} left", feed::fmt_span(l.max(0))))
                            .unwrap_or("no estimate".into()),
                        if r.contains_key("paused_since") {
                            ", paused"
                        } else {
                            ""
                        }
                    )
                }
                feed::Stage::Planned => {
                    let guess = u64_of(r, "eta_guess_s")
                        .filter(|g| Some(*g) != eta)
                        .map(|g| format!(" (said {})", feed::fmt_span(g as i64)))
                        .unwrap_or_default();
                    eta.map(|e| format!("~{}{guess}", feed::fmt_span(e as i64)))
                        .unwrap_or_default()
                }
                feed::Stage::Done => {
                    let took = started
                        .zip(ms_of(r, "ended"))
                        .map(|(s, e)| e.saturating_sub(s) / 1000);
                    let landing = r.get("landing").and_then(Value::as_str).unwrap_or("Done");
                    match took {
                        Some(t) => format!(
                            "{landing}, took {}{}",
                            feed::fmt_span(t as i64),
                            eta.map(|e| format!(" {}", feed::fmt_delta(t as i64 - e as i64)))
                                .unwrap_or_default()
                        ),
                        None => landing.to_string(),
                    }
                }
            };
            out.push_str(&format!(
                "{:<8} {key:<16} {cell:<32} {title}\n",
                stage_word(want)
            ));
        }
    }
    if out.is_empty() {
        out.push_str("(no rows)\n");
    }
    out
}

/// A lock beside the feed, so concurrent `giverny pass` runs (a dispatcher
/// and its workers) never lose each other's rows. Taken with `create_new`;
/// one left behind by a killed run is broken after ten seconds.
pub(crate) struct Lock(PathBuf);

impl Lock {
    pub(crate) fn take(file: &Path) -> Result<Lock, String> {
        let lock = file.with_extension("json.lock");
        for _ in 0..100 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock)
            {
                Ok(_) => return Ok(Lock(lock)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&lock)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| SystemTime::now().duration_since(t).ok())
                        .is_some_and(|age| age > Duration::from_secs(10));
                    if stale {
                        let _ = std::fs::remove_file(&lock);
                    } else {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                Err(e) => return Err(format!("{}: {e}", lock.display())),
            }
        }
        Err(format!("{} is held", lock.display()))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Write `doc` to `file` atomically (a non-`.json` temp name, then rename),
/// and only when its bytes changed: the pane re-reads on a changed mtime.
pub fn write(file: &Path, doc: &Value) -> std::io::Result<()> {
    let mut data = serde_json::to_vec_pretty(doc)?;
    data.push(b'\n');
    if std::fs::read(file).is_ok_and(|old| old == data) {
        return Ok(());
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::File::create(&tmp)?.write_all(&data)?;
    std::fs::rename(&tmp, file)
}

/// The file this session's pass lives in: `<session>.json`, or an existing
/// file that names the session as an alias.
pub fn file_for(dir: &Path, session: &str) -> PathBuf {
    feed::find(dir, session)
        .map(|(p, _)| p)
        .unwrap_or_else(|| feed::feed_path(dir, session))
}

/// Run one command against `dir` for `session` at `now`. The output line,
/// or why not.
pub fn run_in(
    dir: &Path,
    session: &str,
    cmd: &Cmd,
    flags: &Flags,
    now: u64,
) -> Result<String, String> {
    if session.is_empty() || session.contains(['/', '\\']) || session.starts_with('.') {
        return Err(format!("not a session id: {session:?}"));
    }
    let file = file_for(dir, session);
    if *cmd == Cmd::Path {
        return Ok(file.display().to_string());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _lock = Lock::take(&file)?;
    let existing = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object);
    if *cmd == Cmd::ClearDone {
        return Ok(clear_done(&file, existing));
    }
    if let Some(doc) = &existing
        && let Some(w) = writer_of(doc)
        && w != WRITER
    {
        return Err(format!(
            "{} is written by {w}; giverny pass leaves it alone",
            file.display()
        ));
    }
    match cmd {
        Cmd::Show => Ok(show(existing.as_ref().unwrap_or(&new_doc(session)), now)),
        Cmd::Clear => {
            if existing.is_some() {
                std::fs::remove_file(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            }
            Ok("cleared".into())
        }
        _ => {
            let mut doc = existing.unwrap_or_else(|| new_doc(session));
            let obj = doc.as_object_mut().expect("filtered to objects");
            obj.insert("writer".into(), json!(WRITER));
            obj.entry("version").or_insert(json!(feed::FEED_VERSION));
            obj.entry("session").or_insert(json!(session));
            let history = pass_history::path(dir);
            let mut flags = flags.clone();
            let said = correct_estimate(&doc, cmd, &mut flags, history.as_deref());
            let before = done_keys(&doc);
            let msg = apply(&mut doc, cmd, &flags, now)?;
            write(&file, &doc).map_err(|e| format!("{}: {e}", file.display()))?;
            if let Some(h) = &history {
                learn(&doc, &before, session, h);
            }
            Ok(match said {
                Some(said) => format!("{msg}: {said}"),
                None => msg,
            })
        }
    }
}

/// `plan`/`start` with `--eta N`: scale N from the history (giverny#143).
/// Sets `flags.eta_s` to the corrected figure, keeping N as `guess_s`, and
/// fills in the repo the row will remember. Returns the line telling the
/// dispatcher both numbers, or `None` for a command with no estimate.
fn correct_estimate(
    doc: &Value,
    cmd: &Cmd,
    flags: &mut Flags,
    history: Option<&Path>,
) -> Option<String> {
    let (Cmd::Plan(key) | Cmd::Start(key)) = cmd else {
        return None;
    };
    let guess = flags.eta_s?;
    let row = doc
        .get("rows")
        .and_then(Value::as_array)
        .and_then(|rows| find(rows, key).map(|i| &rows[i]))
        .and_then(Value::as_object);
    let row_str = |k: &str| row.and_then(|r| r.get(k)).and_then(Value::as_str);
    if flags.repo.is_none() {
        flags.repo = row_str("repo").map(String::from).or_else(|| {
            let cwd = std::env::current_dir().unwrap_or_default();
            pass_history::repo_of(key, &cwd)
        });
    }
    let title = flags.title.as_deref().or(row_str("title")).unwrap_or("");
    let kind = pass_history::kind_of(title);
    let past = history.map(pass_history::load).unwrap_or_default();
    let fix = pass_history::correct(&past, flags.repo.as_deref(), kind.as_deref(), guess);
    flags.guess_s = Some(guess);
    let span = |s: u64| feed::fmt_span(s as i64);
    Some(match fix {
        Some(c) => {
            flags.eta_s = Some(c.eta_s);
            flags.basis = Some(c.describe());
            format!(
                "~{} (you said {}; {})",
                span(c.eta_s),
                span(guess),
                c.describe()
            )
        }
        None => {
            flags.basis = None;
            let n = past.len();
            format!(
                "~{} as given ({} landed task{} in the history, too few to correct it)",
                span(guess),
                n,
                if n == 1 { "" } else { "s" }
            )
        }
    })
}

/// The keys of the Done rows in a feed document.
fn done_keys(doc: &Value) -> Vec<String> {
    doc.get("rows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter(|r| stage_of(r) == Some(feed::Stage::Done))
        .filter_map(|r| r.get("key").and_then(Value::as_str).map(String::from))
        .collect()
}

/// Append every row that landed in this command (Done now, not before) to
/// the history. A row that never started has no span and teaches nothing.
fn learn(doc: &Value, before: &[String], session: &str, history: &Path) {
    let rows = doc.get("rows").and_then(Value::as_array);
    for r in rows.into_iter().flatten().filter_map(Value::as_object) {
        let Some(key) = r.get("key").and_then(Value::as_str) else {
            continue;
        };
        if stage_of(r) != Some(feed::Stage::Done) || before.iter().any(|k| k == key) {
            continue;
        }
        if let Some(rec) = record_of(r, session)
            && let Err(e) = pass_history::append(history, &rec)
        {
            eprintln!("giverny pass: {}: {e}", history.display());
        }
    }
}

/// A landed row as a history record: wall time from its true start
/// (`spawned` when a pause moved `started` on), working time that less its
/// paused and waiting spans.
pub fn record_of(r: &Map<String, Value>, session: &str) -> Option<pass_history::Record> {
    let started = ms_of(r, "spawned").or_else(|| ms_of(r, "started"))?;
    let ended = ms_of(r, "ended")?;
    let wall_s = ended.saturating_sub(started) / 1000;
    let paused_s = u64_of(r, "paused_s").unwrap_or(0);
    let wait_s = u64_of(r, "wait_s").unwrap_or(0);
    let s = |k: &str| r.get(k).and_then(Value::as_str).map(String::from);
    let title = s("title");
    Some(pass_history::Record {
        key: s("key").unwrap_or_default(),
        kind: title.as_deref().and_then(pass_history::kind_of),
        title,
        repo: s("repo"),
        session: Some(session.to_string()),
        estimate_s: u64_of(r, "eta_guess_s")
            .or_else(|| u64_of(r, "eta_first_s"))
            .or_else(|| u64_of(r, "eta_s")),
        eta_s: u64_of(r, "eta_first_s").or_else(|| u64_of(r, "eta_s")),
        eta_final_s: u64_of(r, "eta_s"),
        wall_s,
        paused_s,
        wait_s,
        work_s: wall_s.saturating_sub(paused_s + wait_s),
        started: Some(stamp(started)),
        ended: Some(stamp(ended)),
        outcome: s("landing"),
    })
}

/// `clear-done` on the file: drop its Done rows when this writer owns it.
/// Another writer's feed is left as it is — the pane hides its Done rows
/// itself once asked ([`main`] asks it) — and so is a missing one.
fn clear_done(file: &Path, existing: Option<Value>) -> String {
    let Some(mut doc) = existing else {
        return "no feed for this session".into();
    };
    if let Some(w) = writer_of(&doc).filter(|w| *w != WRITER) {
        return format!("the feed is {w}'s: left as it is");
    }
    let Ok(rows) = rows_mut(&mut doc) else {
        return "the feed is not a JSON object: left as it is".into();
    };
    let before = rows.len();
    rows.retain(|r| r.as_object().and_then(stage_of) != Some(feed::Stage::Done));
    let gone = before - rows.len();
    if gone == 0 {
        return "no Done rows in the feed".into();
    }
    match write(file, &doc) {
        Ok(()) => format!(
            "cleared {gone} Done row{} from the feed",
            if gone == 1 { "" } else { "s" }
        ),
        Err(e) => format!("{}: {e}", file.display()),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The `giverny pass` entrypoint. Returns the process exit code. `spool` is
/// where a message to the app goes when its socket is not there
/// (`clear-done`).
pub fn main(args: &[String], spool: &Path) -> i32 {
    let (cmd, flags) = match parse_args(args) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    if cmd == Cmd::Nudge {
        // A hook: whatever happens, it never fails the tool call it rides on.
        let mut input = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
        if let Ok(payload) = serde_json::from_str::<Value>(&input)
            && let Some(out) = crate::pass_nudge::run(&payload, &feed::feed_dir(), now_ms())
        {
            println!("{out}");
        }
        return 0;
    }
    let session = flags
        .session
        .clone()
        .or_else(|| std::env::var(SESSION_ENV).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(session) = session else {
        eprintln!(
            "giverny pass: no session — run it from inside Claude Code \
             (it sets ${SESSION_ENV}) or pass --session <id>"
        );
        return 2;
    };
    let now = now_ms();
    match run_in(&feed::feed_dir(), &session, &cmd, &flags, now) {
        Ok(msg) if cmd == Cmd::ClearDone => {
            let pane = if crate::hooks::send_clear_done(spool, Some(&session), now) {
                "asked Giverny to clear the agents pane's Done rows in this tab"
            } else {
                "not in a Giverny tab, so no pane to clear"
            };
            println!("{msg}; {pane}");
            0
        }
        Ok(msg) => {
            if !msg.is_empty() {
                println!("{}", msg.trim_end());
            }
            0
        }
        Err(e) => {
            eprintln!("giverny pass: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    const T0: u64 = 1_790_000_000_000;
    const MIN: u64 = 60_000;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("giverny-pass-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn run(dir: &Path, line: &str, now: u64) -> Result<String, String> {
        let (cmd, flags) = parse_args(&args(line))?;
        run_in(dir, "s1", &cmd, &flags, now)
    }

    fn read_feed(dir: &Path) -> feed::Feed {
        feed::read(&feed::feed_path(dir, "s1")).expect("a feed the pane reads")
    }

    #[test]
    fn durations() {
        assert_eq!(parse_dur("25"), Some(1500));
        assert_eq!(parse_dur("25m"), Some(1500));
        assert_eq!(parse_dur("1h30m"), Some(5400));
        assert_eq!(parse_dur("1.5h"), Some(5400));
        assert_eq!(parse_dur("~2h"), Some(7200));
        assert_eq!(parse_dur("90s"), Some(90));
        assert_eq!(parse_dur("soon"), None);
        assert_eq!(parse_dur("-5"), None);
        assert_eq!(parse_dur("5x"), None);
    }

    #[test]
    fn plan_needs_an_estimate_and_a_name() {
        assert!(parse_args(&args("plan a")).is_err());
        assert!(parse_args(&args("plan --eta 5")).is_err());
        assert!(parse_args(&args("eta a")).is_err());
        assert!(parse_args(&args("bogus a")).is_err());
        assert_eq!(
            parse_args(&args("eta a 1h")).unwrap().0,
            Cmd::Eta("a".into(), 3600)
        );
    }

    #[test]
    fn a_pass_from_plan_to_land_is_what_the_pane_reads() {
        let dir = scratch("life");
        run(&dir, "plan auth-fix --eta 30 --title Fix", T0).unwrap();
        run(&dir, "plan docs --eta 1h", T0).unwrap();
        let f = read_feed(&dir);
        assert_eq!(f.session.as_deref(), Some("s1"));
        assert_eq!(f.rows.len(), 2);
        assert_eq!(f.rows[0].stage(), feed::Stage::Planned);
        assert_eq!(f.rows[0].eta_s, Some(1800));
        assert_eq!(f.rows[0].title.as_deref(), Some("Fix"));
        assert_eq!(f.rows[1].eta_s, Some(3600));

        // Started five minutes later: the clock is the machine's.
        run(&dir, "start auth-fix", T0 + 5 * MIN).unwrap();
        let f = read_feed(&dir);
        assert_eq!(f.rows[0].stage(), feed::Stage::Running);
        assert_eq!(f.rows[0].started_ms, Some(T0 + 5 * MIN));
        assert_eq!(f.rows[0].eta_s, Some(1800), "the planned estimate carries");

        // Ten minutes in, the worker says 40 more: the total is 50.
        run(&dir, "eta auth-fix 40 --note bigger", T0 + 15 * MIN).unwrap();
        let f = read_feed(&dir);
        assert_eq!(f.rows[0].eta_s, Some(50 * 60));
        assert_eq!(f.rows[0].note.as_deref(), Some("bigger"));

        // Lands at 55 minutes: five late against 50.
        run(&dir, "land auth-fix", T0 + 60 * MIN).unwrap();
        let f = read_feed(&dir);
        let r = &f.rows[0];
        assert_eq!(r.stage(), feed::Stage::Done);
        assert_eq!(r.ended_ms, Some(T0 + 60 * MIN));
        assert_eq!(r.landing.as_deref(), Some("Done"));
        let took = (r.ended_ms.unwrap() - r.started_ms.unwrap()) as i64 / 1000;
        assert_eq!(took - r.eta_s.unwrap() as i64, 300);

        // Started again after landing is refused; eta on a landed row too.
        assert!(run(&dir, "start auth-fix", T0 + 61 * MIN).is_err());
        assert!(run(&dir, "eta auth-fix 5", T0 + 61 * MIN).is_err());
        let shown = run(&dir, "show", T0 + 61 * MIN).unwrap();
        assert!(shown.contains("auth-fix"), "{shown}");
        assert!(shown.contains("(+5m)"), "{shown}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn starting_a_workers_next_task_lands_the_one_before() {
        let dir = scratch("handoff");
        // Spawned with a batch of two: both run, neither closes the other.
        run(&dir, "start a1 --agent w1", T0).unwrap();
        run(&dir, "start a2 --agent w1", T0 + MIN).unwrap();
        run(&dir, "start other --agent w2", T0).unwrap();
        let f = read_feed(&dir);
        assert!(f.rows.iter().all(|r| r.stage() == feed::Stage::Running));
        // Forty minutes on, the dispatcher hands w1 its next task.
        let said = run(&dir, "start b --agent w1 --eta 20", T0 + 40 * MIN).unwrap();
        assert!(said.contains("handed on from a1, a2"), "{said}");
        let f = read_feed(&dir);
        let row = |k: &str| f.rows.iter().find(|r| r.key == k).unwrap();
        for k in ["a1", "a2"] {
            assert_eq!(row(k).stage(), feed::Stage::Done, "{k}");
            assert_eq!(row(k).ended_ms, Some(T0 + 40 * MIN), "{k}");
            assert_eq!(row(k).landing.as_deref(), Some("Done"));
        }
        assert_eq!(row("b").stage(), feed::Stage::Running);
        assert_eq!(row("b").started_ms, Some(T0 + 40 * MIN));
        assert_eq!(
            row("other").stage(),
            feed::Stage::Running,
            "another worker's task runs on"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn eta_on_a_task_with_no_row_starts_it() {
        // giverny#144: a worker spawned outside a pass; its dispatcher (or
        // the worker) reaches for `eta`, and gets a Running row, not an error.
        let dir = scratch("eta-no-row");
        let said = run(
            &dir,
            "eta inbar#613 40 --agent w9 --title Graph --repo inbar",
            T0,
        )
        .unwrap();
        assert!(said.contains("started it now with ~40m left"), "{said}");
        assert!(
            said.contains("giverny pass start inbar#613 --eta"),
            "{said}"
        );
        let f = read_feed(&dir);
        let r = &f.rows[0];
        assert_eq!(r.key, "inbar#613");
        assert_eq!(r.stage(), feed::Stage::Running);
        assert_eq!(r.started_ms, Some(T0));
        assert_eq!(r.eta_s, Some(40 * 60), "as given, not corrected");
        assert_eq!(r.title.as_deref(), Some("Graph"));
        assert_eq!(r.agent_id.as_deref(), Some("w9"));

        // From then on it is an ordinary row: a later eta re-estimates it.
        run(&dir, "eta inbar#613 10", T0 + 20 * MIN).unwrap();
        assert_eq!(read_feed(&dir).rows[0].eta_s, Some(30 * 60));

        // `--why wait` on a missing row starts it waiting.
        run(&dir, "eta other 5 --why wait", T0).unwrap();
        let doc: Value =
            serde_json::from_slice(&std::fs::read(feed::feed_path(&dir, "s1")).unwrap()).unwrap();
        let other = doc["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == "other")
            .unwrap();
        assert!(other.get("waiting_since").is_some(), "{other}");

        // The other verbs still refuse a task that is not there.
        assert!(run(&dir, "land nope", T0).is_err());
        assert!(run(&dir, "pause nope", T0).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_text_rides_on_the_done_row() {
        let dir = scratch("review");
        run(&dir, "start ui --eta 20", T0).unwrap();
        let (cmd, mut flags) = parse_args(&args("land ui")).unwrap();
        flags.review = Some("ita — the empty state — npm run dev".into());
        run_in(&dir, "s1", &cmd, &flags, T0 + 10 * MIN).unwrap();
        let r = &read_feed(&dir).rows[0];
        assert_eq!(r.landing.as_deref(), Some("Review — ita"));
        assert_eq!(
            r.review.as_deref(),
            Some("ita — the empty state — npm run dev")
        );
        run(&dir, "start blk --eta 5", T0).unwrap();
        run(
            &dir,
            "land blk --outcome Blocked --note no-network",
            T0 + MIN,
        )
        .unwrap();
        let r = &read_feed(&dir).rows[1];
        assert_eq!(r.landing.as_deref(), Some("Blocked — no-network"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pause_moves_started_on_by_its_span() {
        let dir = scratch("pause");
        run(&dir, "start a --eta 30", T0).unwrap();
        run(&dir, "pause a", T0 + 10 * MIN).unwrap();
        let r = &read_feed(&dir).rows[0];
        assert_eq!(r.paused_since_ms, Some(T0 + 10 * MIN));
        // A re-estimate while paused counts only the time worked.
        run(&dir, "eta a 5", T0 + 20 * MIN).unwrap();
        assert_eq!(read_feed(&dir).rows[0].eta_s, Some(15 * 60));
        run(&dir, "resume a", T0 + 25 * MIN).unwrap();
        let r = &read_feed(&dir).rows[0];
        assert_eq!(r.paused_since_ms, None);
        assert_eq!(r.started_ms, Some(T0 + 15 * MIN));
        assert_eq!(r.spawned_ms, Some(T0));
        assert_eq!(r.paused_s, Some(15 * 60));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn someone_elses_feed_is_left_alone() {
        let dir = scratch("theirs");
        std::fs::create_dir_all(&dir).unwrap();
        let theirs = br#"{"version":1,"session":"s1","writer":"coo/orchestrate-status","rows":[]}"#;
        std::fs::write(feed::feed_path(&dir, "s1"), theirs).unwrap();
        let err = run(&dir, "start a", T0).unwrap_err();
        assert!(err.contains("coo/orchestrate-status"), "{err}");
        assert!(run(&dir, "clear", T0).is_err());
        assert_eq!(
            std::fs::read(feed::feed_path(&dir, "s1")).unwrap(),
            theirs.to_vec()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unchanged_feed_is_not_rewritten_and_leaves_no_temp_or_lock() {
        let dir = scratch("quiet");
        run(&dir, "plan a --eta 5", T0).unwrap();
        let file = feed::feed_path(&dir, "s1");
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        run(&dir, "plan a --eta 5", T0 + MIN).unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            before
        );
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["s1.json".to_string()]);
        run(&dir, "clear", T0).unwrap();
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_writers_lose_no_rows() {
        let dir = scratch("race");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    run(&dir, &format!("plan t{i} --eta 5"), T0).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(read_feed(&dir).rows.len(), 8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_done_drops_our_done_rows_and_leaves_anyone_elses() {
        let dir = scratch("clear-done");
        assert!(run(&dir, "clear-done", T0).unwrap().contains("no feed"));
        run(&dir, "start a --eta 5", T0).unwrap();
        run(&dir, "start b --eta 5", T0).unwrap();
        run(&dir, "plan c --eta 5", T0).unwrap();
        run(&dir, "land a", T0 + MIN).unwrap();
        run(&dir, "land b", T0 + MIN).unwrap();
        let msg = run(&dir, "clear-done", T0 + 2 * MIN).unwrap();
        assert_eq!(msg, "cleared 2 Done rows from the feed");
        let keys: Vec<String> = read_feed(&dir).rows.into_iter().map(|r| r.key).collect();
        assert_eq!(keys, ["c"]);
        assert!(
            run(&dir, "clear-done", T0)
                .unwrap()
                .contains("no Done rows")
        );

        let theirs = br#"{"version":1,"session":"s1","writer":"coo/orchestrate-status","rows":[{"key":"x","stage":"done"}]}"#;
        std::fs::write(feed::feed_path(&dir, "s1"), theirs).unwrap();
        let msg = run(&dir, "clear-done", T0).unwrap();
        assert!(msg.contains("coo/orchestrate-status"), "{msg}");
        assert_eq!(
            std::fs::read(feed::feed_path(&dir, "s1")).unwrap(),
            theirs.to_vec()
        );
        assert_eq!(parse_args(&args("clear-done")).unwrap().0, Cmd::ClearDone);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn history(dir: &Path) -> Vec<pass_history::Record> {
        pass_history::load(&dir.join(pass_history::FILE))
    }

    #[test]
    fn plan_and_start_correct_the_guess_from_history_and_keep_it() {
        let dir = scratch("correct");
        std::fs::create_dir_all(&dir).unwrap();
        let h = dir.join(pass_history::FILE);
        // Five landed giverny BUGs that took half their guess, and five
        // FEATUREs that took a quarter.
        for (kind, work) in [("BUG", 15), ("FEATURE", 10)] {
            for _ in 0..pass_history::MIN_SAMPLES {
                let rec = pass_history::Record {
                    key: "x".into(),
                    repo: Some("giverny".into()),
                    kind: Some(kind.into()),
                    estimate_s: Some(if kind == "BUG" { 1800 } else { 2400 }),
                    wall_s: work * 60,
                    work_s: work * 60,
                    outcome: Some("Done".into()),
                    ..Default::default()
                };
                pass_history::append(&h, &rec).unwrap();
            }
        }
        let (cmd, mut flags) = parse_args(&args("plan giverny#1 --eta 40")).unwrap();
        flags.title = Some("BUG: pane flickers".into());
        let said = run_in(&dir, "s1", &cmd, &flags, T0).unwrap();
        assert_eq!(
            said,
            "planned giverny#1: ~20m (you said 40m; ×0.50 from the last 5 BUG tasks in giverny)"
        );
        let doc: Value =
            serde_json::from_slice(&std::fs::read(feed::feed_path(&dir, "s1")).unwrap()).unwrap();
        let row = &doc["rows"][0];
        assert_eq!(
            row["eta_s"], 1200,
            "the pane counts down from the corrected figure"
        );
        assert_eq!(row["eta_guess_s"], 2400, "the raw guess is kept");
        assert_eq!(row["repo"], "giverny");
        assert!(run(&dir, "show", T0).unwrap().contains("~20m (said 40m)"));

        // `start --eta` re-corrects; a FEATURE title picks its own level.
        let (cmd, mut flags) = parse_args(&args("start giverny#2 --eta 40")).unwrap();
        flags.title = Some("FEATURE: estimates".into());
        let said = run_in(&dir, "s1", &cmd, &flags, T0).unwrap();
        assert!(
            said.ends_with("~10m (you said 40m; ×0.25 from the last 5 FEATURE tasks in giverny)"),
            "{said}"
        );

        // Another repo: everything (0.25 ×5, 0.5 ×5 → 0.375).
        let said = run(&dir, "plan inbar#3 --eta 40", T0).unwrap();
        assert!(
            said.ends_with("~15m (you said 40m; ×0.38 from the last 10 tasks)"),
            "{said}"
        );

        // No history: the guess stands, and says so.
        let empty = scratch("correct-empty");
        let said = run(&empty, "plan a --eta 40", T0).unwrap();
        assert!(said.contains("~40m as given (0 landed tasks"), "{said}");
        let r = &read_feed(&empty).rows[0];
        assert_eq!(r.eta_s, Some(2400));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn land_appends_wall_and_working_time_without_waits_or_pauses() {
        let dir = scratch("learn");
        let (cmd, mut flags) = parse_args(&args("start giverny#9 --eta 30")).unwrap();
        flags.title = Some("FEATURE: x".into());
        run_in(&dir, "s1", &cmd, &flags, T0).unwrap();
        // 10m work, then 20m waiting on a build slot, then 5m work…
        run(
            &dir,
            "eta giverny#9 30 --why wait --note slot",
            T0 + 10 * MIN,
        )
        .unwrap();
        run(&dir, "eta giverny#9 20 --why ready", T0 + 30 * MIN).unwrap();
        // …a 15m pause (a wait open across it is closed by the pause)…
        run(&dir, "eta giverny#9 15 --why blocked", T0 + 35 * MIN).unwrap();
        run(&dir, "pause giverny#9", T0 + 40 * MIN).unwrap();
        run(&dir, "resume giverny#9", T0 + 55 * MIN).unwrap();
        // …and 5m more work.
        run(&dir, "land giverny#9", T0 + 60 * MIN).unwrap();
        let h = history(&dir);
        assert_eq!(h.len(), 1);
        let r = &h[0];
        assert_eq!(r.key, "giverny#9");
        assert_eq!(r.repo.as_deref(), Some("giverny"));
        assert_eq!(r.kind.as_deref(), Some("FEATURE"));
        assert_eq!(r.estimate_s, Some(1800), "the guess as given");
        assert_eq!(r.eta_s, Some(1800), "the first figure the pane showed");
        assert_eq!(r.wall_s, 60 * 60, "wall time runs from the true start");
        assert_eq!(r.paused_s, 15 * 60);
        assert_eq!(r.wait_s, 25 * 60, "20m waiting, then 5m blocked");
        assert_eq!(r.work_s, 20 * 60);
        assert_eq!(r.outcome.as_deref(), Some("Done"));

        // A wait left open at landing ends there; a row never started, and a
        // second `land`, append nothing.
        run(&dir, "start b --eta 10", T0).unwrap();
        run(&dir, "eta b 5 --why wait", T0 + 4 * MIN).unwrap();
        run(&dir, "land b", T0 + 10 * MIN).unwrap();
        run(&dir, "plan c --eta 10", T0).unwrap();
        run(&dir, "land c", T0 + 10 * MIN).unwrap();
        run(&dir, "land b", T0 + 11 * MIN).unwrap();
        let h = history(&dir);
        assert_eq!(h.len(), 2);
        assert_eq!(
            (h[1].key.as_str(), h[1].wait_s, h[1].work_s),
            ("b", 360, 240)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_history_variable_turns_learning_off() {
        // Only the pure parts: the env var is process-wide, so the path is
        // checked through `pass_history::path` rather than by running.
        assert_eq!(
            pass_history::path(Path::new("/f")),
            std::env::var_os(pass_history::ENV)
                .map(|v| (!v.is_empty()).then(|| PathBuf::from(v)))
                .unwrap_or(Some(PathBuf::from("/f/history.jsonl")))
        );
    }

    #[test]
    fn a_session_id_is_a_file_name_not_a_path() {
        let dir = scratch("sid");
        let (cmd, flags) = parse_args(&args("plan a --eta 5")).unwrap();
        assert!(run_in(&dir, "../x", &cmd, &flags, T0).is_err());
        assert!(run_in(&dir, "", &cmd, &flags, T0).is_err());
    }
}

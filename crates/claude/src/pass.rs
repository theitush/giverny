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

use crate::feed;

/// Marks the files this writer owns, so it never rewrites another's.
pub const WRITER: &str = "giverny/pass";

/// Claude Code exports the session id into every Bash command it runs, and a
/// subagent inherits its dispatcher's: a worker re-estimating its own row
/// writes into its dispatcher's file.
pub const SESSION_ENV: &str = "CLAUDE_CODE_SESSION_ID";

pub const USAGE: &str = "\
usage: giverny pass <command> [args] [--session <id>]

  plan  <task> --eta <dur> [--title T] [--note N] [--brief FILE]
                              queue a task (a Next up row) with its estimate
  start <task> [--eta <dur>] [--title T] [--agent <id>] [--note N]
                              the task's worker is starting now (Running)
  eta   <task> <dur left> [--note N]
                              re-estimate: this much is left from now
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
The feed goes to $GIVERNY_FEED_DIR, else <config>/giverny/feeds.";

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
        other => return Err(format!("unknown command {other}\n\n{USAGE}")),
    };
    if matches!(cmd, Cmd::Plan(_)) && flags.eta_s.is_none() {
        return Err("`plan` needs --eta: the pane's Next up rows show it".into());
    }
    Ok((cmd, flags))
}

fn stamp(ms: u64) -> String {
    jiff::Timestamp::from_second((ms / 1000) as i64)
        .map(|t| t.to_string())
        .unwrap_or_default()
}

fn ms_of(row: &Map<String, Value>, key: &str) -> Option<u64> {
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

fn u64_of(row: &Map<String, Value>, key: &str) -> Option<u64> {
    row.get(key).and_then(Value::as_u64)
}

fn stage_of(row: &Map<String, Value>) -> Option<feed::Stage> {
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
        Cmd::Show | Cmd::Path | Cmd::Clear | Cmd::ClearDone => return Ok(String::new()),
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
            set_str(row, "title", &f.title);
            set_str(row, "note", &f.note);
            set_str(row, "brief", &f.brief);
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
                for k in ["ended", "paused_since", "paused_s", "spawned"] {
                    row.remove(k);
                }
                format!("started {key}")
            };
            if let Some(eta) = f.eta_s {
                row.insert("eta_s".into(), json!(eta));
            }
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
            Ok(format!("{key}: ~{} left", feed::fmt_span(*left as i64)))
        }
        Cmd::Land(_) => {
            let i = at.ok_or_else(missing)?;
            let row = rows[i].as_object_mut().ok_or("row is not an object")?;
            close_pause(row, now);
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
        Cmd::Show | Cmd::Path | Cmd::Clear | Cmd::ClearDone => unreachable!(),
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
                feed::Stage::Planned => eta
                    .map(|e| format!("~{}", feed::fmt_span(e as i64)))
                    .unwrap_or_default(),
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
struct Lock(PathBuf);

impl Lock {
    fn take(file: &Path) -> Result<Lock, String> {
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
            let msg = apply(&mut doc, cmd, flags, now)?;
            write(&file, &doc).map_err(|e| format!("{}: {e}", file.display()))?;
            Ok(msg)
        }
    }
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

    #[test]
    fn a_session_id_is_a_file_name_not_a_path() {
        let dir = scratch("sid");
        let (cmd, flags) = parse_args(&args("plan a --eta 5")).unwrap();
        assert!(run_in(&dir, "../x", &cmd, &flags, T0).is_err());
        assert!(run_in(&dir, "", &cmd, &flags, T0).is_err());
    }
}

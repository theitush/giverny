//! A session's own background commands left running (giverny#450): asked
//! about once each, at its turn's end or on its next hook call.
//!
//! A dispatcher's Bash command (a `head` with an empty file name, waiting on
//! stdin) hit its 120 s timeout. Claude Code moved it to the background
//! rather than ending it, and tells a session only when a background command
//! *ends*, so nothing looked at it again: for 30 minutes its tab drew the
//! working dots with nothing working. [`crate::leftovers`] covers a
//! subagent's commands (giverny#370); this covers the session's own.
//!
//! - **At the turn's end.** The plugin's `Stop` hook ([`stop`]) holds the
//!   stop once per command, with a question rather than an order — what it
//!   is, how long it has run, "stop it with TaskStop if it is not needed, or
//!   say why it runs on" — for a shell of the session's own still running
//!   that either was moved to the background by its timeout (never meant to
//!   run on), or has been quiet for [`QUIET_MS`]: its output file has not
//!   grown and its processes have used no CPU. Giverny never stops one
//!   itself: a long quiet download or server may be on purpose.
//! - **Between turns.** The session's next hook call — a tool call's
//!   `PostToolUse` or its next prompt ([`between`]) — says the same, once
//!   per command, for any that went quiet since.
//! - **Never** for a command started in the background that is still making
//!   progress, nor for a subagent's ([`crate::leftovers`] asks those), nor
//!   twice for one command. A `Stop` that is already a held stop's second
//!   (`stop_hook_active`) is let through, so the hook never loops.
//!
//! **What is the session's own.** Measured on Claude Code 2.1.302: a main
//! session's `Stop` payload carries `background_tasks`
//! ([`crate::hooks::in_flight`]) from the session's one task registry, each
//! with its `id`, `type`, `status`, `command`, but no owner and no start.
//! The session's transcript (`transcript_path`) tells its own: the tool
//! result that started one carries `toolUseResult.backgroundTaskId` and says
//! `Command running in background with ID: <id>`, or, for one its timeout
//! moved, `Command did not complete within its 3s timeout and was moved to
//! the background (ID: <id>)` with `timedOutAfterMs`. That line's timestamp
//! is its start, and it names the file its output goes to. A subagent's
//! commands start in the subagent's own transcript, so they are not found
//! here.
//!
//! **Quiet** is measured, not guessed: each check records the output file's
//! size and the CPU time of the processes holding that file open (the
//! command's shell writes there, and so does everything it starts) and
//! everything below them; a command is quiet once neither has moved for
//! [`QUIET_MS`]. Both only grow, so two equal readings mean nothing happened
//! between them. Where `/proc` is not there to read, output alone counts.
//! A command no process holds any more has ended.
//!
//! **Only in a Giverny tab** ([`crate::plugin_hook::in_giverny_tab`]), like
//! giverny#370. **Cheap**: a `Stop` with no shell in flight reads no file;
//! another hook call `stat`s one file, and with commands watched looks at
//! them at most once every [`CHECK_EVERY_MS`].
//!
//! **File:** `<feed dir>/leftovers/<session>.own.json`, the commands
//! watched, there only while one is.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::manage::{self, Lock};

/// How long a command must go without output or CPU to be asked about.
pub const QUIET_MS: u64 = 10 * 60_000;

/// How often a session's hook calls between turns look at its commands.
pub const CHECK_EVERY_MS: u64 = 60_000;

/// The task types that are a shell (a monitor is a wait, meant to run on).
const SHELLS: &[&str] = &["shell", "local_bash"];

/// One shell of the session's in flight, from the `Stop` payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: String,
    pub what: String,
}

/// What a main session's `Stop` payload says.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Stop {
    pub session: String,
    pub transcript: Option<PathBuf>,
    /// A held stop's second: let through.
    pub active: bool,
    pub shells: Vec<Task>,
}

/// A main session's `Stop` payload; `None` for any other, a subagent's
/// `SubagentStop` among them (read whole only when it names the event).
pub fn stop_payload(input: &str) -> Option<Stop> {
    if !input.contains("\"Stop\"") {
        return None;
    }
    let v: Value = serde_json::from_str(input).ok()?;
    if v.get("hook_event_name").and_then(Value::as_str) != Some("Stop") {
        return None;
    }
    let text = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    let session = text(&v, "session_id")?;
    let shells = crate::hooks::in_flight(&v)
        .filter(|t| SHELLS.contains(&t.get("type").and_then(Value::as_str).unwrap_or("")))
        .filter_map(|t| {
            let id = text(t, "id")?;
            let what = text(t, "command")
                .or_else(|| text(t, "description"))
                .unwrap_or_else(|| id.clone());
            Some(Task { id, what })
        })
        .collect();
    Some(Stop {
        session,
        transcript: text(&v, "transcript_path").map(PathBuf::from),
        active: v.get("stop_hook_active").and_then(Value::as_bool) == Some(true),
        shells,
    })
}

/// How a command of the session's own started, from its transcript.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Start {
    pub started_ms: Option<u64>,
    /// The file its output goes to.
    pub output: Option<String>,
    /// The timeout that moved it to the background (`3s`, `2m`), when one
    /// did.
    pub timeout: Option<String>,
}

/// Which of `ids` the session started itself, and how: the tool results in
/// its `transcript` that started them. Read line by line; only a line
/// naming one of them is parsed.
pub fn starts(transcript: &Path, ids: &[&str]) -> Vec<(String, Start)> {
    let Ok(file) = std::fs::File::open(transcript) else {
        return Vec::new();
    };
    let mut found: Vec<Option<Start>> = vec![None; ids.len()];
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        if !line.contains("\"tool_result\"") || !line.contains("background") {
            continue;
        }
        for (i, id) in ids.iter().enumerate() {
            if found[i].is_some() || !line.contains(id) {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(start) = started(&v, id) {
                found[i] = Some(start);
            }
        }
    }
    ids.iter()
        .zip(found)
        .filter_map(|(id, s)| Some((id.to_string(), s?)))
        .collect()
}

/// The start of background command `id`, when this transcript line is the
/// tool result that started it.
fn started(line: &Value, id: &str) -> Option<Start> {
    let result = line.get("toolUseResult");
    let by_field = result
        .and_then(|r| r.get("backgroundTaskId"))
        .and_then(Value::as_str)
        == Some(id);
    let text = result_text(line);
    let by_text = text.contains(&format!("Command running in background with ID: {id}"))
        || (text.contains("moved to the background") && text.contains(&format!("ID: {id}")));
    if !by_field && !by_text {
        return None;
    }
    let timed_out = result.and_then(|r| r.get("timedOutAfterMs")).is_some()
        || text.contains("did not complete within its");
    let timeout = timed_out.then(|| {
        between_words(&text, "did not complete within its ", " timeout")
            .map(String::from)
            .or_else(|| {
                let ms = result?.get("timedOutAfterMs")?.as_u64()?;
                Some(span(ms))
            })
            .unwrap_or_else(|| "its".into())
    });
    let output = between_words(&text, "Output is being written to: ", ".output")
        .map(|p| format!("{p}.output"));
    let started_ms = line
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|t| t.parse::<jiff::Timestamp>().ok())
        .map(|t| t.as_millisecond().max(0) as u64);
    Some(Start {
        started_ms,
        output,
        timeout,
    })
}

/// A tool-result line's text: its `content`, a string or text blocks.
fn result_text(line: &Value) -> String {
    let mut out = String::new();
    let blocks = line
        .pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"));
    for b in blocks {
        match b.get("content") {
            Some(Value::String(s)) => out.push_str(s),
            Some(Value::Array(parts)) => {
                for p in parts {
                    if let Some(s) = p.get("text").and_then(Value::as_str) {
                        out.push_str(s);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The text between `from` and the next `to`, on one line.
fn between_words<'a>(text: &'a str, from: &str, to: &str) -> Option<&'a str> {
    let rest = &text[text.find(from)? + from.len()..];
    let end = rest.find(to)?;
    let got = rest[..end].trim();
    (!got.is_empty() && !got.contains('\n')).then_some(got)
}

/// A span as a line says it: `40s`, `12m`, `2h 5m`.
fn span(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ if s % 3600 / 60 == 0 => format!("{}h", s / 3600),
        _ => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}

/// What a look at a command finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Look {
    /// Its output file's size.
    pub size: Option<u64>,
    /// CPU time, clock ticks, of the processes holding its output file open
    /// and everything below them.
    pub cpu: Option<u64>,
    /// Whether any process still holds its output file; `None` where that
    /// cannot be read.
    pub running: Option<bool>,
}

/// How commands are looked at: the system's files and processes, or a
/// test's.
pub trait Probe {
    /// One look at each output file, in order.
    fn look(&self, outputs: &[Option<&str>]) -> Vec<Look>;
}

/// The real look: the file's size, and on Linux `/proc` for the processes
/// that hold it.
pub struct System;

impl Probe for System {
    fn look(&self, outputs: &[Option<&str>]) -> Vec<Look> {
        let sizes: Vec<Option<u64>> = outputs
            .iter()
            .map(|o| std::fs::metadata((*o)?).ok().map(|m| m.len()))
            .collect();
        let cpu = cpu_of(outputs);
        sizes
            .into_iter()
            .zip(cpu)
            .map(|(size, (cpu, running))| Look { size, cpu, running })
            .collect()
    }
}

/// Each output file's (CPU ticks, running): the processes with it open,
/// and everything below them. The hook's own ancestors (the claude that ran
/// it, which may be reading the file) are not the command's.
#[cfg(target_os = "linux")]
fn cpu_of(outputs: &[Option<&str>]) -> Vec<(Option<u64>, Option<bool>)> {
    use std::collections::HashSet;
    let wanted: Vec<Option<PathBuf>> = outputs.iter().map(|o| o.map(PathBuf::from)).collect();
    let mut holders: Vec<Vec<u32>> = vec![Vec::new(); outputs.len()];
    let table = crate::session_use::proc_table(4);
    let me = std::process::id();
    for p in &table {
        let Ok(fds) = std::fs::read_dir(format!("/proc/{}/fd", p.pid)) else {
            continue; // another user's, or gone
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            for (i, w) in wanted.iter().enumerate() {
                if w.as_deref() == Some(target.as_path()) && !holders[i].contains(&p.pid) {
                    holders[i].push(p.pid);
                }
            }
        }
    }
    holders
        .into_iter()
        .map(|pids| {
            let mut tree: HashSet<u32> = HashSet::new();
            for pid in pids {
                let below = crate::session_use::subtree(&table, pid);
                if !below.contains(&me) {
                    tree.extend(below);
                }
            }
            if tree.is_empty() {
                return (None, Some(false));
            }
            let ticks = table
                .iter()
                .filter(|p| tree.contains(&p.pid))
                .map(|p| p.ticks)
                .sum();
            (Some(ticks), Some(true))
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn cpu_of(outputs: &[Option<&str>]) -> Vec<(Option<u64>, Option<bool>)> {
    vec![(None, None); outputs.len()]
}

/// A command watched: how it started, and its last readings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct Cmd {
    id: String,
    what: String,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    started_ms: Option<u64>,
    #[serde(default)]
    timeout: Option<String>,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    cpu: Option<u64>,
    /// When its size or CPU last moved (or it was first looked at).
    #[serde(default)]
    changed_ms: u64,
    #[serde(default)]
    asked: bool,
}

/// A session's commands watched.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Watch {
    #[serde(default)]
    cmds: Vec<Cmd>,
    /// In flight but not the session's own (a subagent's): not read for
    /// again.
    #[serde(default)]
    others: Vec<String>,
    /// When its commands were last looked at.
    #[serde(default)]
    checked_ms: u64,
}

impl Cmd {
    /// Take in a look; whether it found it ended.
    fn take(&mut self, look: Look, now: u64) -> bool {
        if look.running == Some(false) {
            return true;
        }
        if look.size != self.size || look.cpu != self.cpu || self.changed_ms == 0 {
            self.size = look.size;
            self.cpu = look.cpu;
            self.changed_ms = now;
        }
        false
    }

    fn quiet(&self, now: u64) -> bool {
        self.changed_ms > 0 && now.saturating_sub(self.changed_ms) >= QUIET_MS
    }

    /// Whether it is to be asked about now.
    fn due(&self, now: u64) -> bool {
        !self.asked && (self.timeout.is_some() || self.quiet(now))
    }

    /// Its line: what, which task, how long, and why it is asked about.
    fn line(&self, now: u64) -> String {
        let ran = self
            .started_ms
            .map(|t| match now.saturating_sub(t) {
                d if d < 60_000 => ", under a minute in".to_string(),
                d => format!(", {} in", span(d)),
            })
            .unwrap_or_default();
        let why = match &self.timeout {
            Some(t) => format!("moved there by its {t} timeout"),
            None => format!(
                "no output and no CPU for {}",
                span(now.saturating_sub(self.changed_ms))
            ),
        };
        format!("`{}` (task {}{ran}; {why})", quote(&self.what), self.id)
    }
}

/// How much of a command a line quotes.
const QUOTE_CHARS: usize = 60;

/// A command as a line quotes it: its first line, backticks made quotes,
/// cut to [`QUOTE_CHARS`].
fn quote(what: &str) -> String {
    let first = what.lines().next().unwrap_or("").trim().replace('`', "'");
    if first.chars().count() <= QUOTE_CHARS {
        return first;
    }
    let cut: String = first.chars().take(QUOTE_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// The question about `cmds`: one line, or a list.
pub(crate) fn question(lines: &[String]) -> String {
    if let [one] = lines {
        return format!(
            "Giverny: {one} is still running in the background. Stop it with TaskStop if it \
             is not needed, or say why it runs on."
        );
    }
    let list: Vec<String> = lines.iter().map(|l| format!("- {l}")).collect();
    format!(
        "Giverny: these are still running in the background:\n{}\nStop each with TaskStop \
         if it is not needed, or say why it runs on.",
        list.join("\n")
    )
}

fn safe(session: &str) -> bool {
    !session.is_empty()
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `<feed dir>/leftovers/<session>.own.json`: the session's commands
/// watched.
pub fn watch_path(feed_dir: &Path, session: &str) -> PathBuf {
    feed_dir
        .join(crate::leftovers::DIR)
        .join(format!("{session}.own.json"))
}

fn read(path: &Path) -> Watch {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Write the watch, or remove its file when nothing is watched.
fn save(path: &Path, w: &Watch) -> Option<()> {
    if w.cmds.is_empty() {
        let _ = std::fs::remove_file(path);
        return Some(());
    }
    manage::write(path, &json!(w)).ok()
}

/// Look at every command in `w`, dropping those that ended.
fn look_all(w: &mut Watch, probe: &dyn Probe, now: u64) {
    let outputs: Vec<Option<&str>> = w.cmds.iter().map(|c| c.output.as_deref()).collect();
    let looks = probe.look(&outputs);
    let mut i = 0;
    w.cmds.retain_mut(|c| {
        let look = looks.get(i).copied().unwrap_or_default();
        i += 1;
        !c.take(look, now)
    });
    w.checked_ms = now;
}

/// The `Stop` hook's run: the reply holding the stop, or nothing. `in_tab`
/// is asked only when a shell is in flight.
pub fn stop(
    stop: &Stop,
    feed_dir: &Path,
    now: u64,
    probe: &dyn Probe,
    in_tab: impl FnOnce() -> bool,
) -> Option<String> {
    if !safe(&stop.session) {
        return None;
    }
    let file = watch_path(feed_dir, &stop.session);
    if stop.shells.is_empty() {
        // Nothing in flight: nothing to watch.
        if std::fs::metadata(&file).is_ok() {
            let _lock = Lock::take(&file).ok()?;
            let _ = std::fs::remove_file(&file);
        }
        return None;
    }
    if !in_tab() {
        return None;
    }
    std::fs::create_dir_all(file.parent()?).ok()?;
    let _lock = Lock::take(&file).ok()?;
    let mut w = read(&file);
    // What is no longer in flight has ended.
    w.cmds.retain(|c| stop.shells.iter().any(|t| t.id == c.id));
    w.others.retain(|o| stop.shells.iter().any(|t| &t.id == o));
    let new: Vec<&Task> = stop
        .shells
        .iter()
        .filter(|t| !w.cmds.iter().any(|c| c.id == t.id) && !w.others.contains(&t.id))
        .collect();
    if !new.is_empty() {
        let ids: Vec<&str> = new.iter().map(|t| t.id.as_str()).collect();
        let found = stop
            .transcript
            .as_deref()
            .map(|t| starts(t, &ids))
            .unwrap_or_default();
        for t in new {
            match found.iter().find(|(id, _)| *id == t.id) {
                Some((_, s)) => w.cmds.push(Cmd {
                    id: t.id.clone(),
                    what: t.what.clone(),
                    output: s.output.clone(),
                    started_ms: s.started_ms,
                    timeout: s.timeout.clone(),
                    ..Cmd::default()
                }),
                None => w.others.push(t.id.clone()),
            }
        }
    }
    look_all(&mut w, probe, now);
    let mut lines = Vec::new();
    if !stop.active {
        for c in w.cmds.iter_mut().filter(|c| c.due(now)) {
            lines.push(c.line(now));
            c.asked = true;
        }
    }
    // Others alone are kept only while a command of its own is watched.
    save(&file, &w)?;
    (!lines.is_empty()).then(|| block(&question(&lines)))
}

/// The `Stop` reply holding the stop with `reason`.
fn block(reason: &str) -> String {
    json!({"decision": "block", "reason": reason}).to_string()
}

/// A session's own hook call between turns (its `PostToolUse`, its
/// prompt): the line about each command that went quiet since its last
/// `Stop`, once. A `stat` when nothing is watched; a look at most once
/// every [`CHECK_EVERY_MS`].
pub fn between(feed_dir: &Path, session: &str, now: u64, probe: &dyn Probe) -> Option<String> {
    if !safe(session) {
        return None;
    }
    let file = watch_path(feed_dir, session);
    std::fs::metadata(&file).ok()?;
    let _lock = Lock::take(&file).ok()?;
    let mut w = read(&file);
    if now.saturating_sub(w.checked_ms) < CHECK_EVERY_MS || !w.cmds.iter().any(|c| !c.asked) {
        return None;
    }
    look_all(&mut w, probe, now);
    let mut lines = Vec::new();
    for c in w.cmds.iter_mut().filter(|c| c.due(now)) {
        lines.push(c.line(now));
        c.asked = true;
    }
    save(&file, &w)?;
    (!lines.is_empty()).then(|| question(&lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const T0: u64 = 1_790_000_000_000;
    const MIN: u64 = 60_000;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("giverny-stray-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn stamp(ms: u64) -> String {
        jiff::Timestamp::from_millisecond(ms as i64)
            .unwrap()
            .to_string()
    }

    /// One background start as Claude Code 2.1.302 writes it into the
    /// session's transcript: `timeout` for one its timeout moved there.
    fn start_line(id: &str, at: u64, timeout: bool) -> String {
        let out = format!("/tmp/x/s1/tasks/{id}.output");
        let (text, extra) = if timeout {
            (
                format!(
                    "Command did not complete within its 2m timeout and was moved to the \
                     background (ID: {id}). Output is being written to: {out}. You will be \
                     notified when it completes. If it is still running after 10m in the \
                     background, it will be stopped and you will be notified."
                ),
                json!(120_000),
            )
        } else {
            (
                format!(
                    "Command running in background with ID: {id}. Output is being written \
                     to: {out}. You will be notified when it completes. To check interim \
                     output, use Read on that file path."
                ),
                Value::Null,
            )
        };
        let mut result = json!({"stdout": "", "stderr": "", "interrupted": false,
                                "backgroundTaskId": id});
        if !extra.is_null() {
            result["timedOutAfterMs"] = extra;
        }
        json!({"type": "user", "timestamp": stamp(at),
               "message": {"role": "user", "content": [{"tool_use_id": format!("tu_{id}"),
               "type": "tool_result", "content": text, "is_error": false}]},
               "toolUseResult": result})
        .to_string()
    }

    fn transcript(dir: &Path, lines: &[String]) -> PathBuf {
        let t = dir.join("s1.jsonl");
        let head = json!({"type": "user", "timestamp": stamp(T0),
                          "message": {"role": "user", "content": "go; ignore bzz111"}});
        std::fs::write(&t, format!("{head}\n{}\n", lines.join("\n"))).unwrap();
        t
    }

    fn payload(t: &Path, active: bool, tasks: Value) -> Stop {
        let input = json!({"session_id": "s1", "transcript_path": t.display().to_string(),
               "hook_event_name": "Stop", "stop_hook_active": active,
               "last_assistant_message": "done", "background_tasks": tasks,
               "session_crons": []})
        .to_string();
        stop_payload(&input).unwrap()
    }

    fn shell(id: &str, cmd: &str) -> Value {
        json!({"id": id, "type": "shell", "status": "running", "description": cmd,
               "command": cmd})
    }

    /// A probe whose readings a test sets, per output file.
    #[derive(Default)]
    struct Fake(RefCell<Vec<(String, Look)>>);

    impl Fake {
        fn set(&self, id: &str, size: u64, cpu: u64) {
            self.put(
                id,
                Look {
                    size: Some(size),
                    cpu: Some(cpu),
                    running: Some(true),
                },
            );
        }
        fn end(&self, id: &str) {
            self.put(
                id,
                Look {
                    size: None,
                    cpu: None,
                    running: Some(false),
                },
            );
        }
        fn put(&self, id: &str, look: Look) {
            let key = format!("/tmp/x/s1/tasks/{id}.output");
            let mut v = self.0.borrow_mut();
            v.retain(|(k, _)| *k != key);
            v.push((key, look));
        }
    }

    impl Probe for Fake {
        fn look(&self, outputs: &[Option<&str>]) -> Vec<Look> {
            let v = self.0.borrow();
            outputs
                .iter()
                .map(|o| {
                    v.iter()
                        .find(|(k, _)| Some(k.as_str()) == *o)
                        .map(|(_, l)| *l)
                        .unwrap_or_default()
                })
                .collect()
        }
    }

    fn reason(out: &str) -> String {
        let v: Value = serde_json::from_str(out).unwrap();
        assert_eq!(v["decision"], "block");
        v["reason"].as_str().unwrap().to_string()
    }

    #[test]
    fn a_timeout_backgrounded_command_is_asked_about_once_at_stop() {
        let dir = temp("timeout");
        let feeds = dir.join("feeds");
        let t = transcript(&dir, &[start_line("b1", T0 + MIN, true)]);
        let fake = Fake::default();
        fake.set("b1", 0, 3);
        let tasks = json!([shell("b1", "head -c 600 $f")]);
        let s = payload(&t, false, tasks.clone());
        let out = stop(&s, &feeds, T0 + 5 * MIN, &fake, || true).unwrap();
        assert_eq!(
            reason(&out),
            "Giverny: `head -c 600 $f` (task b1, 4m in; moved there by its 2m timeout) is \
             still running in the background. Stop it with TaskStop if it is not needed, or \
             say why it runs on."
        );
        // The held stop's second is let through, and so is every later one.
        assert_eq!(
            stop(
                &payload(&t, true, tasks.clone()),
                &feeds,
                T0 + 6 * MIN,
                &fake,
                || true
            ),
            None
        );
        assert_eq!(
            stop(&s, &feeds, T0 + 30 * MIN, &fake, || true),
            None,
            "never twice"
        );
        assert_eq!(
            between(&feeds, "s1", T0 + 40 * MIN, &fake),
            None,
            "nor between turns"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_quiet_command_is_asked_about_once_and_a_busy_one_never() {
        let dir = temp("quiet");
        let feeds = dir.join("feeds");
        let t = transcript(
            &dir,
            &[start_line("b1", T0, false), start_line("b2", T0, false)],
        );
        let fake = Fake::default();
        fake.set("b1", 10, 5);
        fake.set("b2", 10, 5);
        let tasks = json!([shell("b1", "cat"), shell("b2", "npm run dev")]);
        let s = payload(&t, false, tasks);
        // First looked at: nothing yet.
        assert_eq!(stop(&s, &feeds, T0 + MIN, &fake, || true), None);
        // b2 keeps printing; b1 does not.
        fake.set("b2", 20, 9);
        assert_eq!(stop(&s, &feeds, T0 + 6 * MIN, &fake, || true), None);
        fake.set("b2", 30, 12);
        let out = stop(&s, &feeds, T0 + 12 * MIN, &fake, || true).unwrap();
        assert_eq!(
            reason(&out),
            "Giverny: `cat` (task b1, 12m in; no output and no CPU for 11m) is still running \
             in the background. Stop it with TaskStop if it is not needed, or say why it runs \
             on."
        );
        // Asked once; b2, still busy, never.
        fake.set("b2", 40, 15);
        assert_eq!(stop(&s, &feeds, T0 + 40 * MIN, &fake, || true), None);
        // CPU alone keeps one busy.
        fake.set("b2", 40, 30);
        assert_eq!(stop(&s, &feeds, T0 + 55 * MIN, &fake, || true), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_command_that_goes_quiet_between_turns_is_told_once_on_the_next_call() {
        let dir = temp("between");
        let feeds = dir.join("feeds");
        let t = transcript(&dir, &[start_line("b1", T0, false)]);
        let fake = Fake::default();
        fake.set("b1", 10, 5);
        let s = payload(&t, false, json!([shell("b1", "curl -O big.iso")]));
        assert_eq!(stop(&s, &feeds, T0 + MIN, &fake, || true), None);
        assert!(watch_path(&feeds, "s1").exists());
        // Still downloading.
        fake.set("b1", 900, 40);
        assert_eq!(between(&feeds, "s1", T0 + 8 * MIN, &fake), None);
        // Stalled: not yet ten minutes, then ten.
        assert_eq!(between(&feeds, "s1", T0 + 15 * MIN, &fake), None);
        assert_eq!(
            between(&feeds, "s1", T0 + 18 * MIN, &fake).as_deref(),
            Some(
                "Giverny: `curl -O big.iso` (task b1, 18m in; no output and no CPU for 10m) \
                 is still running in the background. Stop it with TaskStop if it is not \
                 needed, or say why it runs on."
            )
        );
        assert_eq!(between(&feeds, "s1", T0 + 30 * MIN, &fake), None, "once");
        assert_eq!(
            stop(&s, &feeds, T0 + 31 * MIN, &fake, || true),
            None,
            "nor at stop"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn between_turns_looks_at_most_once_a_minute_and_drops_what_ended() {
        let dir = temp("throttle");
        let feeds = dir.join("feeds");
        let t = transcript(&dir, &[start_line("b1", T0, false)]);
        let fake = Fake::default();
        fake.set("b1", 10, 5);
        let s = payload(&t, false, json!([shell("b1", "sleep 900")]));
        assert_eq!(stop(&s, &feeds, T0, &fake, || true), None);
        // Within the minute: not looked at (a look would find it ended).
        fake.end("b1");
        assert_eq!(between(&feeds, "s1", T0 + 30_000, &fake), None);
        assert!(watch_path(&feeds, "s1").exists());
        // Ended: dropped, and the file with it.
        assert_eq!(between(&feeds, "s1", T0 + 2 * MIN, &fake), None);
        assert!(!watch_path(&feeds, "s1").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_held_stops_second_asks_nothing_and_the_next_stop_does() {
        let dir = temp("active");
        let feeds = dir.join("feeds");
        let t = transcript(&dir, &[start_line("b1", T0, true)]);
        let fake = Fake::default();
        fake.set("b1", 0, 0);
        let tasks = json!([shell("b1", "cat")]);
        assert_eq!(
            stop(&payload(&t, true, tasks.clone()), &feeds, T0, &fake, || {
                true
            }),
            None
        );
        assert!(stop(&payload(&t, false, tasks), &feeds, T0 + MIN, &fake, || true).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn several_are_asked_about_in_one_list() {
        let dir = temp("several");
        let feeds = dir.join("feeds");
        let long = "git log --format=%H --since=2.weeks | awk '{print $1}' | sort | uniq -c | cat";
        let t = transcript(
            &dir,
            &[
                start_line("b1", T0, true),
                start_line("b2", T0 + 30_000, true),
            ],
        );
        let fake = Fake::default();
        let s = payload(
            &t,
            false,
            json!([shell("b1", long), shell("b2", "head -c 9 $f")]),
        );
        let out = stop(&s, &feeds, T0 + 40_000, &fake, || true).unwrap();
        assert_eq!(
            reason(&out),
            "Giverny: these are still running in the background:\n\
             - `git log --format=%H --since=2.weeks | awk '{print $1}' | so…` (task b1, under \
             a minute in; moved there by its 2m timeout)\n\
             - `head -c 9 $f` (task b2, under a minute in; moved there by its 2m timeout)\n\
             Stop each with TaskStop if it is not needed, or say why it runs on."
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_subagents_command_and_one_outside_a_tab_are_never_asked_about() {
        let dir = temp("others");
        let feeds = dir.join("feeds");
        // The session's transcript names bzz111 only in its prompt, and
        // never started bsub1 (a subagent did, in its own transcript).
        let t = transcript(&dir, &[]);
        let fake = Fake::default();
        let tasks = json!([shell("bsub1", "sleep 900"), shell("bzz111", "x"),
                           {"id": "a1", "type": "local_agent", "status": "running"},
                           {"id": "m1", "type": "monitor", "status": "running"}]);
        let s = payload(&t, false, tasks);
        assert_eq!(s.shells.len(), 2, "monitors and subagents are not shells");
        for at in [T0, T0 + 30 * MIN] {
            assert_eq!(stop(&s, &feeds, at, &fake, || true), None);
        }
        // Outside a tab: silent, nothing written.
        let t2 = transcript(&dir, &[start_line("b1", T0, true)]);
        let s2 = payload(&t2, false, json!([shell("b1", "cat")]));
        let feeds2 = dir.join("feeds2");
        assert_eq!(stop(&s2, &feeds2, T0, &fake, || false), None);
        assert!(!watch_path(&feeds2, "s1").exists());
        // Nothing in flight: the tab is not even looked up.
        let none = payload(
            &t2,
            false,
            json!([{"id": "b1", "type": "shell", "status": "completed"}]),
        );
        assert_eq!(
            stop(&none, &feeds2, T0, &fake, || panic!("not asked")),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stop_with_nothing_in_flight_clears_the_watch() {
        let dir = temp("clear");
        let feeds = dir.join("feeds");
        let t = transcript(&dir, &[start_line("b1", T0, false)]);
        let fake = Fake::default();
        fake.set("b1", 1, 1);
        let s = payload(&t, false, json!([shell("b1", "sleep 900")]));
        assert_eq!(stop(&s, &feeds, T0, &fake, || true), None);
        assert!(watch_path(&feeds, "s1").exists());
        let done = payload(&t, false, json!([]));
        assert_eq!(
            stop(&done, &feeds, T0 + MIN, &fake, || panic!("not asked")),
            None
        );
        assert!(!watch_path(&feeds, "s1").exists());
        assert_eq!(between(&feeds, "s1", T0 + 20 * MIN, &fake), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_start_is_read_from_the_tool_result_and_nothing_else() {
        let dir = temp("starts");
        let t = transcript(
            &dir,
            &[start_line("b1", T0, true), start_line("b2", T0, false)],
        );
        let found = starts(&t, &["b1", "b2", "bzz111"]);
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0].1,
            Start {
                started_ms: Some(T0),
                output: Some("/tmp/x/s1/tasks/b1.output".into()),
                timeout: Some("2m".into()),
            }
        );
        assert_eq!(found[1].1.timeout, None);
        // The text alone, with no toolUseResult (an older Claude Code).
        let bare = json!({"type": "user", "timestamp": stamp(T0), "message": {"content": [
            {"type": "tool_result", "content": [{"type": "text", "text":
             "Command did not complete within its 3s timeout and was moved to the background \
              (ID: b9). Output is being written to: /t/b9.output."}]}]}});
        assert_eq!(
            started(&bare, "b9"),
            Some(Start {
                started_ms: Some(T0),
                output: Some("/t/b9.output".into()),
                timeout: Some("3s".into()),
            })
        );
        // A later TaskOutput naming it is no start.
        let read = json!({"type": "user", "message": {"content": [{"type": "tool_result",
            "content": "<task_id>b9</task_id> status: running (background)"}]}});
        assert_eq!(started(&read, "b9"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn other_events_are_not_read_as_a_stop() {
        assert_eq!(
            stop_payload(r#"{"hook_event_name":"SubagentStop","session_id":"s1"}"#),
            None
        );
        let tricky = json!({"hook_event_name": "PostToolUse", "session_id": "s1",
                            "tool_response": "\"Stop\""});
        assert_eq!(stop_payload(&tricky.to_string()), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_system_probe_finds_a_live_commands_processes_and_its_end() {
        let dir = temp("probe");
        let out = dir.join("b1.output");
        let f = std::fs::File::create(&out).unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdout(f)
            .spawn()
            .unwrap();
        let path = out.display().to_string();
        let look = System.look(&[Some(&path)])[0];
        assert_eq!(look.running, Some(true));
        assert_eq!(look.size, Some(0));
        assert!(look.cpu.is_some());
        child.kill().unwrap();
        child.wait().unwrap();
        let look = System.look(&[Some(&path), None])[0];
        assert_eq!(look.running, Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

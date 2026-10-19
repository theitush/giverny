//! A subagent's background commands left running as it finishes
//! (giverny#370): asked about once, and told to its parent.
//!
//! A research subagent once reported and finished while a background command
//! of its own (a pipeline with a `cat` waiting on stdin) hung on. Claude Code
//! counted it as the session's background work, so its tab drew the wave
//! with nothing really going on, until the command was stopped by hand.
//!
//! - **The question.** The plugin's `SubagentStop` hook ([`subagent_stop`])
//!   holds a subagent's finish once when a background shell or monitor of
//!   its own still runs, with a question rather than an order: should it
//!   keep running after the subagent reports? A server, a watcher or a
//!   window it meant to leave may well; a forgotten pipeline should not.
//!   The agent is stamped as asked, so its next finish is let through
//!   whatever it decided: its answer stands, and the hook never loops.
//! - **The parent's line.** If the subagent finishes with one still running,
//!   its parent session hears of each such command once, on its next hook
//!   call — a tool call's `PostToolUse` or its next prompt ([`deliver`]). It
//!   is not drawn on the management panel.
//!
//! **What is the subagent's own.** Measured on Claude Code 2.1.296: a
//! `SubagentStop` payload carries `background_tasks` like `Stop` does
//! ([`crate::hooks::in_flight`]), but filled from the session's one task
//! registry: the parent's shells and the subagent's alike, the subagent
//! itself among them, each with its `command` but no owner and no start
//! time. What tells them apart is the subagent's own transcript
//! (`agent_transcript_path`): the tool result that started a background
//! command names its task id (`Command running in background with ID: …`),
//! and its line's timestamp is when it started. Only the subagent's own
//! subagents are not asked about: they are the delegated work giverny#363
//! counts, and they finish on their own. A command Claude Code ends itself
//! at the subagent's final response (its result says so) is no leftover.
//!
//! **Every subagent in a Giverny tab**, managed or not ([`crate::plugin_hook::in_giverny_tab`]);
//! anywhere else the hook says nothing. **Cheap**: only a `SubagentStop`
//! does any work, and one with no command in flight reads no file; a
//! session's other hook calls `stat` one file.
//!
//! **Files**, under `<feed dir>/leftovers/`: `<session>.json`, the subagents
//! asked and the commands told of; `<session>.tell.json`, the lines the
//! parent has yet to hear, there only until it hears them.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::manage::{self, Lock};

/// The directory under the feed dir the files live in.
pub const DIR: &str = "leftovers";

/// How much of a command a line quotes.
const QUOTE_CHARS: usize = 60;

/// What a background command's tool result says when Claude Code itself
/// ends the command at the agent's final response (2.1.296): not a leftover.
const ENDS_WITH_RESPONSE: &str = "terminated when you give your final response";

/// One background command still in flight, from the `SubagentStop` payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: String,
    /// Its command (a shell), or else its description (a monitor).
    pub what: String,
}

/// What a `SubagentStop` payload says.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Stop {
    pub session: String,
    pub agent_id: String,
    /// The subagent's own transcript.
    pub transcript: Option<PathBuf>,
    /// Its spawn description: its own entry in `background_tasks`, else its
    /// `agent-<id>.meta.json` beside the transcript.
    pub description: Option<String>,
    /// The session's background commands in flight, the subagent's or not.
    pub commands: Vec<Task>,
    /// Every id in flight, commands and agents alike: a recorded shell not
    /// among them has ended ([`crate::bg_shells::turn_ended`]).
    pub in_flight: Vec<String>,
}

/// A command of the subagent's own, still running as it finishes.
#[derive(Debug, Clone, PartialEq)]
pub struct Leftover {
    pub task: Task,
    /// When its tool result came back, from the transcript.
    pub started_ms: Option<u64>,
}

/// A `SubagentStop` payload; `None` for any other (read whole only when it
/// names that event).
pub fn stop_payload(input: &str) -> Option<Stop> {
    if !input.contains("\"SubagentStop\"") {
        return None;
    }
    let v: Value = serde_json::from_str(input).ok()?;
    if v.get("hook_event_name").and_then(Value::as_str) != Some("SubagentStop") {
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
    let agent_id = text(&v, "agent_id")?;
    let mut description = None;
    let mut commands = Vec::new();
    let mut in_flight = Vec::new();
    for task in crate::hooks::in_flight(&v) {
        let Some(id) = text(task, "id") else { continue };
        in_flight.push(id.clone());
        if id == agent_id {
            description = text(task, "description");
        }
        let kind = task.get("type").and_then(Value::as_str).unwrap_or("");
        if crate::hooks::COMMANDS.contains(&kind) {
            let what = text(task, "command")
                .or_else(|| text(task, "description"))
                .unwrap_or_else(|| id.clone());
            commands.push(Task { id, what });
        }
    }
    let transcript = text(&v, "agent_transcript_path").map(PathBuf::from);
    if description.is_none()
        && let Some(dir) = transcript.as_deref().and_then(Path::parent)
    {
        description = crate::subagents::read_meta(dir, &agent_id).description;
    }
    Some(Stop {
        session,
        agent_id,
        transcript,
        description,
        commands,
        in_flight,
    })
}

/// Which of `commands` the subagent started itself: those a tool result in
/// its `transcript` names, but for one Claude Code ends with the subagent's
/// final response. Read line by line, and only a line naming one of them is
/// parsed (for its timestamp).
pub fn owned(transcript: &Path, commands: &[Task]) -> Vec<Leftover> {
    let Ok(file) = std::fs::File::open(transcript) else {
        return Vec::new();
    };
    let mut found: Vec<Option<Leftover>> = vec![None; commands.len()];
    let mut ended = vec![false; commands.len()];
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        if !line.contains("\"tool_result\"") {
            continue;
        }
        for (i, task) in commands.iter().enumerate() {
            if found[i].is_some() || ended[i] || !names(&line, &task.id) {
                continue;
            }
            if line.contains(ENDS_WITH_RESPONSE) {
                ended[i] = true;
                continue;
            }
            let started_ms = serde_json::from_str::<Value>(&line)
                .ok()
                .and_then(|v| {
                    v.get("timestamp")?
                        .as_str()?
                        .parse::<jiff::Timestamp>()
                        .ok()
                })
                .map(|t| t.as_millisecond().max(0) as u64);
            found[i] = Some(Leftover {
                task: task.clone(),
                started_ms,
            });
        }
    }
    found.into_iter().flatten().collect()
}

/// Whether `line` names task `id` as a word of its own, not inside a longer
/// id.
fn names(line: &str, id: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    line.match_indices(id).any(|(at, _)| {
        !word(line[..at].chars().next_back()) && !word(line[at + id.len()..].chars().next())
    })
}

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

/// `(started 4m ago)`, or nothing when the start is unknown.
fn since(started_ms: Option<u64>, now: u64) -> String {
    match started_ms {
        Some(t) if now.saturating_sub(t) < 60_000 => " (started just now)".into(),
        Some(t) => format!(" (started {}m ago)", now.saturating_sub(t) / 60_000),
        None => String::new(),
    }
}

/// The question a finishing subagent is held with.
pub fn question(left: &[Leftover], now: u64) -> String {
    if let [one] = left {
        return format!(
            "Giverny: you still have `{}`{} running in the background. Should it keep \
             running after you report? If not, stop it before you finish.",
            quote(&one.task.what),
            since(one.started_ms, now)
        );
    }
    let lines: Vec<String> = left
        .iter()
        .map(|l| format!("- `{}`{}", quote(&l.task.what), since(l.started_ms, now)))
        .collect();
    format!(
        "Giverny: you still have these running in the background:\n{}\nShould they keep \
         running after you report? If not, stop them before you finish.",
        lines.join("\n")
    )
}

/// The parent's line about one command its subagent left running.
pub fn told(description: Option<&str>, task: &Task) -> String {
    let who = description
        .map(|d| format!("your subagent \"{d}\""))
        .unwrap_or_else(|| "your subagent".into());
    format!(
        "Giverny: {who} finished with `{}` still running in the background (task {}).",
        quote(&task.what),
        task.id
    )
}

/// The `SubagentStop` reply holding the finish with `reason`.
fn block(reason: &str) -> String {
    json!({"decision": "block", "reason": reason}).to_string()
}

/// Who has been asked, and which commands told of, in a session.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Seen {
    #[serde(default)]
    asked: Vec<String>,
    #[serde(default)]
    told: Vec<String>,
}

/// A line the parent has yet to hear.
#[derive(Debug, Serialize, Deserialize)]
struct Tell {
    agent: String,
    task: String,
    line: String,
}

fn safe(session: &str) -> bool {
    !session.is_empty()
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `<feed dir>/leftovers/<session>.json`: the subagents asked, the
/// commands told of.
pub fn seen_path(feed_dir: &Path, session: &str) -> PathBuf {
    feed_dir.join(DIR).join(format!("{session}.json"))
}

/// `<feed dir>/leftovers/<session>.tell.json`: the parent's lines to come.
pub fn tell_path(feed_dir: &Path, session: &str) -> PathBuf {
    feed_dir.join(DIR).join(format!("{session}.tell.json"))
}

fn read<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The `SubagentStop` hook's run: the reply holding the finish, or nothing.
/// `in_tab` is asked only when a command is in flight.
pub fn subagent_stop(
    stop: &Stop,
    feed_dir: &Path,
    now: u64,
    in_tab: impl FnOnce() -> bool,
) -> Option<String> {
    if stop.commands.is_empty() || !safe(&stop.session) || !in_tab() {
        return None;
    }
    let left = owned(stop.transcript.as_deref()?, &stop.commands);
    if left.is_empty() {
        return None;
    }
    let seen_file = seen_path(feed_dir, &stop.session);
    std::fs::create_dir_all(seen_file.parent()?).ok()?;
    let _lock = Lock::take(&seen_file).ok()?;
    let mut seen: Seen = read(&seen_file);
    if !seen.asked.contains(&stop.agent_id) {
        seen.asked.push(stop.agent_id.clone());
        manage::write(&seen_file, &json!(seen)).ok()?;
        return Some(block(&question(&left, now)));
    }
    // Asked already: its answer stands. What it kept running, its parent
    // hears of.
    // A task worker's shell is told to its manager with its task
    // ([`crate::bg_shells`], giverny#472), not here as well.
    let new: Vec<&Leftover> = left
        .iter()
        .filter(|l| !seen.told.contains(&l.task.id))
        .filter(|l| !crate::bg_shells::is_recorded(feed_dir, &stop.session, &l.task.id))
        .collect();
    if new.is_empty() {
        return None;
    }
    let tell_file = tell_path(feed_dir, &stop.session);
    let mut tell: Vec<Tell> = read(&tell_file);
    for l in new {
        seen.told.push(l.task.id.clone());
        tell.push(Tell {
            agent: stop.agent_id.clone(),
            task: l.task.id.clone(),
            line: told(stop.description.as_deref(), &l.task),
        });
    }
    manage::write(&tell_file, &json!(tell)).ok()?;
    manage::write(&seen_file, &json!(seen)).ok()?;
    None
}

/// The parent session's lines, on its own hook call, each once: a `stat`
/// when there are none.
pub fn deliver(feed_dir: &Path, session: &str) -> Option<String> {
    if !safe(session) {
        return None;
    }
    let tell_file = tell_path(feed_dir, session);
    std::fs::metadata(&tell_file).ok()?;
    let _lock = Lock::take(&seen_path(feed_dir, session)).ok()?;
    let tell: Vec<Tell> = read(&tell_file);
    let _ = std::fs::remove_file(&tell_file);
    let lines: Vec<String> = tell.into_iter().map(|t| t.line).collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_790_000_000_000;
    const MIN: u64 = 60_000;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("giverny-left-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn stamp(ms: u64) -> String {
        jiff::Timestamp::from_millisecond(ms as i64)
            .unwrap()
            .to_string()
    }

    /// A subagent transcript in which it started `started` (task id,
    /// command, when) in the background, as Claude Code 2.1.296 writes it.
    fn transcript(dir: &Path, agent: &str, started: &[(&str, &str, u64)]) -> PathBuf {
        let sub = dir.join("proj/s1/subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            sub.join(format!("agent-{agent}.meta.json")),
            json!({"agentType": "general-purpose", "description": "Find the loader"}).to_string(),
        )
        .unwrap();
        let mut lines = vec![
            json!({"type": "user", "agentId": agent, "timestamp": stamp(T0),
                   "message": {"role": "user", "content": "Look around; ignore bzz111."}})
            .to_string(),
        ];
        for (id, cmd, at) in started {
            lines.push(
                json!({"type": "assistant", "agentId": agent, "timestamp": stamp(*at),
                       "message": {"content": [{"type": "tool_use", "id": format!("tu_{id}"),
                       "name": "Bash", "input": {"command": cmd, "run_in_background": true}}]}})
                .to_string(),
            );
            lines.push(
                json!({"type": "user", "agentId": agent, "timestamp": stamp(*at + 1000),
                       "message": {"role": "user", "content": [{"tool_use_id": format!("tu_{id}"),
                       "type": "tool_result", "content": format!(
                           "Command running in background with ID: {id}. Output is being \
                            written to: /tmp/x/tasks/{id}.output. You will be notified when \
                            it completes.")}]}})
                .to_string(),
            );
        }
        let t = sub.join(format!("agent-{agent}.jsonl"));
        std::fs::write(&t, lines.join("\n") + "\n").unwrap();
        t
    }

    /// A `SubagentStop` payload as Claude Code 2.1.296 sends it: the
    /// session's whole registry in `background_tasks`.
    fn payload(agent: &str, t: &Path, tasks: Value) -> String {
        json!({"session_id": "s1", "agent_id": agent, "agent_type": "general-purpose",
               "hook_event_name": "SubagentStop", "stop_hook_active": false,
               "transcript_path": "/x/s1.jsonl",
               "agent_transcript_path": t.display().to_string(),
               "background_tasks": tasks, "session_crons": []})
        .to_string()
    }

    fn reason(out: &str) -> String {
        let v: Value = serde_json::from_str(out).unwrap();
        assert_eq!(v["decision"], "block");
        v["reason"].as_str().unwrap().to_string()
    }

    #[test]
    fn a_subagent_with_a_shell_running_is_asked_once_and_its_parent_told_once() {
        let dir = temp("asked");
        let t = transcript(&dir, "a1", &[("bgej3bpqh", "sleep 371", T0 + MIN)]);
        let input = payload(
            "a1",
            &t,
            json!([
                {"id": "a1", "type": "subagent", "status": "running",
                 "description": "Start a sleeper", "agent_type": "general-purpose"},
                {"id": "bgej3bpqh", "type": "shell", "status": "running",
                 "description": "Sleep", "command": "sleep 371"}
            ]),
        );
        let stop = stop_payload(&input).unwrap();
        assert_eq!(stop.description.as_deref(), Some("Start a sleeper"));
        let feeds = dir.join("feeds");
        let out = subagent_stop(&stop, &feeds, T0 + 5 * MIN, || true).unwrap();
        assert_eq!(
            reason(&out),
            "Giverny: you still have `sleep 371` (started 3m ago) running in the background. \
             Should it keep running after you report? If not, stop it before you finish."
        );
        // Nothing for the parent yet: the subagent may stop it.
        assert_eq!(deliver(&feeds, "s1"), None);
        // Its second finish is let through, whatever it decided…
        assert_eq!(subagent_stop(&stop, &feeds, T0 + 6 * MIN, || true), None);
        // …and the parent hears of what it kept, once.
        assert_eq!(
            deliver(&feeds, "s1").as_deref(),
            Some(
                "Giverny: your subagent \"Start a sleeper\" finished with `sleep 371` still \
                 running in the background (task bgej3bpqh)."
            )
        );
        assert_eq!(deliver(&feeds, "s1"), None, "told once");
        assert_eq!(subagent_stop(&stop, &feeds, T0 + 7 * MIN, || true), None);
        assert_eq!(deliver(&feeds, "s1"), None, "once per leftover");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_subagent_that_stopped_its_shell_leaves_its_parent_nothing() {
        let dir = temp("stopped");
        let t = transcript(&dir, "a1", &[("b1", "tail -f log", T0)]);
        let feeds = dir.join("feeds");
        let running = json!([{"id": "b1", "type": "shell", "status": "running",
                              "command": "tail -f log"}]);
        let first = stop_payload(&payload("a1", &t, running)).unwrap();
        assert!(subagent_stop(&first, &feeds, T0, || true).is_some());
        let killed = json!([{"id": "b1", "type": "shell", "status": "killed",
                             "command": "tail -f log"}]);
        let second = stop_payload(&payload("a1", &t, killed)).unwrap();
        assert!(second.commands.is_empty());
        assert_eq!(subagent_stop(&second, &feeds, T0, || true), None);
        assert_eq!(deliver(&feeds, "s1"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_of_its_own_running_lets_a_subagent_finish_silently() {
        let dir = temp("silent");
        let feeds = dir.join("feeds");
        let t = transcript(&dir, "a1", &[("b1", "sleep 9", T0)]);
        let never = |tasks: Value| {
            let stop = stop_payload(&payload("a1", &t, tasks)).unwrap();
            subagent_stop(&stop, &feeds, T0, || panic!("not asked"))
        };
        // No background work, none in flight, only subagents: the tab is
        // not even looked up.
        assert_eq!(never(json!([])), None);
        assert_eq!(
            never(json!([{"id": "b1", "type": "shell", "status": "completed"}])),
            None
        );
        assert_eq!(
            never(
                json!([{"id": "a1", "type": "subagent", "status": "running"},
                         {"id": "a9", "type": "local_agent", "status": "running"}])
            ),
            None
        );
        // The parent's shell, or a task the subagent only read about.
        let only_theirs = |tasks: Value| {
            let stop = stop_payload(&payload("a1", &t, tasks)).unwrap();
            subagent_stop(&stop, &feeds, T0, || true)
        };
        assert_eq!(
            only_theirs(
                json!([{"id": "bparent1", "type": "shell", "status": "running",
                                "command": "npm run dev"}])
            ),
            None
        );
        assert_eq!(
            only_theirs(
                json!([{"id": "bzz111", "type": "shell", "status": "running",
                                "command": "x"}])
            ),
            None,
            "named in its prompt, not started by it"
        );
        // Outside a Giverny tab: silent.
        let stop = stop_payload(&payload(
            "a1",
            &t,
            json!([{"id": "b1", "type": "shell", "status": "running", "command": "sleep 9"}]),
        ))
        .unwrap();
        assert_eq!(subagent_stop(&stop, &feeds, T0, || false), None);
        assert!(!seen_path(&feeds, "s1").exists(), "nothing written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn several_commands_are_listed_one_line_each() {
        let dir = temp("several");
        let long = "git log --format=%H --since=2.weeks | awk '{print $1}' | sort | uniq -c | cat";
        let t = transcript(
            &dir,
            "a2",
            &[
                ("b1", long, T0),
                ("m1", "until grep -q done out; do sleep 2; done", T0),
            ],
        );
        let tasks = json!([
            {"id": "b1", "type": "shell", "status": "running", "command": long},
            {"id": "m1", "type": "monitor", "status": "pending", "description": "wait for done"},
            {"id": "b1x", "type": "shell", "status": "running", "command": "not this"}
        ]);
        let stop = stop_payload(&payload("a2", &t, tasks)).unwrap();
        assert_eq!(
            stop.description.as_deref(),
            Some("Find the loader"),
            "from its meta"
        );
        let out = subagent_stop(&stop, &dir.join("feeds"), T0 + 30_000, || true).unwrap();
        assert_eq!(
            reason(&out),
            "Giverny: you still have these running in the background:\n\
             - `git log --format=%H --since=2.weeks | awk '{print $1}' | so…` (started just now)\n\
             - `wait for done` (started just now)\n\
             Should they keep running after you report? If not, stop them before you finish."
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_command_claude_code_ends_with_the_response_is_no_leftover() {
        let dir = temp("reaped");
        let t = transcript(&dir, "a3", &[]);
        let line = json!({"type": "user", "timestamp": stamp(T0),
            "message": {"content": [{"type": "tool_result", "content":
            "Command running in background with ID: b7. If it exits while you are still working \
             you will be notified, but it is terminated when you give your final response and \
             no notification can follow that."}]}});
        let mut body = std::fs::read_to_string(&t).unwrap();
        body.push_str(&format!("{line}\n"));
        std::fs::write(&t, body).unwrap();
        let tasks = vec![Task {
            id: "b7".into(),
            what: "sleep 5".into(),
        }];
        assert!(owned(&t, &tasks).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn other_events_are_not_read_as_a_subagent_stop() {
        assert_eq!(
            stop_payload(r#"{"hook_event_name":"Stop","session_id":"s1"}"#),
            None
        );
        let tricky = json!({"hook_event_name": "PostToolUse", "session_id": "s1",
                            "agent_id": "a1", "tool_response": "\"SubagentStop\""});
        assert_eq!(stop_payload(&tricky.to_string()), None);
    }
}

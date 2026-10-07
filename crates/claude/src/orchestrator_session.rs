//! `giverny orchestrator-session`: the agents-pane feed's own writer.
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
//! (its `writer` field names someone else, e.g. another orchestrator's) is
//! never touched.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};

use crate::worker_log::{self, WorkerLog};
use crate::{continuation, feed, orchestrator_session_history, resources};

/// Marks the files this writer owns, so it never rewrites another's.
pub const WRITER: &str = "giverny/orchestrator_session";

/// What this writer called itself while an orchestrator session was a
/// "pass": a feed still marked so is its own, and is re-marked on its next
/// write.
pub const OLD_WRITER: &str = "giverny/pass";

/// Claude Code exports the session id into every Bash command it runs, and a
/// subagent inherits its dispatcher's: a worker re-estimating its own row
/// writes into its dispatcher's file.
pub const SESSION_ENV: &str = "CLAUDE_CODE_SESSION_ID";

pub const USAGE: &str = "\
usage: giverny orchestrator-session <command> [args] [--session <id>]

  plan  <task> --eta <dur> [--title T] [--note N] [--brief FILE] [--repo R]
                              queue a task (a Next up row) with its estimate
  start <task> [--eta <dur>] [--title T] [--agent <id>] [--note N] [--brief FILE]
               [--repo R]     the task's worker is starting now (Running)
  eta   <task> <dur left> [--note N] [--why wait|blocked|scope|load|ready]
                              [--title T] [--agent <id>] [--repo R]
                              re-estimate: this much is left from now (a task
                              with no row is started now, Running, with it);
                              --why wait (or blocked) marks the worker waiting
                              until its next eta, and that span is not work;
                              a worker's first eta on a Running row is its
                              re-estimate: taken as given, scored, and told
                              how past re-estimates fared
  land  <task> [--outcome Done|Blocked|...] [--review TEXT] [--note N]
                              the task landed now (Done)
  pause <task> [--note N]     stop the task's clock; `resume <task>` restarts it
  drop  <task>                remove the task's row
  show                        print the rows
  accuracy [--repo R]         how every estimator's figures fared against the
                              time really worked, older tasks against recent
  path                        print the feed file's path
  clear-done                  clear the Done rows: from this session's feed,
                              and from the agents pane of the Giverny tab it runs in
  clear                       delete this session's feed

  claim <task> [--cpu N] [--ram 3G] [--gpu N --vram 8G] [--slot NAME]...
               [--min-ram 2G] [--priority asap|high|medium|low]
                              ask the machine ledger for what the task's worker
                              needs: granted (exit 0), granted smaller (3),
                              queued behind its holders (4; re-run to poll),
                              refused as larger than the limits (5)
  release <task>              give the task's lease back (land and drop do it)
  resources                   capacity, limits, other programs' load, leases
  ask   <task-or-session> \"<msg>\" [--task <yours>] [--priority P]
                              when the ledger is not enough, ask the session
                              holding <task>'s lease (or a session id); the
                              message reaches it on its next tool call
  reply <msg-id> \"<text>\"     answer an ask (or a reply); it reaches the asker
                              the same way
  run   <task> [--cpu N --ram 3G ...] -- <cmd…>
                              run a worker's heavy command under the task's
                              lease: capped by a systemd scope where there is
                              one (else plain, advisory), its slots locked for
                              the command's life, peak memory and CPU time
                              recorded on the row; exit code = the command's

<task> is any short name (`auth-fix`, `#12`); name it, as a whole word, in the
worker's spawn description too. --brief FILE is what the task's row opens to in
the pane: write the worker's prompt (or at least the task's text) to a file and
pass it on every `plan`; `start` fills it in or replaces it. <dur> is minutes (`25`) or `25m`, `1h30m`, `1.5h`.
The session is --session, else $CLAUDE_CODE_SESSION_ID (set inside Claude Code).
The feed goes to $GIVERNY_FEED_DIR, else <config>/giverny/feeds.

Estimates are told, never corrected: the pane counts down from the figure
given. Every landed task appends its estimate, wall time
and working time (wall minus pauses and waits) to history.jsonl beside the feeds
($GIVERNY_ORCHESTRATOR_SESSION_HISTORY overrides; empty turns it off). `plan`/`start --eta N`
print how such guesses have fared: the median working-time/estimate ratio of
the last 20 tasks of the same kind (repo + the title's type word, as `BUG:`),
else the repo, else all. A worker's first `eta` on a running task (its
re-estimate, made after reading the code) is scored and told the same way,
against the working time that was still to come. `accuracy` shows each track. The plugin's
hook (`giverny hook`) asks a worker to re-estimate five minutes into its task.
A subagent that is no task here has an agent ETA instead (`giverny eta`).

Resources: one ledger for every session on the machine, at
<feed dir>/resources/ledger.json ($GIVERNY_LEDGER overrides). Leases expire
20 minutes after their session's last `giverny orchestrator-session` command. Limits are
[orchestrator.limits] in Giverny's config.toml, else auto (cores-2, 70% RAM,
90% of each GPU's VRAM). A slot (`cargo:/path/target`) is held by one lease.
`run` with no lease claims one first (the default lease, [agents_panel.lease]
in config.toml: 3 cpu, 3G unless set; --cpu/--ram override), waits while it is
queued, and releases it when the command ends.
`claim` on a held lease with a smaller --cpu/--ram/--vram shrinks it in place.
Messages go to <feed dir>/inbox/<session>.jsonl; the plugin's hook delivers
them, and renews the calling session's leases, on every tool call.";

/// One `giverny orchestrator-session` command, parsed.
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
    /// Ask the machine ledger for a lease.
    Claim(String),
    Release(String),
    Resources,
    /// Run a command under the task's lease.
    Run(String),
    /// Ask the session holding a lease: target, message.
    Ask(String, String),
    /// Answer a message: its id, the text.
    Reply(String, String),
    /// How each estimator's figures fared, older against recent.
    Accuracy,
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
    /// `claim`: what the worker needs.
    pub cpu: Option<u32>,
    pub ram_mb: Option<u64>,
    pub gpu: Option<u32>,
    pub vram_mb: Option<u64>,
    pub slots: Vec<String>,
    pub min_ram_mb: Option<u64>,
    pub priority: Option<String>,
    /// `run`: the command, everything after `--`.
    pub command: Vec<String>,
    /// `ask`: the asker's own task, when the ledger does not make it plain.
    pub task: Option<String>,
    /// Set by [`run_in`], not parsed: the `--agent` worker's spawn
    /// description, which names the rows a dispatcher started for it before
    /// its id was known.
    pub agent_desc: Option<String>,
    /// The Claude config dir to read spawn descriptions from: tests set it;
    /// else `$CLAUDE_CONFIG_DIR`, `$GIVERNY_PROFILE_DIR`, `~/.claude`.
    pub claude_dir: Option<PathBuf>,
}

impl Flags {
    /// The ledger request these flags make: one core and no RAM unless said.
    pub fn request(&self) -> resources::Request {
        resources::Request {
            cpu: self.cpu.unwrap_or(1),
            ram_mb: self.ram_mb.unwrap_or(0),
            gpu: self.gpu.unwrap_or(0),
            vram_mb: self.vram_mb.unwrap_or(0),
            slots: self.slots.clone(),
            min_ram_mb: self.min_ram_mb,
            priority: self.priority.clone(),
        }
    }
}

fn parse_mem(name: &str, v: &str) -> Result<u64, String> {
    giverny_core::limits::Mem::parse(v)
        .map(|m| m.0)
        .ok_or_else(|| format!("{name}: bad size {v} (e.g. 3G, 512M)"))
}

fn parse_count(name: &str, v: &str) -> Result<u32, String> {
    v.trim()
        .parse()
        .map_err(|_| format!("{name}: not a whole number: {v}"))
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

/// Parse `giverny orchestrator-session …`'s arguments (after
/// `orchestrator-session`).
pub fn parse_args(args: &[String]) -> Result<(Cmd, Flags), String> {
    let mut flags = Flags::default();
    let mut pos: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            flags.command = it.by_ref().cloned().collect();
            break;
        }
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
            "--brief" => {
                // The pane reads it from its own directory: keep it absolute.
                let v = val("--brief")?;
                flags.brief = Some(
                    std::path::absolute(&v)
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or(v),
                );
            }
            "--agent" => flags.agent = Some(val("--agent")?),
            "--outcome" => flags.outcome = Some(val("--outcome")?),
            "--review" => flags.review = Some(val("--review")?),
            "--session" => flags.session = Some(val("--session")?),
            "--why" => flags.why = Some(val("--why")?),
            "--repo" => flags.repo = Some(val("--repo")?),
            "--cpu" => flags.cpu = Some(parse_count("--cpu", &val("--cpu")?)?),
            "--gpu" => flags.gpu = Some(parse_count("--gpu", &val("--gpu")?)?),
            "--ram" => flags.ram_mb = Some(parse_mem("--ram", &val("--ram")?)?),
            "--vram" => flags.vram_mb = Some(parse_mem("--vram", &val("--vram")?)?),
            "--min-ram" => flags.min_ram_mb = Some(parse_mem("--min-ram", &val("--min-ram")?)?),
            "--slot" => flags.slots.push(val("--slot")?),
            "--priority" => flags.priority = Some(val("--priority")?),
            "--task" => flags.task = Some(val("--task")?),
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
                .ok_or("`eta` needs the time left, e.g. `giverny orchestrator-session eta auth-fix 20`")?;
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
        "claim" => Cmd::Claim(task()?),
        "release" => Cmd::Release(task()?),
        "resources" => Cmd::Resources,
        "accuracy" => Cmd::Accuracy,
        "run" => Cmd::Run(task()?),
        "ask" | "reply" => {
            let what = task()?;
            let text = pos.by_ref().collect::<Vec<_>>().join(" ");
            if text.trim().is_empty() {
                return Err(format!(
                    "`{verb}` needs the message, e.g. `giverny orchestrator-session {verb} {what} \"…\"`"
                ));
            }
            if verb == "ask" {
                Cmd::Ask(what, text)
            } else {
                Cmd::Reply(what, text)
            }
        }
        other => return Err(format!("unknown command {other}\n\n{USAGE}")),
    };
    if matches!(cmd, Cmd::Plan(_)) && flags.eta_s.is_none() {
        return Err("`plan` needs --eta: the pane's Next up rows show it".into());
    }
    if matches!(cmd, Cmd::Run(_)) && flags.command.is_empty() {
        return Err(format!(
            "`run` needs a command after `--`, e.g. `giverny orchestrator-session run t -- cargo test`\n\n{USAGE}"
        ));
    }
    if flags.gpu.unwrap_or(0) > 0 && flags.vram_mb.is_none() {
        return Err("`--gpu` needs --vram: how much of each GPU the worker needs".into());
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

/// Who wrote this document, when it says; [`OLD_WRITER`] reads as
/// [`WRITER`].
pub fn writer_of(doc: &Value) -> Option<&str> {
    doc.get("writer")
        .and_then(Value::as_str)
        .map(|w| if w == OLD_WRITER { WRITER } else { w })
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
/// added to `paused_s`; `spawned` keeps the true start.
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
/// the history takes off the task's working time. The pane's
/// clock is not moved: a wait is still wall time.
fn close_wait(row: &mut Map<String, Value>, now: u64) {
    let Some(since) = ms_of(row, "waiting_since") else {
        return;
    };
    row.remove("waiting_since");
    let total = u64_of(row, "wait_s").unwrap_or(0) + now.saturating_sub(since) / 1000;
    row.insert("wait_s".into(), json!(total));
}

/// Working time so far on a Running row, in seconds: since `started`
/// (already moved on by closed pauses) up to an open pause, less its closed
/// and open waiting spans.
fn worked_s(row: &Map<String, Value>, now: u64) -> u64 {
    let Some(started) = ms_of(row, "started") else {
        return 0;
    };
    let upto = ms_of(row, "paused_since").unwrap_or(now);
    let open_wait = ms_of(row, "waiting_since").map_or(0, |w| upto.saturating_sub(w) / 1000);
    (upto.saturating_sub(started) / 1000)
        .saturating_sub(u64_of(row, "wait_s").unwrap_or(0) + open_wait)
}

/// A new estimate on a row drops what a row from before giverny#229 kept
/// beside its corrected `eta_s`: the raw guess and where the correction came
/// from. `eta_s` is the guess now.
fn drop_old_guess(row: &mut Map<String, Value>, f: &Flags) {
    if f.eta_s.is_some() {
        row.remove("eta_guess_s");
        row.remove("eta_basis");
    }
}

/// `start <task> --agent <worker>` on a worker already running another
/// task: the dispatcher has handed it the next one, so the
/// worker's earlier Running rows land now, each with its own measured span,
/// and the pane gives the new task its own clock and its own tokens. A row
/// started less than [`feed::LATER_TASK_MS`] before is not an earlier task
/// but one of a batch the worker was spawned with (`Work #144 #145`), and
/// keeps running. Returns the keys landed.
///
/// The worker's rows are those carrying its `agent_id`, and — since a
/// dispatcher may `start` a task before the spawn tells it the id — those
/// with no `agent_id` whose key its spawn description (`desc`) names, the
/// join the pane makes. A landed row loses its `lease`: the caller gives it
/// back to the ledger.
fn hand_off(
    rows: &mut [Value],
    new: usize,
    agent: &str,
    desc: Option<&str>,
    now: u64,
) -> Vec<String> {
    let mut landed = Vec::new();
    for (i, row) in rows.iter_mut().enumerate() {
        let Some(row) = row.as_object_mut() else {
            continue;
        };
        let key = row.get("key").and_then(Value::as_str).unwrap_or("");
        let theirs = match row.get("agent_id").and_then(Value::as_str) {
            Some(a) => a == agent,
            None => desc.is_some_and(|d| feed::names_key(d, key)),
        };
        let earlier = i != new
            && theirs
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
        row.entry("agent_id").or_insert(json!(agent));
        row.remove("lease");
        if let Some(k) = row.get("key").and_then(Value::as_str) {
            landed.push(k.to_string());
        }
    }
    landed
}

/// Whether a `start --agent` could hand off a row the agent id alone does
/// not find: a Running row with no `agent_id`, old enough to be an earlier
/// task. Only then is the worker's spawn description looked up.
fn needs_description(doc: &Value, now: u64) -> bool {
    doc.get("rows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .any(|r| {
            !r.contains_key("agent_id")
                && stage_of(r) == Some(feed::Stage::Running)
                && ms_of(r, "started").is_some_and(|s| s.saturating_add(feed::LATER_TASK_MS) <= now)
        })
}

/// `session`'s `subagents/` dir, under the Claude config dir it runs in.
fn session_subagents(cfg: Option<&Path>, session: &str) -> Option<PathBuf> {
    let cfg = cfg
        .map(Path::to_path_buf)
        .or_else(continuation::config_dir)?;
    crate::subagents::subagents_dir(&cfg, session)
}

/// The transcript of subagent `agent`, read whole: under the first of
/// `sessions` (the conversation's ids, now and before a re-id) that has it.
fn worker_log(cfg: Option<&Path>, sessions: &[String], agent: &str) -> Option<WorkerLog> {
    sessions.iter().find_map(|session| {
        let dir = session_subagents(cfg, session)?;
        let path = crate::subagents::agent_transcript(&dir, agent);
        if !path.exists() {
            return None;
        }
        let mut log = WorkerLog::new(path);
        log.poll().then_some(log)
    })
}

/// Freeze what each row that just landed spent, when its worker held other
/// tasks before or after it: `task_tokens`, its worker's turns from the
/// row's start (the worker's first turn, for its first task) to its landing.
/// The pane draws that figure for the Done row from then on, whatever the
/// worker does next. A worker that held one task keeps its own count, and
/// a row whose transcript cannot be read is left for the pane to split.
/// A worker's task that landed before it was handed the next (`land`, then
/// `start <next> --agent <worker>`) is frozen at that hand-off: `handed` is
/// that worker.
fn freeze_tokens(
    doc: &mut Value,
    before: &[String],
    handed: Option<&str>,
    log: impl Fn(&str) -> Option<WorkerLog>,
) {
    let Some(rows) = doc.get_mut("rows").and_then(Value::as_array_mut) else {
        return;
    };
    let began = |r: &Map<String, Value>| ms_of(r, "spawned").or_else(|| ms_of(r, "started"));
    let starts: Vec<(String, u64, Option<u64>)> = rows
        .iter()
        .filter_map(Value::as_object)
        .filter(|r| stage_of(r) != Some(feed::Stage::Planned))
        .filter_map(|r| {
            let agent = r.get("agent_id")?.as_str()?.to_string();
            Some((agent, began(r)?, ms_of(r, "ended")))
        })
        .collect();
    for row in rows.iter_mut().filter_map(Value::as_object_mut) {
        let key = row.get("key").and_then(Value::as_str).unwrap_or("");
        let agent_now = handed.is_some() && row.get("agent_id").and_then(Value::as_str) == handed;
        if stage_of(row) != Some(feed::Stage::Done)
            || (before.iter().any(|k| k == key) && !agent_now)
            || row.contains_key("task_tokens")
        {
            continue;
        }
        let (Some(agent), Some(start), Some(end)) = (
            row.get("agent_id").and_then(Value::as_str),
            began(row),
            ms_of(row, "ended"),
        ) else {
            continue;
        };
        let other = |far: &dyn Fn(u64, Option<u64>) -> bool| {
            starts.iter().any(|(a, s, e)| a == agent && far(*s, *e))
        };
        // Another of its tasks, before or after: one begun well apart, or
        // one that had landed before the other began, however short.
        let earlier = other(&|s, e| {
            s.saturating_add(feed::LATER_TASK_MS) <= start
                || (s < start && e.is_some_and(|e| e <= start))
        });
        let later = other(&|s, _| {
            start.saturating_add(feed::LATER_TASK_MS) <= s || (start < s && end <= s)
        });
        if !earlier && !later {
            continue;
        }
        let Some(w) = log(agent) else { continue };
        let from = if earlier { start } else { 0 };
        row.insert("task_tokens".into(), json!(w.added(from, Some(end))));
    }
}

/// Whether any Running row has no worker: only then can a dispatcher's
/// message have handed one over ([`link_handoffs`]).
fn has_unworkered(doc: &Value) -> bool {
    doc.get("rows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .any(|r| !r.contains_key("agent_id") && stage_of(r) == Some(feed::Stage::Running))
}

/// The hand-offs a dispatcher made by message alone (giverny#217): a plain
/// `start <task>`, then a `SendMessage` to a worker of this session naming the
/// task in any wording. Each Running row with no worker that such a message
/// hands over ([`feed::handed_by`]) gets that worker's `agent_id`, as
/// `start <task> --agent <worker>` would have given it, so its tokens are
/// counted from its own start and frozen when it lands. The workers looked
/// at are those the feed already names (a reused worker is one), their
/// messages taken in the order sent. A message counts only when the worker
/// was idle (every task it held before had landed by then) or it says it is
/// a new task, which also lands the worker's earlier Running rows at that
/// message, as `start --agent` does. Returns `(agent, keys)` linked.
fn link_handoffs(
    doc: &mut Value,
    log: impl Fn(&str) -> Option<WorkerLog>,
) -> Vec<(String, Vec<String>)> {
    if !has_unworkered(doc) {
        return Vec::new();
    }
    let Some(rows) = doc.get_mut("rows").and_then(Value::as_array_mut) else {
        return Vec::new();
    };
    let mut agents: Vec<String> = Vec::new();
    for r in rows.iter().filter_map(Value::as_object) {
        if let Some(a) = r.get("agent_id").and_then(Value::as_str)
            && !a.is_empty()
            && !agents.iter().any(|x| x == a)
        {
            agents.push(a.to_string());
        }
    }
    let mut sent: Vec<(String, worker_log::Message)> = Vec::new();
    for a in &agents {
        if let Some(w) = log(a) {
            sent.extend(w.messages().iter().map(|m| (a.clone(), m.clone())));
        }
    }
    sent.sort_by_key(|(_, m)| m.at_ms);
    let mut linked: Vec<(String, Vec<String>)> = Vec::new();
    for (agent, m) in &sent {
        let at = m.at_ms;
        let mine = |r: &Map<String, Value>| {
            r.get("agent_id").and_then(Value::as_str) == Some(agent)
                && stage_of(r) != Some(feed::Stage::Planned)
        };
        let idle = rows
            .iter()
            .filter_map(Value::as_object)
            .filter(|r| mine(r) && ms_of(r, "started").is_some_and(|s| s < at))
            .all(|r| ms_of(r, "ended").is_some_and(|e| e <= at));
        if !idle && !m.new_task {
            continue;
        }
        let waiting: Vec<(usize, String, Option<u64>)> = rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                let r = r.as_object()?;
                let key = r.get("key").and_then(Value::as_str)?;
                (!r.contains_key("agent_id")
                    && stage_of(r) == Some(feed::Stage::Running)
                    && !key.is_empty())
                .then(|| (i, key.to_string(), ms_of(r, "started")))
            })
            .collect();
        let slots: Vec<feed::Waiting> = waiting
            .iter()
            .map(|(_, key, started)| feed::Waiting {
                key,
                started_ms: *started,
            })
            .collect();
        let picks = feed::handed_by(m, &slots, idle);
        if picks.is_empty() {
            continue;
        }
        for &p in &picks {
            let (i, key) = (waiting[p].0, &waiting[p].1);
            if !idle {
                hand_off(rows, i, agent, None, at);
            }
            if let Some(r) = rows[i].as_object_mut() {
                r.insert("agent_id".into(), json!(agent));
            }
            match linked.iter_mut().find(|(a, _)| a == agent) {
                Some((_, keys)) => keys.push(key.clone()),
                None => linked.push((agent.clone(), vec![key.clone()])),
            }
        }
    }
    linked
}

/// `start <task>` with no `--agent`: the workers of this pass that are
/// idle now — every row they hold has landed, the last within
/// [`IDLE_HINT_MS`] — newest landing first, with that row's key. One of them
/// may be about to be handed `task`.
fn idle_workers(doc: &Value, task: &str, now: u64) -> Vec<(String, String)> {
    let rows: Vec<&Map<String, Value>> = doc
        .get("rows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .collect();
    let mut idle: Vec<(u64, String, String)> = Vec::new();
    for r in &rows {
        let Some(a) = r.get("agent_id").and_then(Value::as_str) else {
            continue;
        };
        if a.is_empty() || idle.iter().any(|(_, x, _)| x == a) {
            continue;
        }
        let theirs: Vec<&&Map<String, Value>> = rows
            .iter()
            .filter(|r| r.get("agent_id").and_then(Value::as_str) == Some(a))
            .filter(|r| r.get("key").and_then(Value::as_str) != Some(task))
            .collect();
        if theirs
            .iter()
            .any(|r| stage_of(r) != Some(feed::Stage::Done))
        {
            continue;
        }
        let last = theirs
            .iter()
            .filter_map(|r| Some((ms_of(r, "ended")?, r.get("key")?.as_str()?)))
            .max_by_key(|(e, _)| *e);
        if let Some((ended, key)) = last
            && now.saturating_sub(ended) <= IDLE_HINT_MS
        {
            idle.push((ended, a.to_string(), key.to_string()));
        }
    }
    idle.sort_by(|x, y| y.0.cmp(&x.0).then_with(|| x.1.cmp(&y.1)));
    idle.into_iter().map(|(_, a, k)| (a, k)).collect()
}

/// How lately a worker's last task must have landed for `start` with no
/// `--agent` to name it as idle.
const IDLE_HINT_MS: u64 = 30 * 60 * 1000;

/// The line `start <task>` with no `--agent` adds when a worker of this
/// orchestrator session is idle: hand it over with `--agent`, so its tokens are its own.
fn idle_hint(task: &str, idle: &[(String, String)]) -> Option<String> {
    let (agent, key) = idle.first()?;
    let more = match idle.len() {
        1 => String::new(),
        n => format!(" ({} more idle)", n - 1),
    };
    Some(format!(
        "  {agent} is idle since {key} landed{more}: if it takes {task}, start it with \
         `--agent {agent}` so {task} counts its own tokens (a SendMessage to it naming \
         {task} links it too)"
    ))
}

/// The spawn description of `session`'s subagent `agent`, from its
/// `agent-<id>.meta.json` under the Claude config dir the session runs in.
fn agent_description(cfg: Option<&Path>, sessions: &[String], agent: &str) -> Option<String> {
    sessions.iter().find_map(|session| {
        let dir = session_subagents(cfg, session)?;
        crate::subagents::read_meta(&dir, agent).description
    })
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
        Cmd::Show
        | Cmd::Path
        | Cmd::Clear
        | Cmd::ClearDone
        | Cmd::Nudge
        | Cmd::Claim(_)
        | Cmd::Release(_)
        | Cmd::Resources
        | Cmd::Accuracy
        | Cmd::Run(_)
        | Cmd::Ask(..)
        | Cmd::Reply(..) => {
            return Ok(String::new());
        }
    };
    let at = find(rows, &key);
    let stage = at.and_then(|i| rows[i].as_object().and_then(stage_of));
    let missing = || {
        format!(
            "no task `{key}` in this orchestrator session (`giverny orchestrator-session show` lists them)"
        )
    };

    match cmd {
        Cmd::Plan(_) => {
            if let Some(s) = stage.filter(|s| *s != feed::Stage::Planned) {
                return Err(format!(
                    "`{key}` is already {}; `giverny orchestrator-session eta` re-estimates it",
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
            drop_old_guess(row, f);
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
            drop_old_guess(row, f);
            set_str(row, "repo", &f.repo);
            set_str(row, "title", &f.title);
            set_str(row, "agent_id", &f.agent);
            set_str(row, "note", &f.note);
            set_str(row, "brief", &f.brief);
            // A dispatcher's `start` owns the row: it lands it.
            row.remove("follows_worker");
            let handed = match &f.agent {
                Some(agent) => hand_off(rows, i, agent, f.agent_desc.as_deref(), now),
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
            // A worker spawned outside an orchestrator session has no row
            // for `eta` to re-estimate, and an error would teach nothing:
            // start one now, with what is left as its estimate. The time
            // the worker spent before this is not known, so the clock starts
            // here; and the figure is taken as given, as every figure is.
            let mut sf = f.clone();
            sf.eta_s = Some(*left);
            apply(doc, &Cmd::Start(key.clone()), &sf, now)?;
            let rows = rows_mut(doc)?;
            if let Some(row) = find(rows, &key).and_then(|i| rows[i].as_object_mut()) {
                // Started from `eta`, by a dispatcher that forgot `start`
                // (or a worker of giverny#158's first round): none may
                // ever land it, so the pane ends it with its worker.
                row.insert("follows_worker".into(), json!(true));
                if is_wait(f.why.as_deref()) {
                    row.insert("waiting_since".into(), json!(stamp(now)));
                }
            }
            Ok(format!(
                "{key}: no row in this orchestrator session, so started it now with ~{} left \
                 (as given; `giverny orchestrator-session start {key} --eta <min> --agent <id>` \
                 before the spawn gives a row its whole time)",
                feed::fmt_span(*left as i64)
            ))
        }
        Cmd::Eta(_, left) => {
            let i = at.ok_or_else(missing)?;
            let row = rows[i].as_object_mut().ok_or("row is not an object")?;
            if stage == Some(feed::Stage::Running) && !row.contains_key("reest_s") {
                // The worker's first re-estimate: kept, with
                // when it was made in working time, to be scored at landing.
                row.insert("reest_s".into(), json!(left));
                row.insert("reest_at_s".into(), json!(worked_s(row, now)));
            }
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
            // `run_in` gives the lease back to the ledger.
            row.remove("lease");
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
        Cmd::Show
        | Cmd::Path
        | Cmd::Clear
        | Cmd::ClearDone
        | Cmd::Nudge
        | Cmd::Claim(_)
        | Cmd::Release(_)
        | Cmd::Resources
        | Cmd::Accuracy
        | Cmd::Run(_)
        | Cmd::Ask(..)
        | Cmd::Reply(..) => unreachable!(),
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

/// A lock beside the feed, so concurrent `giverny orchestrator-session` runs (a dispatcher
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

/// The file this session's orchestrator session lives in: `<session>.json`, or an existing
/// file that names the session as an alias.
pub fn file_for(dir: &Path, session: &str) -> PathBuf {
    feed::find(dir, session)
        .map(|(p, _)| p)
        .unwrap_or_else(|| feed::feed_path(dir, session))
}

/// A session no feed names yet takes its conversation's feed under an
/// earlier id, when this writer wrote it: Claude Code re-ids a session on a
/// resume, the agents view's switch and the move into a background host
/// ([`continuation`]). The file keeps its name; the new id becomes its
/// `session` and the old one an alias, so the pane, the hook and workers
/// still holding the old id all find it.
fn adopt_continued(dir: &Path, session: &str, cfg: Option<&Path>) {
    if feed::find(dir, session).is_some() {
        return;
    }
    let Some(root) = continuation::root_of(cfg, session) else {
        return;
    };
    let mine = |d: &Value| writer_of(d) == Some(WRITER);
    let Some(file) = continuation::continued(dir, session, &root, cfg, mine) else {
        return;
    };
    let Ok(_lock) = Lock::take(&file) else {
        return;
    };
    let Some(mut doc) = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(|d| d.is_object() && mine(d))
    else {
        return;
    };
    continuation::adopt(&mut doc, session);
    continuation::stamp_root(&mut doc, Some(&root));
    if let Err(e) = write(&file, &doc) {
        eprintln!("giverny orchestrator-session: {}: {e}", file.display());
    }
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
    run_in_code(dir, session, cmd, flags, now, None).map(|(msg, _)| msg)
}

/// [`run_in`], with the exit code (`claim` has several, [`resources::exit`])
/// and, for tests, the machine to claim against (else this one, detected).
pub fn run_in_code(
    dir: &Path,
    session: &str,
    cmd: &Cmd,
    flags: &Flags,
    now: u64,
    cap: Option<&resources::Capacity>,
) -> Result<(String, i32), String> {
    if session.is_empty() || session.contains(['/', '\\']) || session.starts_with('.') {
        return Err(format!("not a session id: {session:?}"));
    }
    adopt_continued(dir, session, flags.claude_dir.as_deref());
    let ledger = resources::ledger_path(dir);
    // Every command from a session keeps its leases alive.
    if *cmd != Cmd::Path
        && let Err(e) = resources::heartbeat(&ledger, Some(dir), session, now)
    {
        eprintln!("giverny orchestrator-session: {e}");
    }
    match cmd {
        Cmd::Claim(task) => return claim(dir, &ledger, session, task, flags, now, cap),
        Cmd::Run(task) => {
            return crate::orchestrator_session_run::run(dir, &ledger, session, task, flags, cap);
        }
        Cmd::Release(task) => {
            let gone = resources::release_at(&ledger, session, task, now)?;
            resources::annotate_row(dir, session, task, None);
            return Ok((
                match gone {
                    Some(l) => format!("released {task}: {}", l.describe()),
                    None => format!("{task} held no lease"),
                },
                0,
            ));
        }
        Cmd::Ask(target, text) => {
            let msg = crate::orchestrator_session_inbox::ask(
                dir,
                &ledger,
                session,
                target,
                text,
                flags.task.as_deref(),
                flags.priority.as_deref(),
                now,
            )?;
            return Ok((msg, 0));
        }
        Cmd::Reply(id, text) => {
            let msg =
                crate::orchestrator_session_inbox::reply(dir, &ledger, session, id, text, now)?;
            return Ok((msg, 0));
        }
        Cmd::Accuracy => return Ok((accuracy(dir, flags.repo.as_deref()), 0)),
        Cmd::Resources => {
            let cap = match cap {
                Some(c) => c.clone(),
                None => resources::Capacity::detect()?,
            };
            let eta = |s: &str, t: &str| resources::eta_left_s(dir, s, t, now);
            let out =
                resources::with_ledger(&ledger, now, |l| resources::report(l, &cap, now, &eta))?;
            return Ok((out, 0));
        }
        _ => {}
    }
    let (mut msg, landed) = match run_feed(dir, session, cmd, flags, now) {
        Ok(x) => x,
        // A task that is over gives its lease back even when its row could
        // not be changed (none in this orchestrator session, another writer's feed).
        Err(e) => {
            if let Cmd::Land(task) | Cmd::Drop(task) = cmd
                && let Some(l) = resources::release_at(&ledger, session, task, now)?
            {
                return Err(format!("{e}; released {}", l.describe()));
            }
            return Err(e);
        }
    };
    // A task that lands — by `land`, or handed off by a `start --agent` — or
    // is dropped gives its lease back, after the feed's lock is let go (the
    // ledger's is never taken under it).
    let mut free = landed;
    if let Cmd::Land(task) | Cmd::Drop(task) = cmd
        && !free.contains(task)
    {
        free.push(task.clone());
    }
    for task in &free {
        if let Some(l) = resources::release_at(&ledger, session, task, now)? {
            msg = if free.len() > 1 {
                format!("{msg}; released {task}: {}", l.describe())
            } else {
                format!("{msg}; released {}", l.describe())
            };
        }
    }
    Ok((msg, 0))
}

/// `claim`: ask the ledger, and copy the answer onto the task's feed row.
fn claim(
    dir: &Path,
    ledger: &Path,
    session: &str,
    task: &str,
    flags: &Flags,
    now: u64,
    cap: Option<&resources::Capacity>,
) -> Result<(String, i32), String> {
    let cap = match cap {
        Some(c) => c.clone(),
        None => resources::Capacity::detect()?,
    };
    let req = flags.request();
    let repo = flags.repo.clone().or_else(|| {
        let cwd = std::env::current_dir().unwrap_or_default();
        orchestrator_session_history::repo_of(task, &cwd)
    });
    // Figures given on a held lease, each no larger, shrink it in place: the
    // answer to an ask. Otherwise it is a claim as ever.
    let (out, shrunk) = resources::with_ledger(ledger, now, |l| {
        if let Some(s) = l.shrink(session, task, flags.cpu, flags.ram_mb, flags.vram_mb, now) {
            l.heartbeat(session, now);
            return (resources::Outcome::Held(s.1.clone()), Some(s.0));
        }
        (
            l.claim(&cap, session, task, repo.as_deref(), &req, now),
            None,
        )
    })?;
    resources::annotate_row(dir, session, task, resources::row_lease(&out, &req));
    let eta = |s: &str, t: &str| resources::eta_left_s(dir, s, t, now);
    if let (Some(was), resources::Outcome::Held(l)) = (&shrunk, &out) {
        return Ok((
            format!("shrunk {task}: {} (was {})", l.describe(), was.describe()),
            resources::exit::GRANTED,
        ));
    }
    let mut line = resources::outcome_line(task, &out, &eta);
    if let resources::Outcome::Held(l) = &out {
        let grow = flags.cpu.is_some_and(|c| c > l.cpu)
            || flags.ram_mb.is_some_and(|r| r > l.ram_mb)
            || flags
                .vram_mb
                .is_some_and(|v| v > l.vram_mb && !l.gpus.is_empty());
        if grow {
            line.push_str("; a held lease is not grown: release it and claim again");
        }
    }
    if let resources::Outcome::Queued { blockers, .. } = &out
        && let Some(hint) = resources::ask_hint(
            task,
            blockers,
            resources::eta_or_estimate_s(dir, session, task, now),
            &eta,
        )
    {
        line.push_str(&format!("; {hint}"));
    }
    if !matches!(out, resources::Outcome::Refused(_))
        && let Some(hint) = size_hint(dir, session, task, repo.as_deref())
    {
        line.push_str(&format!(" ({hint})"));
    }
    Ok((line, out.exit_code()))
}

/// What past tasks like this one peaked at under `giverny orchestrator-session run`,
/// for `claim` to say beside its answer: `the last 4 BUG
/// tasks in demo peaked at 1.8G (median), 2.6G at most`.
fn size_hint(dir: &Path, session: &str, task: &str, repo: Option<&str>) -> Option<String> {
    let history = orchestrator_session_history::path(dir)?;
    let title = feed::find(dir, session).and_then(|(_, f)| {
        f.rows
            .into_iter()
            .find(|r| r.key == task)
            .and_then(|r| r.title)
    });
    let kind = title
        .as_deref()
        .and_then(orchestrator_session_history::kind_of);
    orchestrator_session_history::peak_hint(
        &orchestrator_session_history::load(&history),
        repo,
        kind.as_deref(),
    )
}

/// Change `session`'s feed row for `task` under the feed's lock, when the
/// feed is this writer's and has the row. False when nothing was changed.
pub(crate) fn edit_row(
    dir: &Path,
    session: &str,
    task: &str,
    f: impl FnOnce(&mut Map<String, Value>),
) -> bool {
    let file = file_for(dir, session);
    if !file.exists() {
        return false;
    }
    let Ok(_lock) = Lock::take(&file) else {
        return false;
    };
    let Some(mut doc) = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return false;
    };
    if writer_of(&doc).is_some_and(|w| w != WRITER) {
        return false;
    }
    let Some(row) = doc
        .get_mut("rows")
        .and_then(Value::as_array_mut)
        .and_then(|rows| {
            rows.iter_mut()
                .find(|r| r.get("key").and_then(Value::as_str) == Some(task))
        })
        .and_then(Value::as_object_mut)
    else {
        return false;
    };
    f(row);
    write(&file, &doc).is_ok()
}

/// Open (`Some(note)`) or close (`None`) a waiting span on a Running row,
/// as `eta --why wait` and the next plain `eta` do: `run` waiting on a slot
/// or a queued lease is not work. Opens only where no wait or pause is
/// open; true when it opened one (only that one is closed later).
pub(crate) fn mark_waiting(
    dir: &Path,
    session: &str,
    task: &str,
    note: Option<&str>,
    now: u64,
) -> bool {
    let mut opened = false;
    edit_row(dir, session, task, |row| match note {
        Some(n) => {
            if stage_of(row) == Some(feed::Stage::Running)
                && !row.contains_key("waiting_since")
                && !row.contains_key("paused_since")
            {
                row.insert("waiting_since".into(), json!(stamp(now)));
                row.insert("note".into(), json!(n));
                opened = true;
            }
        }
        None => close_wait(row, now),
    });
    opened
}

/// The feed commands: read the session's feed, change it, write it back.
/// The message, and the keys the command landed (whose leases go back).
fn run_feed(
    dir: &Path,
    session: &str,
    cmd: &Cmd,
    flags: &Flags,
    now: u64,
) -> Result<(String, Vec<String>), String> {
    let cfg = flags.claude_dir.as_deref();
    let file = file_for(dir, session);
    if *cmd == Cmd::Path {
        return Ok((file.display().to_string(), Vec::new()));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _lock = Lock::take(&file)?;
    let existing = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object);
    if *cmd == Cmd::ClearDone {
        return Ok((clear_done(&file, existing), Vec::new()));
    }
    if let Some(doc) = &existing
        && let Some(w) = writer_of(doc)
        && w != WRITER
    {
        return Err(format!(
            "{} is written by {w}; giverny orchestrator-session leaves it alone",
            file.display()
        ));
    }
    match cmd {
        Cmd::Show => Ok((
            show(existing.as_ref().unwrap_or(&new_doc(session)), now),
            Vec::new(),
        )),
        Cmd::Clear => {
            if existing.is_some() {
                std::fs::remove_file(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            }
            Ok(("cleared".into(), Vec::new()))
        }
        _ => {
            let mut doc = existing.unwrap_or_else(|| new_doc(session));
            let obj = doc.as_object_mut().expect("filtered to objects");
            obj.insert("writer".into(), json!(WRITER));
            obj.entry("version").or_insert(json!(feed::FEED_VERSION));
            obj.entry("session").or_insert(json!(session));
            if doc.get("root").is_none() {
                continuation::stamp_root(&mut doc, continuation::root_of(cfg, session).as_deref());
            }
            let sessions = continuation::ids(&doc);
            let history = orchestrator_session_history::path(dir);
            let mut flags = flags.clone();
            let said = tell_record(&doc, cmd, &mut flags, history.as_deref());
            if let (Cmd::Start(_), Some(agent), None) = (cmd, &flags.agent, &flags.agent_desc)
                && needs_description(&doc, now)
            {
                flags.agent_desc = agent_description(cfg, &sessions, agent);
            }
            let before = done_keys(&doc);
            let read_log = |agent: &str| worker_log(cfg, &sessions, agent);
            let linked = link_handoffs(&mut doc, read_log);
            for (agent, _) in &linked {
                freeze_tokens(&mut doc, &before, Some(agent), read_log);
            }
            let msg = apply(&mut doc, cmd, &flags, now)?;
            let handed = match cmd {
                Cmd::Start(_) => flags.agent.as_deref(),
                _ => None,
            };
            freeze_tokens(&mut doc, &before, handed, read_log);
            let mut msg = msg;
            for (agent, keys) in &linked {
                msg.push_str(&format!(
                    "\n  {} handed to {agent} by message: its tokens are its own",
                    keys.join(", ")
                ));
            }
            if let (Cmd::Start(task), None) = (cmd, &flags.agent)
                && let Some(hint) = idle_hint(task, &idle_workers(&doc, task, now))
                && !row_has_agent(&doc, task)
            {
                msg = format!("{msg}\n{hint}");
            }
            write(&file, &doc).map_err(|e| format!("{}: {e}", file.display()))?;
            if let Some(h) = &history {
                learn(&doc, &before, session, h);
            }
            let msg = match said {
                Some(said) => format!("{msg}{said}"),
                None => msg,
            };
            let landed = done_keys(&doc)
                .into_iter()
                .filter(|k| !before.contains(k))
                .collect();
            let msg = match cmd {
                Cmd::Plan(key) if !has_brief(&doc, key) => format!("{msg}\n{}", no_brief(key)),
                _ => msg,
            };
            Ok((msg, landed))
        }
    }
}

/// Whether the row `key` has a worker.
fn row_has_agent(doc: &Value, key: &str) -> bool {
    doc.get("rows")
        .and_then(Value::as_array)
        .and_then(|rows| find(rows, key).map(|i| &rows[i]))
        .is_some_and(|r| r.get("agent_id").is_some())
}

/// Whether the row `key` names a brief.
fn has_brief(doc: &Value, key: &str) -> bool {
    doc.get("rows")
        .and_then(Value::as_array)
        .and_then(|rows| find(rows, key).map(|i| &rows[i]))
        .and_then(|r| r.get("brief"))
        .and_then(Value::as_str)
        .is_some_and(|b| !b.trim().is_empty())
}

/// The line `plan` adds when its row has no brief.
fn no_brief(key: &str) -> String {
    format!(
        "  no brief: its Next up row opens to its title and note only; write the \
         worker's prompt (or the task's text) to a file and pass `--brief FILE` \
         (`giverny orchestrator-session plan {key} --eta … --brief FILE`, or on `start`)"
    )
}

/// `plan`/`start` with `--eta N`, and a worker's first `eta` on a Running
/// row (its re-estimate): how such estimates have fared, from the history,
/// told and never applied — the figure given is the figure stored and shown.
/// Also fills in the repo the row will remember. Returns what to add to the
/// command's line, or `None` for a command with no estimate.
fn tell_record(
    doc: &Value,
    cmd: &Cmd,
    flags: &mut Flags,
    history: Option<&Path>,
) -> Option<String> {
    let (key, track, given) = match cmd {
        Cmd::Plan(key) | Cmd::Start(key) => (
            key,
            orchestrator_session_history::Track::Guess,
            flags.eta_s?,
        ),
        Cmd::Eta(key, left) => (key, orchestrator_session_history::Track::Reestimate, *left),
        _ => return None,
    };
    let row = doc
        .get("rows")
        .and_then(Value::as_array)
        .and_then(|rows| find(rows, key).map(|i| &rows[i]))
        .and_then(Value::as_object);
    if track == orchestrator_session_history::Track::Reestimate
        && !row.is_some_and(|r| {
            stage_of(r) == Some(feed::Stage::Running) && !r.contains_key("reest_s")
        })
    {
        // Not a first re-estimate: a later one, or a plan's, is not scored.
        return None;
    }
    let row_str = |k: &str| row.and_then(|r| r.get(k)).and_then(Value::as_str);
    if flags.repo.is_none() {
        flags.repo = row_str("repo").map(String::from).or_else(|| {
            let cwd = std::env::current_dir().unwrap_or_default();
            orchestrator_session_history::repo_of(key, &cwd)
        });
    }
    let title = flags.title.as_deref().or(row_str("title")).unwrap_or("");
    let kind = orchestrator_session_history::kind_of(title);
    let past = history
        .map(orchestrator_session_history::load)
        .unwrap_or_default();
    let (repo, kind) = (flags.repo.as_deref(), kind.as_deref());
    let record = orchestrator_session_history::track_record(&past, track, repo, kind);
    if track == orchestrator_session_history::Track::Reestimate {
        return record.map(|r| format!("\n  {r}"));
    }
    let told = record.unwrap_or_else(|| {
        let n = past.len();
        format!("too few landed tasks to tell how such guesses fare ({n} in the history)")
    });
    Some(format!(": ~{}\n  {told}", feed::fmt_span(given as i64)))
}

/// `accuracy`: the history's report.
fn accuracy(dir: &Path, repo: Option<&str>) -> String {
    match orchestrator_session_history::path(dir) {
        Some(h) => {
            orchestrator_session_history::accuracy(&orchestrator_session_history::load(&h), repo)
        }
        None => format!(
            "the history is off (${} is empty)",
            orchestrator_session_history::ENV
        ),
    }
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
            && let Err(e) = orchestrator_session_history::append(history, &rec)
        {
            eprintln!("giverny orchestrator-session: {}: {e}", history.display());
        }
    }
}

/// A landed row as a history record: wall time from its true start
/// (`spawned` when a pause moved `started` on), working time that less its
/// paused and waiting spans.
pub fn record_of(
    r: &Map<String, Value>,
    session: &str,
) -> Option<orchestrator_session_history::Record> {
    let started = ms_of(r, "spawned").or_else(|| ms_of(r, "started"))?;
    let ended = ms_of(r, "ended")?;
    let wall_s = ended.saturating_sub(started) / 1000;
    let paused_s = u64_of(r, "paused_s").unwrap_or(0);
    let wait_s = u64_of(r, "wait_s").unwrap_or(0);
    let s = |k: &str| r.get(k).and_then(Value::as_str).map(String::from);
    let title = s("title");
    let usage_u64 = |k: &str| {
        r.get("usage")
            .and_then(Value::as_object)
            .and_then(|u| u64_of(u, k))
    };
    Some(orchestrator_session_history::Record {
        key: s("key").unwrap_or_default(),
        kind: title
            .as_deref()
            .and_then(orchestrator_session_history::kind_of),
        title,
        repo: s("repo"),
        session: Some(session.to_string()),
        // `eta_guess_s`: the raw guess a row from before giverny#229 kept
        // beside its corrected `eta_s`.
        estimate_s: u64_of(r, "eta_guess_s")
            .or_else(|| u64_of(r, "eta_first_s"))
            .or_else(|| u64_of(r, "eta_s")),
        eta_s: u64_of(r, "eta_first_s").or_else(|| u64_of(r, "eta_s")),
        eta_final_s: u64_of(r, "eta_s"),
        reest_s: u64_of(r, "reest_s"),
        reest_at_s: u64_of(r, "reest_at_s"),
        wall_s,
        paused_s,
        wait_s,
        work_s: wall_s.saturating_sub(paused_s + wait_s),
        started: Some(stamp(started)),
        ended: Some(stamp(ended)),
        outcome: s("landing"),
        peak_mb: usage_u64("peak_mb"),
        cpu_s: usage_u64("cpu_s"),
        oom_kills: usage_u64("oom_kills").filter(|n| *n > 0),
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

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The `giverny orchestrator-session` entrypoint. Returns the process exit code. `spool` is
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
        // The plugin's hook, under the name it had before `giverny hook`.
        return crate::plugin_hook::main(false);
    }
    if cmd == Cmd::Accuracy {
        // The history is the machine's, not a session's.
        println!("{}", accuracy(&feed::feed_dir(), flags.repo.as_deref()));
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
            "giverny orchestrator-session: no session — run it from inside Claude Code \
             (it sets ${SESSION_ENV}) or pass --session <id>"
        );
        return 2;
    };
    let now = now_ms();
    match run_in_code(&feed::feed_dir(), &session, &cmd, &flags, now, None) {
        Ok((msg, _)) if cmd == Cmd::ClearDone => {
            let pane = if crate::hooks::send_clear_done(spool, Some(&session), now) {
                "asked Giverny to clear the agents pane's Done rows in this tab"
            } else {
                "not in a Giverny tab, so no pane to clear"
            };
            println!("{msg}; {pane}");
            0
        }
        Ok((msg, code)) => {
            if !msg.is_empty() {
                println!("{}", msg.trim_end());
            }
            code
        }
        Err(e) => {
            eprintln!("giverny orchestrator-session: {e}");
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
        let d = std::env::temp_dir().join(format!(
            "giverny-orchestrator-session-{name}-{}",
            std::process::id()
        ));
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
    fn an_orchestrator_session_from_plan_to_land_is_what_the_pane_reads() {
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
    fn plan_says_when_a_row_has_no_brief_and_start_fills_it() {
        let dir = scratch("brief");
        let said = run(&dir, "plan a --eta 10", T0).unwrap();
        assert!(said.contains("no brief"), "{said}");
        assert_eq!(read_feed(&dir).rows[0].brief, None);
        let said = run(&dir, "plan b --eta 10 --brief /x/b.md", T0).unwrap();
        assert!(!said.contains("no brief"), "{said}");
        assert_eq!(read_feed(&dir).rows[1].brief, Some("/x/b.md".into()));
        // Re-planned without --brief: the row keeps its brief, and says nothing.
        let said = run(&dir, "plan b --eta 12", T0).unwrap();
        assert!(!said.contains("no brief"), "{said}");
        // `start` fills one in, and replaces one.
        run(&dir, "start a --brief /x/a.md", T0).unwrap();
        run(&dir, "start b --brief /x/b2.md", T0).unwrap();
        let f = read_feed(&dir);
        assert_eq!(f.rows[0].brief, Some("/x/a.md".into()));
        assert_eq!(f.rows[1].brief, Some("/x/b2.md".into()));
        // A relative path is stored absolute: the pane reads it from elsewhere.
        let (_, flags) = parse_args(&args("plan c --eta 5 --brief rel.md")).unwrap();
        assert!(Path::new(flags.brief.as_deref().unwrap()).is_absolute());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_workers_first_re_estimate_is_kept_told_and_learned() {
        let dir = scratch("reest");
        let h = dir.join(orchestrator_session_history::FILE);
        // Past FEATURE re-estimates in demo ran ×1.5: 10m said, 15m taken.
        for _ in 0..orchestrator_session_history::MIN_SAMPLES {
            let rec = orchestrator_session_history::Record {
                key: "demo#1".into(),
                repo: Some("demo".into()),
                kind: Some("FEATURE".into()),
                estimate_s: Some(3000),
                reest_s: Some(600),
                reest_at_s: Some(300),
                wall_s: 1200,
                work_s: 1200,
                outcome: Some("Done".into()),
                ..orchestrator_session_history::Record::default()
            };
            orchestrator_session_history::append(&h, &rec).unwrap();
        }
        let said = run(&dir, "plan demo#9 --eta 50 --title FEATURE:x", T0).unwrap();
        assert!(
            said.contains("your last 5 FEATURE guesses in demo took ×0.40"),
            "{said}"
        );
        run(&dir, "start demo#9", T0).unwrap();
        // Paused two minutes: not working time.
        run(&dir, "pause demo#9", T0 + MIN).unwrap();
        run(&dir, "resume demo#9", T0 + 3 * MIN).unwrap();
        // Told how such re-estimates fared, but the figure stands.
        let said = run(&dir, "eta demo#9 10", T0 + 6 * MIN).unwrap();
        assert_eq!(
            said,
            "demo#9: ~10m left\n  your last 5 FEATURE re-estimates in demo took \
             ×1.50 of what you said (median)"
        );
        let row = |k: &str| {
            let doc: Value =
                serde_json::from_slice(&std::fs::read(feed::feed_path(&dir, "s1")).unwrap())
                    .unwrap();
            doc["rows"][0][k].clone()
        };
        assert_eq!(row("reest_s"), 600, "kept as given");
        assert_eq!(row("reest_at_s"), 4 * 60, "four minutes worked");
        assert_eq!(row("eta_s"), 4 * 60 + 600, "not corrected");
        // A later eta is taken as given and leaves the re-estimate alone.
        let said = run(&dir, "eta demo#9 10", T0 + 8 * MIN).unwrap();
        assert_eq!(said, "demo#9: ~10m left");
        assert_eq!(row("eta_s"), 6 * 60 + 600);
        assert_eq!(row("reest_s"), 600);
        // Landed: the record carries the re-estimate.
        run(&dir, "land demo#9", T0 + 30 * MIN).unwrap();
        let last = orchestrator_session_history::load(&h).pop().unwrap();
        assert_eq!(last.reest_s, Some(600));
        assert_eq!(last.reest_at_s, Some(240));
        assert_eq!(last.work_s, 28 * 60);
        // `accuracy` reads the same history.
        let out = run(&dir, "accuracy --repo demo", T0).unwrap();
        assert!(out.contains("over 6 landed tasks in demo"), "{out}");
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
        // A worker spawned outside an orchestrator session; its dispatcher (or
        // the worker) reaches for `eta`, and gets a Running row, not an error.
        let dir = scratch("eta-no-row");
        let said = run(
            &dir,
            "eta acme#613 40 --agent w9 --title Graph --repo acme",
            T0,
        )
        .unwrap();
        assert!(said.contains("started it now with ~40m left"), "{said}");
        assert!(
            said.contains("giverny orchestrator-session start acme#613 --eta"),
            "{said}"
        );
        let f = read_feed(&dir);
        let r = &f.rows[0];
        assert_eq!(r.key, "acme#613");
        assert_eq!(r.stage(), feed::Stage::Running);
        assert_eq!(r.started_ms, Some(T0));
        assert_eq!(r.eta_s, Some(40 * 60), "as given");
        assert_eq!(r.title.as_deref(), Some("Graph"));
        assert_eq!(r.agent_id.as_deref(), Some("w9"));
        assert!(r.follows_worker, "no dispatcher lands it");

        // From then on it is an ordinary row: a later eta re-estimates it.
        run(&dir, "eta acme#613 10", T0 + 20 * MIN).unwrap();
        assert_eq!(read_feed(&dir).rows[0].eta_s, Some(30 * 60));
        assert!(read_feed(&dir).rows[0].follows_worker);
        // A dispatcher's `start` takes it over: it lands it, not the worker.
        run(&dir, "start acme#613 --agent w9", T0 + 21 * MIN).unwrap();
        assert!(!read_feed(&dir).rows[0].follows_worker);

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
        let theirs = br#"{"version":1,"session":"s1","writer":"other/status-writer","rows":[]}"#;
        std::fs::write(feed::feed_path(&dir, "s1"), theirs).unwrap();
        let err = run(&dir, "start a", T0).unwrap_err();
        assert!(err.contains("other/status-writer"), "{err}");
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

        let theirs = br#"{"version":1,"session":"s1","writer":"other/status-writer","rows":[{"key":"x","stage":"done"}]}"#;
        std::fs::write(feed::feed_path(&dir, "s1"), theirs).unwrap();
        let msg = run(&dir, "clear-done", T0).unwrap();
        assert!(msg.contains("other/status-writer"), "{msg}");
        assert_eq!(
            std::fs::read(feed::feed_path(&dir, "s1")).unwrap(),
            theirs.to_vec()
        );
        assert_eq!(parse_args(&args("clear-done")).unwrap().0, Cmd::ClearDone);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn history(dir: &Path) -> Vec<orchestrator_session_history::Record> {
        orchestrator_session_history::load(&dir.join(orchestrator_session_history::FILE))
    }

    #[test]
    fn plan_and_start_keep_the_guess_and_tell_how_such_guesses_fared() {
        let dir = scratch("told");
        std::fs::create_dir_all(&dir).unwrap();
        let h = dir.join(orchestrator_session_history::FILE);
        // Five landed demo BUGs that took half their guess, and five
        // FEATUREs that took a quarter.
        for (kind, work) in [("BUG", 15), ("FEATURE", 10)] {
            for _ in 0..orchestrator_session_history::MIN_SAMPLES {
                let rec = orchestrator_session_history::Record {
                    key: "x".into(),
                    repo: Some("demo".into()),
                    kind: Some(kind.into()),
                    estimate_s: Some(if kind == "BUG" { 1800 } else { 2400 }),
                    wall_s: work * 60,
                    work_s: work * 60,
                    outcome: Some("Done".into()),
                    ..Default::default()
                };
                orchestrator_session_history::append(&h, &rec).unwrap();
            }
        }
        let (cmd, mut flags) = parse_args(&args("plan demo#1 --eta 40")).unwrap();
        flags.title = Some("BUG: pane flickers".into());
        let said = run_in(&dir, "s1", &cmd, &flags, T0).unwrap();
        assert_eq!(
            said.lines().take(2).collect::<Vec<_>>(),
            [
                "planned demo#1: ~40m",
                "  your last 5 BUG guesses in demo took ×0.50 of what you said (median)"
            ]
        );
        let doc: Value =
            serde_json::from_slice(&std::fs::read(feed::feed_path(&dir, "s1")).unwrap()).unwrap();
        let row = &doc["rows"][0];
        assert_eq!(row["eta_s"], 2400, "the pane counts down from the guess");
        assert!(row.get("eta_guess_s").is_none() && row.get("eta_basis").is_none());
        assert_eq!(row["repo"], "demo");
        let shown = run(&dir, "show", T0).unwrap();
        assert!(shown.contains("~40m") && !shown.contains("said"), "{shown}");

        // `start --eta` tells again; a FEATURE title picks its own level.
        let (cmd, mut flags) = parse_args(&args("start demo#2 --eta 40")).unwrap();
        flags.title = Some("FEATURE: estimates".into());
        let said = run_in(&dir, "s1", &cmd, &flags, T0).unwrap();
        assert!(said.starts_with("started demo#2: ~40m\n"), "{said}");
        assert!(
            said.contains("your last 5 FEATURE guesses in demo took ×0.25"),
            "{said}"
        );

        // Another repo: everything (0.25 ×5, 0.5 ×5 → 0.375).
        let said = run(&dir, "plan acme#3 --eta 40", T0).unwrap();
        assert!(
            said.contains("your last 10 guesses took ×0.38 of what you said (median)"),
            "{said}"
        );

        // A row planned before giverny#229 kept its raw guess beside a
        // corrected `eta_s`; a new estimate drops both old fields.
        let file = feed::feed_path(&dir, "s1");
        let mut doc: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        doc["rows"][0]["eta_s"] = json!(1200);
        doc["rows"][0]["eta_guess_s"] = json!(2400);
        doc["rows"][0]["eta_basis"] = json!("×0.50 from the last 5 BUG tasks in demo");
        std::fs::write(&file, serde_json::to_vec(&doc).unwrap()).unwrap();
        run(&dir, "plan demo#1 --eta 30", T0).unwrap();
        let doc: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        let row = &doc["rows"][0];
        assert_eq!(row["eta_s"], 1800);
        assert!(row.get("eta_guess_s").is_none() && row.get("eta_basis").is_none());

        // No history: the guess stands, and the line says there is too little.
        let empty = scratch("told-empty");
        let said = run(&empty, "plan a --eta 40", T0).unwrap();
        assert!(
            said.starts_with(
                "planned a: ~40m\n  too few landed tasks to tell how such guesses fare \
                 (0 in the history)"
            ),
            "{said}"
        );
        let r = &read_feed(&empty).rows[0];
        assert_eq!(r.eta_s, Some(2400));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn land_appends_wall_and_working_time_without_waits_or_pauses() {
        let dir = scratch("learn");
        let (cmd, mut flags) = parse_args(&args("start demo#9 --eta 30")).unwrap();
        flags.title = Some("FEATURE: x".into());
        run_in(&dir, "s1", &cmd, &flags, T0).unwrap();
        // 10m work, then 20m waiting on a build slot, then 5m work…
        run(&dir, "eta demo#9 30 --why wait --note slot", T0 + 10 * MIN).unwrap();
        run(&dir, "eta demo#9 20 --why ready", T0 + 30 * MIN).unwrap();
        // …a 15m pause (a wait open across it is closed by the pause)…
        run(&dir, "eta demo#9 15 --why blocked", T0 + 35 * MIN).unwrap();
        run(&dir, "pause demo#9", T0 + 40 * MIN).unwrap();
        run(&dir, "resume demo#9", T0 + 55 * MIN).unwrap();
        // …and 5m more work.
        run(&dir, "land demo#9", T0 + 60 * MIN).unwrap();
        let h = history(&dir);
        assert_eq!(h.len(), 1);
        let r = &h[0];
        assert_eq!(r.key, "demo#9");
        assert_eq!(r.repo.as_deref(), Some("demo"));
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
        // checked through `orchestrator_session_history::path` rather than by running.
        assert_eq!(
            orchestrator_session_history::path(Path::new("/f")),
            std::env::var_os(orchestrator_session_history::ENV)
                .map(|v| (!v.is_empty()).then(|| PathBuf::from(v)))
                .unwrap_or(Some(PathBuf::from("/f/history.jsonl")))
        );
    }

    /// 14 cores, 23 G; limits 12 cores, 16 G; idle.
    fn machine() -> resources::Capacity {
        use giverny_core::limits::{Limits, Load, Machine, Mem, Resolved};
        resources::Capacity {
            machine: Machine {
                cores: 14,
                ram: Mem::gb(23),
                gpus: vec![],
            },
            limits: Resolved {
                cpu_cores: 12,
                ram: Mem::gb(16),
                gpus: vec![],
            },
            configured: Limits::default(),
            load: Load {
                mem_available: Some(Mem::gb(20)),
                load1: Some(0.0),
            },
            default_lease: Default::default(),
        }
    }

    fn run_as(dir: &Path, session: &str, line: &str, now: u64) -> (String, i32) {
        let (cmd, flags) = parse_args(&args(line)).unwrap();
        run_in_code(dir, session, &cmd, &flags, now, Some(&machine())).unwrap()
    }

    #[test]
    fn claim_queues_past_the_limit_across_sessions_and_land_releases() {
        let dir = scratch("claim");
        run_as(&dir, "a", "start t1 --eta 30", T0);
        let (msg, code) = run_as(&dir, "a", "claim t1 --cpu 8 --ram 10G --slot cargo:/t", T0);
        assert_eq!(code, resources::exit::GRANTED, "{msg}");
        assert_eq!(msg, "granted t1: 8 cpu, 10G, slot cargo:/t");
        // The row carries its lease for the pane.
        let f = feed::read(&feed::feed_path(&dir, "a")).unwrap();
        let l = f.rows[0].lease.as_ref().unwrap();
        assert_eq!(
            (l.state, l.cpu, l.ram_mb),
            (feed::LeaseState::Granted, 8, 10240)
        );

        // Session b asks past the limit: queued behind t1, with t1's ETA.
        run_as(&dir, "b", "plan t2 --eta 10", T0);
        let (msg, code) = run_as(&dir, "b", "claim t2 --cpu 6 --ram 2G", T0 + 16 * MIN);
        assert_eq!(code, resources::exit::QUEUED, "{msg}");
        assert!(
            msg.starts_with("queued #1 behind t1 (8 cpu, 10G, slot cargo:/t; ~14m)"),
            "{msg}"
        );
        let f = feed::read(&feed::feed_path(&dir, "b")).unwrap();
        let l = f.rows[0].lease.as_ref().unwrap();
        assert_eq!(l.state, feed::LeaseState::Queued);
        assert_eq!((l.position, l.behind.as_deref()), (Some(1), Some("t1")));
        // The slot is exclusive whatever the size.
        let (_, code) = run_as(&dir, "b", "claim t3 --cpu 1 --slot cargo:/t", T0 + 16 * MIN);
        assert_eq!(code, resources::exit::QUEUED);
        run_as(&dir, "b", "release t3", T0 + 16 * MIN);

        let shown = run_as(&dir, "b", "resources", T0 + 17 * MIN).0;
        assert!(
            shown.contains("limits    12 cores (auto), 16G RAM (auto)"),
            "{shown}"
        );
        assert!(shown.contains("t1"), "{shown}");
        assert!(shown.contains("#1 t2"), "{shown}");

        // t1 lands: its lease goes, and t2's re-claim is granted.
        let (msg, _) = run_as(&dir, "a", "land t1", T0 + 18 * MIN);
        assert!(msg.contains("released 8 cpu, 10G, slot cargo:/t"), "{msg}");
        let f = feed::read(&feed::feed_path(&dir, "a")).unwrap();
        assert_eq!(f.rows[0].lease, None, "a landed row holds nothing");
        let (msg, code) = run_as(&dir, "b", "claim t2 --cpu 6 --ram 2G", T0 + 19 * MIN);
        assert_eq!(code, resources::exit::GRANTED, "{msg}");
        // Too big for the limits is refused, not queued.
        let (msg, code) = run_as(&dir, "a", "claim huge --cpu 20", T0 + 19 * MIN);
        assert_eq!(code, resources::exit::REFUSED, "{msg}");
        // `drop` releases too; `release` of nothing says so.
        let (msg, _) = run_as(&dir, "b", "drop t2", T0 + 22 * MIN);
        assert!(msg.contains("released"), "{msg}");
        assert_eq!(
            run_as(&dir, "b", "release t2", T0 + 22 * MIN).0,
            "t2 held no lease"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_expired_lease_frees_itself_and_any_command_keeps_it() {
        let dir = scratch("claim-ttl");
        run_as(&dir, "a", "claim dead --cpu 12", T0);
        run_as(&dir, "b", "claim live --cpu 0 --ram 4G", T0);
        let (_, code) = run_as(&dir, "c", "claim x --cpu 4", T0);
        assert_eq!(code, resources::exit::QUEUED);
        // b and c run orchestrator-session commands every few minutes; a is gone.
        for k in 1..=5 {
            run_as(&dir, "b", "show", T0 + k * 5 * MIN);
            run_as(&dir, "c", "show", T0 + k * 5 * MIN);
        }
        let (msg, code) = run_as(&dir, "c", "claim x --cpu 4", T0 + 26 * MIN);
        assert_eq!(code, resources::exit::GRANTED, "{msg}");
        let shown = run_as(&dir, "c", "resources", T0 + 26 * MIN).0;
        assert!(!shown.contains("dead"), "{shown}");
        assert!(shown.contains("live"), "{shown}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The hook's reply for `session`'s own call, as text.
    fn hook(dir: &Path, session: &str, now: u64) -> Option<String> {
        let out = crate::plugin_hook::run(&json!({"session_id": session}), dir, now, true)?;
        let v: Value = serde_json::from_str(&out).unwrap();
        Some(
            v["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .to_string(),
        )
    }

    fn line(s: &str) -> Vec<String> {
        // Like a shell: `"…"` is one argument.
        let mut out = Vec::new();
        for (i, part) in s.split('"').enumerate() {
            if i % 2 == 1 {
                out.push(part.to_string());
            } else {
                out.extend(part.split_whitespace().map(String::from));
            }
        }
        out
    }

    fn run_line(dir: &Path, session: &str, l: &str, now: u64) -> Result<(String, i32), String> {
        let (cmd, flags) = parse_args(&line(l))?;
        run_in_code(dir, session, &cmd, &flags, now, Some(&machine()))
    }

    #[test]
    fn a_queued_orchestrator_asks_the_holder_which_shrinks_and_releases() {
        let dir = scratch("ask");
        // a holds the cargo slot for a long task; b's short task needs it.
        run_as(&dir, "sess-a", "start demo#12 --eta 60", T0);
        let (msg, _) = run_as(
            &dir,
            "sess-a",
            "claim demo#12 --cpu 3 --ram 6G --slot cargo:/t",
            T0,
        );
        assert!(msg.starts_with("granted"), "{msg}");
        run_as(&dir, "sess-b", "plan acme#5 --eta 10", T0);
        let (msg, code) = run_as(
            &dir,
            "sess-b",
            "claim acme#5 --cpu 2 --ram 2G --slot cargo:/t --priority high",
            T0 + MIN,
        );
        assert_eq!(code, resources::exit::QUEUED, "{msg}");
        assert!(
            msg.contains("that wait (~59m) is longer than acme#5 itself (~10m)")
                && msg.contains("giverny orchestrator-session ask demo#12"),
            "the queued answer suggests asking: {msg}"
        );
        // Nothing in anyone's inbox yet: the hook says nothing.
        assert_eq!(hook(&dir, "sess-a", T0 + MIN), None);

        let (msg, _) = run_line(
            &dir,
            "sess-b",
            r#"ask demo#12 "a 2-minute test needs the slot""#,
            T0 + 2 * MIN,
        )
        .unwrap();
        assert!(msg.starts_with("asked demo#12 (session sess-a)"), "{msg}");
        assert!(msg.contains("while acme#5 holds or waits"), "{msg}");
        let id = msg.split("message ").nth(1).unwrap()[..7].to_string();

        // a's next tool call carries it, once; b's own calls do not.
        assert_eq!(hook(&dir, "sess-b", T0 + 3 * MIN), None);
        let ctx = hook(&dir, "sess-a", T0 + 3 * MIN).unwrap();
        for want in [
            "session sess-b, task acme#5",
            "priority high",
            "queued #1 for 2 cpu, 2G, slot cargo:/t",
            "~10m left on it",
            "your lease demo#12 (3 cpu, 6G, slot cargo:/t)",
            "\"a 2-minute test needs the slot\"",
            &format!("giverny-orchestrator-session reply {id} "),
            "giverny-orchestrator-session release demo#12",
            "giverny-orchestrator-session claim demo#12 --cpu <fewer> --ram <less>",
        ] {
            assert!(ctx.contains(want), "{want:?} in {ctx}");
        }
        assert_eq!(hook(&dir, "sess-a", T0 + 3 * MIN), None, "delivered once");

        // a shrinks in place: fewer cores, less RAM, the slot kept.
        let (msg, code) = run_as(
            &dir,
            "sess-a",
            "claim demo#12 --cpu 2 --ram 3G",
            T0 + 4 * MIN,
        );
        assert_eq!(code, resources::exit::GRANTED);
        assert_eq!(
            msg,
            "shrunk demo#12: 2 cpu, 3G, slot cargo:/t (was 3 cpu, 6G, slot cargo:/t)"
        );
        let (msg, _) = run_as(&dir, "sess-a", "claim demo#12 --ram 8G", T0 + 4 * MIN);
        assert!(msg.contains("is not grown"), "{msg}");
        let f = feed::read(&feed::feed_path(&dir, "sess-a")).unwrap();
        assert_eq!(f.rows[0].lease.as_ref().unwrap().ram_mb, 3072);
        // ... then gives up the slot altogether, and answers.
        run_as(&dir, "sess-a", "release demo#12", T0 + 5 * MIN);
        let (msg, _) = run_line(
            &dir,
            "sess-a",
            &format!(r#"reply {id} "released; claim it again when done""#),
            T0 + 5 * MIN,
        )
        .unwrap();
        assert!(
            msg.starts_with("replied to acme#5 (session sess-b)"),
            "{msg}"
        );

        // b's next call carries the reply; its re-claim is granted.
        let ctx = hook(&dir, "sess-b", T0 + 6 * MIN).unwrap();
        assert!(ctx.contains(&format!("to your ask {id}")), "{ctx}");
        assert!(ctx.contains("demo#12 has released its lease"), "{ctx}");
        assert!(
            ctx.contains("giverny-orchestrator-session claim acme#5"),
            "{ctx}"
        );
        let (msg, code) = run_as(
            &dir,
            "sess-b",
            "claim acme#5 --cpu 2 --ram 2G --slot cargo:/t --priority high",
            T0 + 6 * MIN,
        );
        assert_eq!(code, resources::exit::GRANTED, "{msg}");

        // Errors say what to do.
        assert!(run_line(&dir, "sess-b", r#"reply mzzzzzz "x""#, T0 + 6 * MIN).is_err());
        assert!(run_line(&dir, "sess-b", r#"ask acme#5 "x""#, T0 + 6 * MIN).is_err());
        assert!(run_line(&dir, "sess-b", r#"ask nobody "x""#, T0 + 6 * MIN).is_err());
        assert!(parse_args(&line("ask demo#12")).is_err(), "no message");

        // A message expires with the asker's lease: b asks a, then lands.
        run_as(&dir, "sess-a", "claim other --cpu 1", T0 + 7 * MIN);
        run_line(&dir, "sess-b", r#"ask other "spare a core?""#, T0 + 7 * MIN).unwrap();
        run_as(&dir, "sess-b", "release acme#5", T0 + 8 * MIN);
        assert_eq!(hook(&dir, "sess-a", T0 + 9 * MIN), None, "expired unread");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_hook_keeps_a_busy_orchestrators_leases_alive() {
        let dir = scratch("hook-beat");
        run_as(&dir, "busy", "claim t --cpu 2", T0);
        run_as(&dir, "idle", "claim u --cpu 2", T0);
        // `busy` runs no orchestrator-session command, but its (and its workers') tool calls
        // fire the hook every minute; `idle` does nothing.
        for k in 1..=25 {
            let worker = json!({"session_id": "busy", "agent_id": "w1"});
            let now = T0 + k * MIN;
            let _ = crate::plugin_hook::run(&worker, &dir, now, true);
            let _ = hook(&dir, "busy", now + 1000);
        }
        let shown = run_as(&dir, "other", "resources", T0 + 25 * MIN).0;
        assert!(shown.contains("t   "), "busy's lease kept: {shown}");
        assert!(!shown.contains("u   "), "idle's expired: {shown}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn claim_flags_parse() {
        let (cmd, f) = parse_args(&args(
            "claim t --cpu 3 --ram 1.5G --gpu 1 --vram 8G --slot a --slot b --min-ram 512M --priority high",
        ))
        .unwrap();
        assert_eq!(cmd, Cmd::Claim("t".into()));
        let r = f.request();
        assert_eq!((r.cpu, r.ram_mb, r.gpu, r.vram_mb), (3, 1536, 1, 8192));
        assert_eq!(r.slots, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(r.min_ram_mb, Some(512));
        assert_eq!(r.priority.as_deref(), Some("high"));
        assert!(parse_args(&args("claim t --ram lots")).is_err());
        assert!(
            parse_args(&args("claim t --gpu 1")).is_err(),
            "--gpu needs --vram"
        );
        assert!(parse_args(&args("claim")).is_err());
        assert_eq!(parse_args(&args("resources")).unwrap().0, Cmd::Resources);
    }

    /// A worker spawned with the Agent tool: its `agent-<id>.meta.json`
    /// (the spawn description) under `<dir>/claude/projects/-w/<session>/
    /// subagents`. Returns that config dir.
    fn spawned(dir: &Path, session: &str, agent: &str, desc: &str) -> PathBuf {
        let cfg = dir.join("claude");
        let sub = cfg
            .join("projects")
            .join("-w")
            .join(session)
            .join("subagents");
        std::fs::create_dir_all(&sub).unwrap();
        let meta = json!({"description": desc, "agentType": "general-purpose"});
        std::fs::write(
            sub.join(format!("agent-{agent}.meta.json")),
            meta.to_string(),
        )
        .unwrap();
        std::fs::write(sub.join(format!("agent-{agent}.jsonl")), "").unwrap();
        cfg
    }

    fn run_cfg(dir: &Path, session: &str, l: &str, cfg: Option<&Path>, now: u64) -> (String, i32) {
        let (cmd, mut flags) = parse_args(&args(l)).unwrap();
        flags.claude_dir = cfg.map(Path::to_path_buf);
        run_in_code(dir, session, &cmd, &flags, now, Some(&machine())).unwrap()
    }

    fn held(dir: &Path, session: &str, task: &str, now: u64) -> bool {
        resources::with_ledger(&resources::ledger_path(dir), now, |l| {
            l.lease(session, task).is_some()
        })
        .unwrap()
    }

    #[test]
    fn a_hand_off_lands_a_row_started_before_the_worker_had_an_id() {
        let dir = scratch("handoff-noid");
        // plan → claim → start with no --agent → spawn (description `parse-fix: …`).
        run_cfg(&dir, "s1", "plan parse-fix --eta 30", None, T0);
        let (msg, code) = run_cfg(&dir, "s1", "claim parse-fix --cpu 3 --ram 3G", None, T0);
        assert_eq!(code, resources::exit::GRANTED, "{msg}");
        run_cfg(&dir, "s1", "start parse-fix", None, T0);
        let cfg = spawned(&dir, "s1", "w1", "parse-fix: the parser");
        // Another session needs more than is left: it queues behind parse-fix.
        let (msg, code) = run_cfg(&dir, "s2", "claim big --cpu 10 --ram 2G", None, T0 + MIN);
        assert_eq!(code, resources::exit::QUEUED, "{msg}");

        // Fifteen minutes on, the dispatcher hands that worker its next task.
        let (said, _) = run_cfg(
            &dir,
            "s1",
            "start lex-fix --agent w1 --eta 20",
            Some(&cfg),
            T0 + 15 * MIN,
        );
        assert!(said.contains("handed on from parse-fix"), "{said}");
        assert!(said.contains("released 3 cpu, 3G"), "{said}");
        let f = read_feed_of(&dir, "s1");
        let row = |k: &str| f.rows.iter().find(|r| r.key == k).unwrap();
        assert_eq!(row("parse-fix").stage(), feed::Stage::Done);
        assert_eq!(row("parse-fix").ended_ms, Some(T0 + 15 * MIN));
        assert_eq!(row("parse-fix").landing.as_deref(), Some("Done"));
        assert_eq!(row("parse-fix").agent_id.as_deref(), Some("w1"));
        assert_eq!(row("parse-fix").lease, None, "a landed row holds nothing");
        assert_eq!(row("lex-fix").stage(), feed::Stage::Running);
        assert!(
            !held(&dir, "s1", "parse-fix", T0 + 15 * MIN),
            "the lease went back"
        );
        let (msg, code) = run_cfg(
            &dir,
            "s2",
            "claim big --cpu 10 --ram 2G",
            None,
            T0 + 16 * MIN,
        );
        assert_eq!(code, resources::exit::GRANTED, "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_hook_puts_the_workers_id_on_a_row_started_without_it() {
        let dir = scratch("handoff-hook");
        run_cfg(&dir, "s1", "plan cfg-keys --eta 30", None, T0);
        run_cfg(&dir, "s1", "claim cfg-keys --cpu 3 --ram 3G", None, T0);
        run_cfg(&dir, "s1", "start cfg-keys", None, T0);
        let cfg = spawned(&dir, "s1", "w2", "cfg-keys: config keys");
        // The worker's first tool call: the hook joins it to its row by the
        // spawn description, and writes the id on the row.
        let transcript = cfg.join("projects").join("-w").join("s1.jsonl");
        let payload = json!({"session_id": "s1", "agent_id": "w2",
                             "transcript_path": transcript});
        crate::plugin_hook::run(&payload, &dir, T0 + MIN, true);
        let f = read_feed_of(&dir, "s1");
        assert_eq!(f.rows[0].agent_id.as_deref(), Some("w2"));
        // So the hand-off finds it by the id alone, no description read.
        let nowhere = dir.join("nowhere");
        let (said, _) = run_cfg(
            &dir,
            "s1",
            "start cfg-docs --agent w2",
            Some(&nowhere),
            T0 + 15 * MIN,
        );
        assert!(said.contains("handed on from cfg-keys"), "{said}");
        assert!(said.contains("released 3 cpu, 3G"), "{said}");
        assert!(!held(&dir, "s1", "cfg-keys", T0 + 15 * MIN));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_description_naming_no_row_hands_off_nothing() {
        let dir = scratch("handoff-other");
        run_cfg(&dir, "s1", "start parse-fix", None, T0);
        let cfg = spawned(&dir, "s1", "w2", "other: something else");
        let (said, _) = run_cfg(
            &dir,
            "s1",
            "start more --agent w2",
            Some(&cfg),
            T0 + 40 * MIN,
        );
        assert!(!said.contains("handed on"), "{said}");
        let f = read_feed_of(&dir, "s1");
        assert_eq!(f.rows[0].stage(), feed::Stage::Running, "not w2's task");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A billed reply at `at` that added `n` tokens (all of it fresh input).
    fn turn(id: &str, at: u64, n: u64) -> String {
        json!({"type": "assistant", "timestamp": stamp(at),
            "message": {"id": id, "model": "m",
                "usage": {"input_tokens": n, "output_tokens": 0}}})
        .to_string()
    }

    fn task_tokens(f: &feed::Feed, key: &str) -> Option<u64> {
        f.rows.iter().find(|r| r.key == key).unwrap().task_tokens
    }

    /// One worker, three tasks in turn: the first started before the worker
    /// had an id and handed on by its spawn description, the second landed
    /// by `land`, the third still running. Each landed row keeps what its own
    /// span spent, written when it landed, and the worker's later turns
    /// never reach it.
    #[test]
    fn a_landed_task_of_a_reused_worker_keeps_its_own_count() {
        let dir = scratch("frozen");
        run_cfg(&dir, "s1", "start parse-fix", None, T0);
        let cfg = spawned(&dir, "s1", "w1", "parse-fix: the parser");
        let log = cfg
            .join("projects")
            .join("-w")
            .join("s1")
            .join("subagents")
            .join("agent-w1.jsonl");
        let write = |lines: &[String]| std::fs::write(&log, lines.join("\n") + "\n").unwrap();
        let mut lines = vec![turn("a", T0 + MIN, 1_000), turn("b", T0 + 10 * MIN, 2_000)];
        write(&lines);

        let (said, _) = run_cfg(
            &dir,
            "s1",
            "start lex-fix --agent w1",
            Some(&cfg),
            T0 + 15 * MIN,
        );
        assert!(said.contains("handed on from parse-fix"), "{said}");
        let f = read_feed_of(&dir, "s1");
        assert_eq!(
            task_tokens(&f, "parse-fix"),
            Some(3_000),
            "its turns, to the hand-off"
        );
        assert_eq!(task_tokens(&f, "lex-fix"), None, "still running");

        lines.push(turn("c", T0 + 20 * MIN, 400));
        write(&lines);
        run_cfg(&dir, "s1", "land lex-fix", Some(&cfg), T0 + 25 * MIN);
        lines.push(turn("d", T0 + 30 * MIN, 50_000));
        write(&lines);
        run_cfg(
            &dir,
            "s1",
            "start emit-fix --agent w1",
            Some(&cfg),
            T0 + 30 * MIN,
        );
        lines.push(turn("e", T0 + 40 * MIN, 70_000));
        write(&lines);
        run_cfg(&dir, "s1", "eta emit-fix 5", Some(&cfg), T0 + 41 * MIN);

        let f = read_feed_of(&dir, "s1");
        assert_eq!(task_tokens(&f, "parse-fix"), Some(3_000));
        assert_eq!(
            task_tokens(&f, "lex-fix"),
            Some(400),
            "from its start to its landing"
        );
        assert_eq!(task_tokens(&f, "emit-fix"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A worker's only task keeps its worker's own count; one landed before
    /// the worker was handed its next is frozen at that hand-off.
    #[test]
    fn a_task_landed_before_its_worker_was_reused_is_frozen_at_the_hand_off() {
        let dir = scratch("frozen-later");
        run_cfg(&dir, "s1", "start a --agent w1", None, T0);
        let cfg = spawned(&dir, "s1", "w1", "a: first");
        let log = cfg
            .join("projects")
            .join("-w")
            .join("s1")
            .join("subagents")
            .join("agent-w1.jsonl");
        std::fs::write(&log, turn("a", T0 + MIN, 1_000) + "\n").unwrap();
        run_cfg(&dir, "s1", "land a", Some(&cfg), T0 + 10 * MIN);
        assert_eq!(
            task_tokens(&read_feed_of(&dir, "s1"), "a"),
            None,
            "one task so far"
        );
        std::fs::write(
            &log,
            [turn("a", T0 + MIN, 1_000), turn("b", T0 + 12 * MIN, 9_000)].join("\n") + "\n",
        )
        .unwrap();
        run_cfg(&dir, "s1", "start b --agent w1", Some(&cfg), T0 + 20 * MIN);
        let f = read_feed_of(&dir, "s1");
        assert_eq!(
            task_tokens(&f, "a"),
            Some(1_000),
            "to its landing, not the hand-off"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The dispatcher's `SendMessage` at `at`, as the worker's transcript has it.
    fn sent_at(at: u64, body: &str) -> String {
        json!({"type": "user", "timestamp": stamp(at), "isMeta": true,
            "origin": {"kind": "coordinator"},
            "message": {"role": "user", "content":
                format!("The coordinator sent a message while you were working:\n{body}")}})
        .to_string()
    }

    fn agent_of(f: &feed::Feed, key: &str) -> Option<String> {
        f.rows
            .iter()
            .find(|r| r.key == key)
            .unwrap()
            .agent_id
            .clone()
    }

    /// The inbar orchestrator session (giverny#217): one worker reused for three tasks with
    /// a plain `start` each time and the task handed over by a message in
    /// the dispatcher's own words, the third named only as "a review round
    /// on #828". Each task still gets the worker and its own count, frozen
    /// when it lands, however short it was.
    #[test]
    fn a_task_handed_by_message_after_a_plain_start_counts_its_own_tokens() {
        let dir = scratch("handoff-msg");
        run_cfg(&dir, "s1", "start inbar#828 --agent w1", None, T0);
        let cfg = spawned(&dir, "s1", "w1", "inbar#828: exit charts");
        let log = cfg
            .join("projects")
            .join("-w")
            .join("s1")
            .join("subagents")
            .join("agent-w1.jsonl");
        let write = |lines: &[String]| std::fs::write(&log, lines.join("\n") + "\n").unwrap();
        let mut lines = vec![turn("a", T0 + MIN, 1_000), turn("b", T0 + 5 * MIN, 2_000)];
        write(&lines);
        run_cfg(&dir, "s1", "land inbar#828", Some(&cfg), T0 + 8 * MIN);

        // A plain start: the idle worker is pointed out.
        let (said, _) = run_cfg(&dir, "s1", "start inbar#829", Some(&cfg), T0 + 8 * MIN);
        assert!(said.contains("w1 is idle since inbar#828 landed"), "{said}");
        assert!(said.contains("--agent w1"), "{said}");
        lines.push(sent_at(
            T0 + 8 * MIN + 13_000,
            "#828 verified and landed in Review — thanks. Next you hold inbar#829 and \
             nothing else (not inbar#8290).",
        ));
        lines.push(turn("c", T0 + 10 * MIN, 400));
        write(&lines);
        let (said, _) = run_cfg(&dir, "s1", "land inbar#829", Some(&cfg), T0 + 12 * MIN);
        assert!(said.contains("inbar#829 handed to w1 by message"), "{said}");
        let f = read_feed_of(&dir, "s1");
        assert_eq!(agent_of(&f, "inbar#829").as_deref(), Some("w1"));
        assert_eq!(
            task_tokens(&f, "inbar#828"),
            Some(3_000),
            "frozen at the hand-off"
        );
        assert_eq!(
            task_tokens(&f, "inbar#829"),
            Some(400),
            "four minutes, its own"
        );

        // The next one is named only by the task it is a round of.
        run_cfg(&dir, "s1", "start inbar#828-r1", Some(&cfg), T0 + 14 * MIN);
        lines.push(sent_at(
            T0 + 14 * MIN + 7_000,
            "#829 verified and landed in Review. Now a review round on #828 (covers #829 too).",
        ));
        lines.push(turn("d", T0 + 16 * MIN, 50));
        write(&lines);
        run_cfg(&dir, "s1", "land inbar#828-r1", Some(&cfg), T0 + 17 * MIN);
        lines.push(turn("e", T0 + 20 * MIN, 90_000));
        write(&lines);
        run_cfg(&dir, "s1", "show", Some(&cfg), T0 + 21 * MIN);
        let f = read_feed_of(&dir, "s1");
        assert_eq!(agent_of(&f, "inbar#828-r1").as_deref(), Some("w1"));
        assert_eq!(task_tokens(&f, "inbar#828-r1"), Some(50));
        assert_eq!(
            task_tokens(&f, "inbar#828"),
            Some(3_000),
            "frozen stays frozen"
        );
        assert_eq!(
            task_tokens(&f, "inbar#829"),
            Some(400),
            "frozen stays frozen"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A message to a worker busy on its own task hands nothing over unless
    /// it says it is a new task; a fresh spawn's row is left for its spawn.
    #[test]
    fn a_message_to_a_busy_worker_hands_nothing_over() {
        let dir = scratch("handoff-busy");
        run_cfg(&dir, "s1", "start inbar#900 --agent w1", None, T0);
        let cfg = spawned(&dir, "s1", "w1", "inbar#900: one");
        let log = cfg
            .join("projects")
            .join("-w")
            .join("s1")
            .join("subagents")
            .join("agent-w1.jsonl");
        run_cfg(&dir, "s1", "start inbar#901", Some(&cfg), T0 + 10 * MIN);
        let lines = [
            turn("a", T0 + MIN, 1_000),
            sent_at(
                T0 + 10 * MIN + 5_000,
                "FYI inbar#901 is going to a new worker.",
            ),
        ];
        std::fs::write(&log, lines.join("\n") + "\n").unwrap();
        let (said, _) = run_cfg(&dir, "s1", "eta inbar#900 5", Some(&cfg), T0 + 11 * MIN);
        assert!(!said.contains("handed to"), "{said}");
        let f = read_feed_of(&dir, "s1");
        assert_eq!(agent_of(&f, "inbar#901"), None);
        assert_eq!(f.rows[0].stage(), feed::Stage::Running, "w1 still on #900");

        // Said to be a new task, it is one: #900 lands at the message.
        std::fs::write(
            &log,
            [
                lines[0].clone(),
                lines[1].clone(),
                sent_at(T0 + 12 * MIN, "New task for you: inbar#901, drop #900."),
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        run_cfg(&dir, "s1", "eta inbar#901 5", Some(&cfg), T0 + 13 * MIN);
        let f = read_feed_of(&dir, "s1");
        assert_eq!(agent_of(&f, "inbar#901").as_deref(), Some("w1"));
        let r900 = f.rows.iter().find(|r| r.key == "inbar#900").unwrap();
        assert_eq!(r900.stage(), feed::Stage::Done);
        assert_eq!(r900.ended_ms, Some(T0 + 12 * MIN));
        assert_eq!(task_tokens(&f, "inbar#900"), Some(1_000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_lease_outlives_its_task() {
        let dir = scratch("lease-landed");
        run_cfg(&dir, "s1", "start t", None, T0);
        run_cfg(&dir, "s1", "claim t --cpu 3 --ram 3G", None, T0);
        // The row lands by a path that missed the ledger (an older writer,
        // an edit by hand): the lease is still there…
        assert!(edit_row(&dir, "s1", "t", |r| {
            r.insert("stage".into(), json!("done"));
        }));
        assert!(held(&dir, "s1", "t", T0 + MIN));
        // …until the session's next command or hook beat, which drops it
        // rather than renewing it.
        run_cfg(&dir, "s1", "show", None, T0 + 2 * MIN);
        assert!(!held(&dir, "s1", "t", T0 + 2 * MIN));
        run_cfg(&dir, "s1", "claim t2 --cpu 1", None, T0 + 2 * MIN);
        // A task landed with no row in the orchestrator session still gives its lease back.
        run_cfg(&dir, "s1", "claim u --cpu 1", None, T0 + 3 * MIN);
        let (cmd, flags) = parse_args(&args("land u")).unwrap();
        let err =
            run_in_code(&dir, "s1", &cmd, &flags, T0 + 4 * MIN, Some(&machine())).unwrap_err();
        assert!(err.contains("released 1 cpu"), "{err}");
        assert!(!held(&dir, "s1", "u", T0 + 4 * MIN));
        assert!(
            held(&dir, "s1", "t2", T0 + 4 * MIN),
            "a rowless claim is left alone"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn read_feed_of(dir: &Path, session: &str) -> feed::Feed {
        feed::read(&feed::feed_path(dir, session)).expect("a feed the pane reads")
    }

    #[test]
    fn a_session_id_is_a_file_name_not_a_path() {
        let dir = scratch("sid");
        let (cmd, flags) = parse_args(&args("plan a --eta 5")).unwrap();
        assert!(run_in(&dir, "../x", &cmd, &flags, T0).is_err());
        assert!(run_in(&dir, "", &cmd, &flags, T0).is_err());
    }

    /// giverny#105: Claude Code re-ids the session mid-run. The next
    /// command under the new id adopts the conversation's file — every row
    /// kept, none duplicated — and a `/clear` (a new root) starts afresh.
    #[test]
    fn a_re_id_session_adopts_its_conversations_feed() {
        use crate::continuation::tests::transcript;
        let dir = scratch("reid");
        let cfg = spawned(&dir, "old", "w1", "fix-a: the first task");
        transcript(&cfg, "old", "root-1");
        let c = Some(cfg.as_path());
        run_cfg(&dir, "old", "plan fix-a --eta 20 --title A", c, T0);
        run_cfg(&dir, "old", "plan fix-b --eta 5 --title B", c, T0);
        run_cfg(&dir, "old", "start fix-a --agent w1", c, T0 + MIN);
        run_cfg(&dir, "old", "plan fix-c --eta 3 --title C", c, T0);
        run_cfg(&dir, "old", "start fix-c", c, T0 + MIN);
        run_cfg(&dir, "old", "land fix-c", c, T0 + 2 * MIN);
        let old_file = feed::feed_path(&dir, "old");
        let doc: Value = serde_json::from_slice(&std::fs::read(&old_file).unwrap()).unwrap();
        assert_eq!(doc["root"], "root-1", "the root is recorded");

        // The same conversation under a new id (its records copied forward).
        transcript(&cfg, "new", "root-1");
        let (out, code) = run_cfg(&dir, "new", "eta fix-a 12", c, T0 + 3 * MIN);
        assert_eq!(code, 0, "{out}");
        assert!(!feed::feed_path(&dir, "new").exists(), "no second file");
        let f = feed::read(&old_file).unwrap();
        assert_eq!(f.session.as_deref(), Some("new"));
        assert_eq!(f.aliases, ["old"]);
        let keys: Vec<(&str, feed::Stage)> =
            f.rows.iter().map(|r| (r.key.as_str(), r.stage())).collect();
        assert_eq!(
            keys,
            [
                ("fix-a", feed::Stage::Running),
                ("fix-b", feed::Stage::Planned),
                ("fix-c", feed::Stage::Done)
            ]
        );
        let a = &f.rows[0];
        assert_eq!(a.title.as_deref(), Some("A"));
        assert_eq!(a.agent_id.as_deref(), Some("w1"));
        assert_eq!(a.started_ms, Some(T0 + MIN), "the row keeps its start");
        // Both ids find it: the pane's tab and a worker that kept the old one.
        assert_eq!(feed::find(&dir, "old").unwrap().0, old_file);
        assert_eq!(feed::find(&dir, "new").unwrap().0, old_file);
        // A worker holding the old id still lands its row in it.
        run_cfg(&dir, "old", "land fix-a", c, T0 + 4 * MIN);
        let f = feed::read(&old_file).unwrap();
        assert_eq!(f.rows[0].stage(), feed::Stage::Done);
        assert_eq!(
            f.session.as_deref(),
            Some("new"),
            "an alias does not take it back"
        );

        // `/clear`: a new conversation, a new file.
        transcript(&cfg, "cleared", "root-2");
        run_cfg(
            &dir,
            "cleared",
            "plan fix-d --eta 5 --title D",
            c,
            T0 + 5 * MIN,
        );
        let fresh = feed::read(&feed::feed_path(&dir, "cleared")).unwrap();
        assert_eq!(fresh.rows.len(), 1);
        assert!(fresh.aliases.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Another writer's feed is never adopted, whatever its root.
    #[test]
    fn a_re_id_leaves_another_writers_feed_alone() {
        use crate::continuation::tests::transcript;
        let dir = scratch("reid-other");
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("claude");
        transcript(&cfg, "old", "root-1");
        transcript(&cfg, "new", "root-1");
        let theirs = r#"{"version":1,"session":"old","root":"root-1","writer":"other/status-writer","rows":[]}"#;
        std::fs::write(feed::feed_path(&dir, "old"), theirs).unwrap();
        run_cfg(&dir, "new", "plan x --eta 5 --title X", Some(&cfg), T0);
        assert_eq!(
            std::fs::read_to_string(feed::feed_path(&dir, "old")).unwrap(),
            theirs
        );
        assert!(feed::feed_path(&dir, "new").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

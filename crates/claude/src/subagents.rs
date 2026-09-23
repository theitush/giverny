//! Subagents: the workers a tab's Claude session has spawned, as rows.
//!
//! The agents pane (off by default, `claude.agents_pane`) lists the subagents
//! of the Claude session in the tab you are looking at — the ones running and
//! the ones that finished — so a finished worker stays on screen until
//! `/clear` instead of vanishing thirty seconds after it lands. This module
//! is the model behind that list, and it needs no setup from the user: every
//! row comes from data Claude Code already writes.
//!
//! Three sources, each carrying what the others cannot:
//!
//! * **The live list.** Claude Code hands its `subagentStatusLine` command a
//!   JSON object on stdin at least every five seconds while workers run:
//!   `session_id`, and `tasks[]` with `id`, `type`, `status`, `description`,
//!   `label`, `startTime`, `model`, `tokenCount`, `cwd`, and `name` only when
//!   the agent was given one. [`LiveSnapshot::parse`] reads it. It is the
//!   authority on what is running *now*.
//! * **The worker's own transcript.**
//!   `projects/<munged cwd>/<parent session>/subagents/agent-<id>.jsonl`, with
//!   `agent-<id>.meta.json` beside it (`description`, `agentType`, `model`).
//!   The last `tool_use` in it is what the worker is doing ([`read_activity`]).
//! * **The parent's transcript.** When a worker stops, Claude Code queues a
//!   `<task-notification>` naming its `<task-id>` and a `<status>` —
//!   `completed`, `failed`, `killed` or `stopped` on every transcript on the
//!   machine this was written against. [`NotificationTail`] follows the parent
//!   transcript and collects them. A worker that is messaged again after it
//!   finished writes new lines to its transcript and gets a *new*
//!   notification when it stops again, so "done" is always "the latest
//!   notification is newer than the worker's latest line", never "a
//!   notification exists".
//!
//! [`Tracker`] folds the three into [`SubagentRow`]s for one tab. It is
//! serde-serialisable so the tab can persist it and a Giverny restart keeps
//! the Done rows.
//!
//! **Merging with the feed.** The optional feed (`feed.rs`) adds Planned
//! rows, titles, ETAs and landings. A feed row carrying `agent_id` merges with
//! the [`SubagentRow`] whose [`SubagentRow::id`] equals it — that string, the
//! agent id in `agent-<id>.jsonl` and in `tasks[].id`, is the one join key
//! and is never rewritten. The native row owns [`SubagentRow::started_ms`],
//! [`SubagentRow::tokens`] and [`SubagentRow::activity`]; the feed owns
//! title, ETA and landing. [`Tracker::get`] is the lookup the merge uses.
//!
//! Everything read here is Claude Code's private state and will change shape.
//! Fields are pulled out of `serde_json::Value` one at a time, as in
//! `jobs.rs`, so a surprise costs one field and never the row.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Field helpers (tolerant: a wrong type is an absent field)
// ---------------------------------------------------------------------------

fn as_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(Into::into)
}

fn as_u64(v: &Value, key: &str) -> Option<u64> {
    match v.get(key)? {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)),
        _ => None,
    }
}

/// Milliseconds since the epoch, from a number or an RFC 3339 string.
fn as_millis(v: &Value, key: &str) -> Option<u64> {
    match v.get(key)? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => parse_rfc3339_ms(s),
        _ => None,
    }
}

fn parse_rfc3339_ms(s: &str) -> Option<u64> {
    s.parse::<jiff::Timestamp>()
        .ok()
        .map(|t| t.as_millisecond().max(0) as u64)
}

// ---------------------------------------------------------------------------
// Stage and outcome
// ---------------------------------------------------------------------------

/// Where a native row stands. Planned rows exist only in the feed, so they
/// are not a stage here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Running,
    Done,
}

/// How a Done row ended, from the `<status>` of its notification or the
/// `status` of its live-list entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Completed,
    Failed,
    /// Killed by the user or the parent (`TaskStop`).
    Killed,
    Stopped,
    /// It left the live list and no notification has said how. The usual
    /// case for the few seconds between the two, and the permanent one when
    /// the parent transcript cannot be found.
    Unknown,
}

impl Outcome {
    /// Map a status word. `None` for the words that mean "still going".
    pub fn parse(s: &str) -> Option<Outcome> {
        match s {
            "running" | "pending" | "queued" | "starting" | "" => None,
            "completed" | "complete" | "done" | "finished" | "success" => Some(Outcome::Completed),
            "failed" | "error" | "errored" => Some(Outcome::Failed),
            "killed" | "cancelled" | "canceled" => Some(Outcome::Killed),
            "stopped" => Some(Outcome::Stopped),
            _ => Some(Outcome::Unknown),
        }
    }

    /// Anything but a clean completion — worth a warning colour.
    pub fn is_bad(self) -> bool {
        matches!(self, Outcome::Failed | Outcome::Killed)
    }
}

// ---------------------------------------------------------------------------
// The live list (subagentStatusLine stdin)
// ---------------------------------------------------------------------------

/// One `tasks[]` entry of the `subagentStatusLine` input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveTask {
    /// The agent id — `agent-<id>.jsonl`, and the merge key.
    pub id: String,
    /// `local_agent` for a subagent; carried through as-is.
    pub kind: Option<String>,
    /// Raw status word (`running`, `completed`, …).
    pub status: String,
    /// The Agent tool's `description`.
    pub description: Option<String>,
    /// Claude Code's own one-line "what it's doing" (often empty).
    pub label: Option<String>,
    /// Present only for an agent that was given a name.
    pub name: Option<String>,
    pub start_ms: Option<u64>,
    pub model: Option<String>,
    pub tokens: Option<u64>,
    pub cwd: Option<PathBuf>,
}

impl LiveTask {
    fn from_value(v: &Value) -> Option<LiveTask> {
        Some(LiveTask {
            id: as_str(v, "id")?,
            kind: as_str(v, "type"),
            status: as_str(v, "status").unwrap_or_default(),
            description: as_str(v, "description"),
            label: as_str(v, "label"),
            name: as_str(v, "name"),
            start_ms: as_millis(v, "startTime"),
            model: as_str(v, "model"),
            tokens: as_u64(v, "tokenCount"),
            cwd: as_str(v, "cwd").map(PathBuf::from),
        })
    }

    /// Still going, by its own status word.
    pub fn is_running(&self) -> bool {
        Outcome::parse(&self.status).is_none()
    }
}

/// One `subagentStatusLine` input: whose session, and its visible workers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveSnapshot {
    pub session_id: Option<String>,
    pub tasks: Vec<LiveTask>,
}

impl LiveSnapshot {
    /// Parse the stdin JSON. Never fails: garbage is an empty snapshot, an
    /// entry with no `id` is skipped.
    pub fn parse(json: &str) -> LiveSnapshot {
        serde_json::from_str::<Value>(json)
            .map(|v| LiveSnapshot::from_value(&v))
            .unwrap_or_default()
    }

    pub fn from_value(v: &Value) -> LiveSnapshot {
        LiveSnapshot {
            session_id: as_str(v, "session_id"),
            tasks: v
                .get("tasks")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(LiveTask::from_value).collect())
                .unwrap_or_default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Files on disk
// ---------------------------------------------------------------------------

/// The `subagents/` directory of a parent session:
/// `projects/<munged cwd>/<session>/subagents`. Found by scanning project
/// dirs, since the munging is lossy (as `registry::find_transcript`).
pub fn subagents_dir(config_dir: &Path, parent_session: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(config_dir.join("projects"))
        .ok()?
        .flatten()
    {
        let candidate = entry.path().join(parent_session).join("subagents");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// `agent-<id>.jsonl` inside a `subagents/` dir.
pub fn agent_transcript(subagents_dir: &Path, id: &str) -> PathBuf {
    subagents_dir.join(format!("agent-{id}.jsonl"))
}

/// Agent ids with a transcript in a `subagents/` dir.
pub fn list_agent_ids(subagents_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(subagents_dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_prefix("agent-")?
                .strip_suffix(".jsonl")
                .map(str::to_string)
        })
        .collect();
    ids.sort();
    ids
}

/// `agent-<id>.meta.json`: what the spawn said about the worker.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentMeta {
    pub description: Option<String>,
    pub agent_type: Option<String>,
    /// Only when the spawn passed one.
    pub model: Option<String>,
}

pub fn read_meta(subagents_dir: &Path, id: &str) -> AgentMeta {
    let path = subagents_dir.join(format!("agent-{id}.meta.json"));
    let Some(v) = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return AgentMeta::default();
    };
    AgentMeta {
        description: as_str(&v, "description"),
        agent_type: as_str(&v, "agentType"),
        model: as_str(&v, "model"),
    }
}

/// What a worker's transcript says about it, read from its tail.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    /// The last tool call, one line: `Bash: cargo test`, `Read: src/lib.rs`.
    pub doing: Option<String>,
    /// Timestamp of the newest *assistant* line — the worker's last turn.
    /// Only its own turns count: a background shell it left running can
    /// land a notification line in its transcript after it has finished,
    /// and that is not the worker working (seen on real transcripts).
    pub last_turn_ms: Option<u64>,
    /// Context size at the last assistant turn (input + cache + output).
    pub tokens: Option<u64>,
    /// The model the last assistant turn ran on.
    pub model: Option<String>,
}

/// How far back from the end [`read_activity`] looks. A tool result can be
/// large, so this is generous; a transcript whose last tool call is further
/// back than this reports no activity, which is honest enough.
const TAIL_BYTES: u64 = 512 * 1024;

/// Read the tail of an agent transcript.
pub fn read_activity(path: &Path) -> Activity {
    let Ok(buf) = read_tail(path, TAIL_BYTES) else {
        return Activity::default();
    };
    activity_from_lines(buf.lines())
}

fn read_tail(path: &Path, max: u64) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(max).read_to_end(&mut bytes)?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        // The first line is cut; drop it rather than half-parse it.
        match text.find('\n') {
            Some(i) => {
                text.drain(..=i);
            }
            None => text.clear(),
        }
    }
    Ok(text)
}

fn activity_from_lines<'a>(lines: impl DoubleEndedIterator<Item = &'a str>) -> Activity {
    let mut act = Activity::default();
    for line in lines.rev() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if act.last_turn_ms.is_none() {
            act.last_turn_ms = as_millis(&v, "timestamp");
        }
        let Some(msg) = v.get("message") else {
            continue;
        };
        if act.tokens.is_none()
            && let Some(u) = msg.get("usage")
        {
            let t: u64 = [
                "input_tokens",
                "cache_creation_input_tokens",
                "cache_read_input_tokens",
                "output_tokens",
            ]
            .iter()
            .filter_map(|k| as_u64(u, k))
            .sum();
            act.tokens = (t > 0).then_some(t);
        }
        if act.model.is_none() {
            act.model = as_str(msg, "model");
        }
        if act.doing.is_none()
            && let Some(items) = msg.get("content").and_then(Value::as_array)
            && let Some(tu) = items
                .iter()
                .rev()
                .find(|c| c.get("type").and_then(Value::as_str) == Some("tool_use"))
        {
            act.doing = Some(describe_tool_use(tu));
        }
        if act.doing.is_some() && act.tokens.is_some() && act.model.is_some() {
            break;
        }
    }
    act
}

/// Longest `doing` line, in characters.
const DOING_MAX: usize = 80;

/// `Tool: the argument that says what it is doing`, one line.
pub fn describe_tool_use(tool_use: &Value) -> String {
    let name = as_str(tool_use, "name").unwrap_or_else(|| "tool".into());
    let input = tool_use.get("input").unwrap_or(&Value::Null);
    let arg = [
        "description",
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "query",
        "skill",
        "to",
        "prompt",
    ]
    .iter()
    .find_map(|k| as_str(input, k));
    let line = match arg {
        Some(a) => format!("{name}: {a}"),
        None => name,
    };
    one_line(&line, DOING_MAX)
}

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        flat.chars().take(max - 1).collect::<String>() + "…"
    } else {
        flat
    }
}

// ---------------------------------------------------------------------------
// Completion notifications in the parent transcript
// ---------------------------------------------------------------------------

/// A `<task-notification>` for one worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completion {
    pub outcome: Outcome,
    /// When the notification was queued — the landing time.
    pub at_ms: u64,
    /// `<summary>`: `Agent "…" finished`.
    pub summary: Option<String>,
}

/// Pull `<tag>…</tag>` out of `text`.
fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = start + text[start..].find(&close)?;
    Some(text[start..end].trim())
}

/// Parse one transcript line for a notification. The same notification is
/// written up to three times (queue enqueue, attachment, user turn) with
/// timestamps milliseconds apart; the caller keeps the latest.
fn completion_in_line(line: &str) -> Option<(String, Completion)> {
    if line.contains("\"toolUseResult\"")
        && line.contains("\"agentId\"")
        && let Some(found) = foreground_result(line)
    {
        return Some(found);
    }
    if !line.contains("<task-notification>") {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    // A dequeue ("remove") is the queue letting go, not a new landing.
    if v.get("operation").and_then(Value::as_str) == Some("remove") {
        return None;
    }
    let at_ms = as_millis(&v, "timestamp")?;
    // The text sits in one of three places depending on the line's type; a
    // string search over the serialised value finds it in all of them
    // without binding to any.
    let texts = collect_strings(&v);
    let text = texts.iter().find(|s| s.contains("<task-notification>"))?;
    let body = tag(text, "task-notification")?;
    let id = tag(body, "task-id")?.to_string();
    if id.is_empty() {
        return None;
    }
    let outcome = tag(body, "status")
        .and_then(Outcome::parse)
        .unwrap_or(Outcome::Unknown);
    let summary = tag(body, "summary").map(str::to_string);
    Some((
        id,
        Completion {
            outcome,
            at_ms,
            summary,
        },
    ))
}

/// A foreground worker sends no notification: the Agent tool call simply
/// returns, and the parent's `tool_result` line carries `toolUseResult` with
/// the `agentId` and a `status`. A background spawn writes the same shape at
/// launch with `status: "async_launched"`, which is not a landing.
fn foreground_result(line: &str) -> Option<(String, Completion)> {
    let v: Value = serde_json::from_str(line).ok()?;
    let r = v.get("toolUseResult")?;
    let id = as_str(r, "agentId")?;
    let status = as_str(r, "status").unwrap_or_default();
    if status == "async_launched" {
        return None;
    }
    let outcome = Outcome::parse(&status).unwrap_or(Outcome::Unknown);
    Some((
        id,
        Completion {
            outcome,
            at_ms: as_millis(&v, "timestamp")?,
            summary: None,
        },
    ))
}

fn collect_strings(v: &Value) -> Vec<&str> {
    let mut out = Vec::new();
    let mut stack = vec![v];
    while let Some(v) = stack.pop() {
        match v {
            Value::String(s) => out.push(s.as_str()),
            Value::Array(a) => stack.extend(a),
            Value::Object(o) => stack.extend(o.values()),
            _ => {}
        }
    }
    out
}

/// Follows a parent transcript and collects the latest landing per worker:
/// a `<task-notification>` for a background worker, the Agent tool's result
/// for a foreground one. [`NotificationTail::poll`] reads only what was appended since the
/// last call, so polling a long session every second costs a `stat`.
///
/// Background shells (`run_in_background`) notify through the same channel;
/// their ids are not agent ids, so they never match a row and are harmless.
#[derive(Debug, Clone, Default)]
pub struct NotificationTail {
    path: PathBuf,
    offset: u64,
    partial: String,
    latest: HashMap<String, Completion>,
}

impl NotificationTail {
    pub fn new(path: impl Into<PathBuf>) -> NotificationTail {
        NotificationTail {
            path: path.into(),
            ..NotificationTail::default()
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read what was appended. Returns true when anything new was learned.
    /// A file that shrank (rewritten) is read again from the start.
    pub fn poll(&mut self) -> bool {
        let Ok(mut file) = std::fs::File::open(&self.path) else {
            return false;
        };
        let Ok(len) = file.metadata().map(|m| m.len()) else {
            return false;
        };
        if len < self.offset {
            self.offset = 0;
            self.partial.clear();
            self.latest.clear();
        }
        if len == self.offset || file.seek(SeekFrom::Start(self.offset)).is_err() {
            return false;
        }
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return false;
        }
        self.offset += bytes.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&bytes));
        let Some(cut) = self.partial.rfind('\n') else {
            return false;
        };
        let complete: String = self.partial.drain(..=cut).collect();
        let mut changed = false;
        for line in complete.lines() {
            if let Some((id, c)) = completion_in_line(line) {
                let newer = self.latest.get(&id).is_none_or(|old| c.at_ms > old.at_ms);
                if newer {
                    self.latest.insert(id, c);
                    changed = true;
                }
            }
        }
        changed
    }

    /// Latest notification per task id.
    pub fn completions(&self) -> &HashMap<String, Completion> {
        &self.latest
    }
}

/// Every notification in a whole transcript, latest per id. A one-shot
/// [`NotificationTail`].
pub fn read_completions(parent_transcript: &Path) -> HashMap<String, Completion> {
    let mut tail = NotificationTail::new(parent_transcript);
    tail.poll();
    tail.latest
}

/// A worker is done when its latest notification is at least as new as its
/// latest turn. A turn after a notification means it was messaged again and
/// is working. `SLACK_MS` absorbs the few milliseconds by which the final
/// turn and the notification can land out of order.
pub fn is_finished(completion: &Completion, last_turn_ms: Option<u64>) -> bool {
    const SLACK_MS: u64 = 2_000;
    match last_turn_ms {
        Some(last) => completion.at_ms + SLACK_MS >= last,
        None => true,
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// One subagent, as the pane shows it. The stable type the pane (build task
/// C) and the relay (task B) consume; fields are only ever added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentRow {
    /// Agent id: `agent-<id>.jsonl`, `tasks[].id`, `<task-id>`. The join key
    /// with the feed's `agent_id`. Never rewritten.
    pub id: String,
    pub stage: Stage,
    /// Set once Done.
    #[serde(default)]
    pub outcome: Option<Outcome>,
    /// A name, when the agent was given one.
    #[serde(default)]
    pub name: Option<String>,
    /// The Agent tool's `description` — what the pane shows without a feed.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Epoch ms; ELAPSED is drawn from this every second.
    #[serde(default)]
    pub started_ms: Option<u64>,
    /// Epoch ms it landed (notification time, else when it left the list).
    #[serde(default)]
    pub ended_ms: Option<u64>,
    #[serde(default)]
    pub tokens: Option<u64>,
    /// What it is doing now: the live `label` when Claude Code gave one,
    /// else the last tool call in its transcript. Cleared on Done.
    #[serde(default)]
    pub activity: Option<String>,
    /// Its `agent-<id>.jsonl`, once known — what a click opens.
    #[serde(default)]
    pub transcript: Option<PathBuf>,
    /// Its last assistant turn (epoch ms), for revival detection.
    #[serde(default)]
    pub last_turn_ms: Option<u64>,
    /// Summary line of its last notification.
    #[serde(default)]
    pub summary: Option<String>,
}

impl SubagentRow {
    fn new(id: &str, stage: Stage) -> SubagentRow {
        SubagentRow {
            id: id.to_string(),
            stage,
            outcome: None,
            name: None,
            description: None,
            agent_type: None,
            model: None,
            started_ms: None,
            ended_ms: None,
            tokens: None,
            activity: None,
            transcript: None,
            last_turn_ms: None,
            summary: None,
        }
    }

    /// The join key with the feed's `agent_id` (same as [`SubagentRow::id`]).
    /// With [`SubagentRow::running`], [`SubagentRow::started_ms`] and
    /// [`SubagentRow::tokens`] this is everything `feed::LiveAgent` asks of a
    /// live row, so its impl is one line per method.
    pub fn agent_id(&self) -> &str {
        &self.id
    }

    /// Still running (else it has finished).
    pub fn running(&self) -> bool {
        self.stage == Stage::Running
    }

    /// Name, else description, else the id — never empty.
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .or(self.description.as_deref())
            .unwrap_or(&self.id)
    }

    /// Wall time so far (Running) or taken (Done), in ms.
    pub fn elapsed_ms(&self, now_ms: u64) -> Option<u64> {
        let start = self.started_ms?;
        let end = match self.stage {
            Stage::Running => now_ms,
            Stage::Done => self.ended_ms.unwrap_or(now_ms),
        };
        Some(end.saturating_sub(start))
    }

    fn mark_done(&mut self, outcome: Outcome, at_ms: u64) {
        self.stage = Stage::Done;
        // A known outcome is never downgraded to Unknown.
        if self.outcome.is_none() || outcome != Outcome::Unknown {
            self.outcome = Some(outcome);
        }
        if self.ended_ms.is_none() || outcome != Outcome::Unknown {
            self.ended_ms = Some(at_ms);
        }
        self.activity = None;
    }

    fn revive(&mut self) {
        self.stage = Stage::Running;
        self.outcome = None;
        self.ended_ms = None;
        self.summary = None;
    }

    fn fill_meta(&mut self, meta: AgentMeta) {
        if self.description.is_none() {
            self.description = meta.description;
        }
        if self.agent_type.is_none() {
            self.agent_type = meta.agent_type;
        }
        if self.model.is_none() {
            self.model = meta.model;
        }
    }
}

/// The subagents of one tab's Claude session, Running and Done.
///
/// Feed it [`LiveSnapshot`]s as the relay delivers them
/// ([`Tracker::apply_live`]) and call [`Tracker::refresh`] about once a
/// second; read [`Tracker::rows`]. Nothing ages out — Done rows stay until
/// [`Tracker::clear`] (the tab's `/clear`, or `SessionStart` with
/// `source=clear`).
///
/// Serialise it with the tab; the on-disk readers (`subagents_dir`, the
/// notification offset) are rebuilt after a restart, from the stored session
/// and config dir, on the first `refresh`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tracker {
    /// Parent session id: the one the rows belong to, the latest seen.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Earlier ids of the same conversation (a resume re-ids it); rows
    /// survive the change.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Claude config dir the session runs under.
    #[serde(default)]
    pub config_dir: Option<PathBuf>,
    rows: Vec<SubagentRow>,
    #[serde(skip)]
    dirs: Vec<PathBuf>,
    #[serde(skip)]
    tails: Vec<NotificationTail>,
    /// Sessions whose `subagents/` dir has been found.
    #[serde(skip)]
    resolved: Vec<String>,
}

impl Tracker {
    pub fn new(config_dir: Option<PathBuf>) -> Tracker {
        Tracker {
            config_dir,
            ..Tracker::default()
        }
    }

    /// All rows: Running first (oldest start first), then Done (most recent
    /// landing first).
    pub fn rows(&self) -> &[SubagentRow] {
        &self.rows
    }

    /// The row for an agent id — the feed merge's lookup.
    pub fn get(&self, id: &str) -> Option<&SubagentRow> {
        self.rows.iter().find(|r| r.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Forget every row (the tab's `/clear`). Keeps the session binding.
    pub fn clear(&mut self) {
        self.rows.clear();
    }

    /// Bind to a session. A different id keeps the rows and records the old
    /// one as an alias — a resumed or re-id'd conversation is still the same
    /// pass; call [`Tracker::clear`] as well if it is not.
    pub fn set_session(&mut self, session_id: &str) {
        if let Some(cur) = &self.session_id {
            if cur == session_id {
                return;
            }
            if !self.aliases.contains(cur) {
                self.aliases.push(cur.clone());
            }
        }
        self.aliases.retain(|a| a != session_id);
        self.session_id = Some(session_id.to_string());
    }

    fn row_mut(&mut self, id: &str, stage: Stage) -> &mut SubagentRow {
        if let Some(i) = self.rows.iter().position(|r| r.id == id) {
            &mut self.rows[i]
        } else {
            self.rows.push(SubagentRow::new(id, stage));
            self.rows.last_mut().expect("just pushed")
        }
    }

    /// Fold in one live list. Every listed task becomes (or stays) a row;
    /// a Running row absent from the list is Done as of `now_ms`, with its
    /// outcome to be filled by a notification.
    pub fn apply_live(&mut self, snap: &LiveSnapshot, now_ms: u64) {
        if let Some(sid) = &snap.session_id {
            self.set_session(sid);
        }
        for t in &snap.tasks {
            let row = self.row_mut(&t.id, Stage::Running);
            if t.name.is_some() {
                row.name = t.name.clone();
            }
            if t.description.is_some() {
                row.description = t.description.clone();
            }
            if t.model.is_some() {
                row.model = t.model.clone();
            }
            if t.start_ms.is_some() {
                row.started_ms = t.start_ms;
            }
            if t.tokens.is_some() {
                row.tokens = t.tokens;
            }
            match Outcome::parse(&t.status) {
                None => {
                    if row.stage == Stage::Done {
                        row.revive();
                    }
                    if t.label.is_some() {
                        row.activity = t.label.clone();
                    }
                }
                Some(o) => row.mark_done(o, now_ms),
            }
        }
        for row in &mut self.rows {
            if row.stage == Stage::Running && !snap.tasks.iter().any(|t| t.id == row.id) {
                row.mark_done(Outcome::Unknown, now_ms);
            }
        }
        self.sort();
    }

    /// Read the disk: find the session's `subagents/` dir and transcript,
    /// pick up new notifications, refresh each row's activity, meta and
    /// transcript path, and add Done rows for workers that finished while
    /// nothing was watching (Giverny closed, or the pane just switched on).
    ///
    /// Cheap enough for once a second: one `read_dir`, an appended-bytes read
    /// of the parent transcript, and a tail read per Running row.
    pub fn refresh(&mut self) {
        let Some(config) = self.config_dir.clone() else {
            return;
        };
        // Resolve each session's dir once; one not found yet (no worker
        // spawned so far) is looked for again next time.
        let sessions: Vec<String> = self
            .session_id
            .iter()
            .chain(self.aliases.iter())
            .filter(|s| !self.resolved.contains(s))
            .cloned()
            .collect();
        for sid in sessions {
            if let Some(dir) = subagents_dir(&config, &sid) {
                if let Some(parent) = dir.parent().and_then(Path::parent) {
                    self.tails
                        .push(NotificationTail::new(parent.join(format!("{sid}.jsonl"))));
                }
                self.dirs.push(dir);
                self.resolved.push(sid);
            }
        }
        for tail in &mut self.tails {
            tail.poll();
        }
        let mut completions: HashMap<String, Completion> = HashMap::new();
        for tail in &self.tails {
            for (id, c) in tail.completions() {
                if completions.get(id).is_none_or(|old| c.at_ms > old.at_ms) {
                    completions.insert(id.clone(), c.clone());
                }
            }
        }
        let dirs = self.dirs.clone();
        for dir in &dirs {
            for id in list_agent_ids(dir) {
                let known = self.rows.iter().any(|r| r.id == id);
                let completion = completions.get(&id);
                if !known && completion.is_none() {
                    // Not in the live list and never notified: a worker the
                    // live list will announce, or one killed with its parent.
                    // Either way not ours to invent.
                    continue;
                }
                let needs_read = !known
                    || self.rows.iter().any(|r| {
                        r.id == id && (r.stage == Stage::Running || r.transcript.is_none())
                    });
                if !needs_read && completion.is_none() {
                    continue;
                }
                let path = agent_transcript(dir, &id);
                let row_is_running = self
                    .rows
                    .iter()
                    .any(|r| r.id == id && r.stage == Stage::Running);
                let act = if needs_read || row_is_running {
                    Some(read_activity(&path))
                } else {
                    None
                };
                let meta = (!known).then(|| read_meta(dir, &id));
                // A worker new to us is Done unless it has written since its
                // notification: messaged again and still going, with no live
                // entry yet — Running until the list or the next
                // notification says otherwise.
                let initial = match (completion, &act) {
                    (Some(c), Some(a)) if !is_finished(c, a.last_turn_ms) => Stage::Running,
                    _ => Stage::Done,
                };
                let row = self.row_mut(&id, initial);
                row.transcript = Some(path);
                if let Some(meta) = meta {
                    row.fill_meta(meta);
                }
                if let Some(act) = act {
                    if act.last_turn_ms.is_some() {
                        row.last_turn_ms = act.last_turn_ms;
                    }
                    if row.model.is_none() {
                        row.model = act.model;
                    }
                    if row.stage == Stage::Running {
                        // Live label wins when it said something; the
                        // transcript's last tool call otherwise.
                        if act.doing.is_some() {
                            row.activity = act.doing;
                        }
                        if row.tokens.is_none() {
                            row.tokens = act.tokens;
                        }
                    } else if row.tokens.is_none() {
                        row.tokens = act.tokens;
                    }
                }
                if !known && row.started_ms.is_none() {
                    row.started_ms = first_line_ms(&agent_transcript(dir, &id));
                }
                if let Some(c) = completion
                    && is_finished(c, row.last_turn_ms)
                {
                    row.mark_done(c.outcome, c.at_ms);
                    row.summary = c.summary.clone();
                }
            }
        }
        self.sort();
    }

    fn sort(&mut self) {
        self.rows.sort_by(|a, b| match (a.stage, b.stage) {
            (Stage::Running, Stage::Done) => std::cmp::Ordering::Less,
            (Stage::Done, Stage::Running) => std::cmp::Ordering::Greater,
            (Stage::Running, Stage::Running) => a
                .started_ms
                .unwrap_or(u64::MAX)
                .cmp(&b.started_ms.unwrap_or(u64::MAX))
                .then_with(|| a.id.cmp(&b.id)),
            (Stage::Done, Stage::Done) => b.ended_ms.cmp(&a.ended_ms).then_with(|| a.id.cmp(&b.id)),
        });
    }
}

/// Timestamp of the first line of a transcript — its start, when no live
/// list ever said.
fn first_line_ms(path: &Path) -> Option<u64> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file).lines().take(20) {
        let Ok(line) = line else { break };
        if let Ok(v) = serde_json::from_str::<Value>(&line)
            && let Some(ms) = as_millis(&v, "timestamp")
        {
            return Some(ms);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("giverny-subagents-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    const T0: u64 = 1_790_000_000_000;

    fn iso(ms: u64) -> String {
        jiff::Timestamp::from_millisecond(ms as i64)
            .unwrap()
            .to_string()
    }

    fn live(session: &str, tasks: &[(&str, &str, u64)]) -> LiveSnapshot {
        let tasks: Vec<Value> = tasks
            .iter()
            .map(|(id, status, start)| {
                serde_json::json!({
                    "id": id, "type": "local_agent", "status": status,
                    "description": format!("Work {id}"), "label": "",
                    "startTime": start, "model": "claude-opus-5[1m]",
                    "tokenCount": 1000, "cwd": "/w"
                })
            })
            .collect();
        LiveSnapshot::from_value(&serde_json::json!({"session_id": session, "tasks": tasks}))
    }

    fn assistant_line(ms: u64, tool: Option<(&str, Value)>) -> String {
        let mut content = vec![serde_json::json!({"type": "text", "text": "hi"})];
        if let Some((name, input)) = tool {
            content.push(
                serde_json::json!({"type": "tool_use", "id": "t", "name": name, "input": input}),
            );
        }
        serde_json::json!({
            "type": "assistant", "timestamp": iso(ms),
            "message": {"model": "claude-opus-5-5", "content": content,
                        "usage": {"input_tokens": 2, "cache_read_input_tokens": 100,
                                  "cache_creation_input_tokens": 8, "output_tokens": 10}}
        })
        .to_string()
    }

    fn notification_lines(id: &str, status: &str, ms: u64) -> String {
        let text = format!(
            "<task-notification>\n<task-id>{id}</task-id>\n<status>{status}</status>\n<summary>Agent \"x\" finished</summary>\n</task-notification>"
        );
        [
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "timestamp": iso(ms), "content": text}),
            serde_json::json!({"type": "queue-operation", "operation": "remove", "timestamp": iso(ms + 900), "content": text}),
            serde_json::json!({"type": "user", "timestamp": iso(ms + 30), "message": {"role": "user", "content": text}}),
        ]
        .iter()
        .map(|v| v.to_string() + "\n")
        .collect()
    }

    /// config/projects/-w/<sid>.jsonl + <sid>/subagents/
    fn layout(name: &str, sid: &str) -> (PathBuf, PathBuf, PathBuf) {
        let config = scratch(name);
        let proj = config.join("projects").join("-w");
        let subs = proj.join(sid).join("subagents");
        std::fs::create_dir_all(&subs).unwrap();
        let parent = proj.join(format!("{sid}.jsonl"));
        std::fs::write(&parent, "{\"type\":\"user\"}\n").unwrap();
        (config, subs, parent)
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn parses_live_snapshot_tolerantly() {
        let snap = LiveSnapshot::parse(
            r#"{"session_id":"s1","columns":179,"tasks":[
                {"id":"a1","type":"local_agent","status":"running","description":"Work coo#74",
                 "label":"Grepping","startTime":1790089835667,"model":"m","tokenCount":124832,"cwd":"/w"},
                {"id":"a2","status":"completed","startTime":"2026-09-23T08:00:00Z","tokenCount":"oops"},
                {"status":"running"}
            ]}"#,
        );
        assert_eq!(snap.session_id.as_deref(), Some("s1"));
        assert_eq!(snap.tasks.len(), 2, "entry with no id skipped");
        assert_eq!(snap.tasks[0].label.as_deref(), Some("Grepping"));
        assert_eq!(snap.tasks[0].tokens, Some(124832));
        assert!(snap.tasks[0].is_running());
        assert!(!snap.tasks[1].is_running());
        assert!(snap.tasks[1].start_ms.is_some(), "RFC 3339 start accepted");
        assert_eq!(
            snap.tasks[1].tokens, None,
            "wrong type costs the field only"
        );
        assert_eq!(LiveSnapshot::parse("not json"), LiveSnapshot::default());
    }

    #[test]
    fn describes_tool_calls_on_one_line() {
        let tu = serde_json::json!({"type":"tool_use","name":"Bash",
            "input":{"command":"cargo test\n  --workspace","description":"Run the tests"}});
        assert_eq!(describe_tool_use(&tu), "Bash: Run the tests");
        let tu =
            serde_json::json!({"type":"tool_use","name":"Read","input":{"file_path":"/a/b.rs"}});
        assert_eq!(describe_tool_use(&tu), "Read: /a/b.rs");
        let tu = serde_json::json!({"type":"tool_use","name":"TodoWrite","input":{"todos":[]}});
        assert_eq!(describe_tool_use(&tu), "TodoWrite");
        let long = "x".repeat(200);
        let tu = serde_json::json!({"name":"Grep","input":{"pattern": long}});
        assert_eq!(describe_tool_use(&tu).chars().count(), DOING_MAX);
    }

    #[test]
    fn activity_is_the_last_tool_call() {
        let (_c, subs, _p) = layout("activity", "s1");
        let path = agent_transcript(&subs, "a1");
        let lines = [
            assistant_line(T0, Some(("Read", serde_json::json!({"file_path": "/x"})))),
            serde_json::json!({"type":"user","timestamp": iso(T0 + 1000)}).to_string(),
            assistant_line(
                T0 + 2000,
                Some(("Bash", serde_json::json!({"command": "ls"}))),
            ),
            assistant_line(T0 + 3000, None),
            serde_json::json!({"type":"attachment","timestamp": iso(T0 + 4000)}).to_string(),
            "garbage".into(),
        ];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let act = read_activity(&path);
        assert_eq!(act.doing.as_deref(), Some("Bash: ls"));
        assert_eq!(
            act.last_turn_ms,
            Some(T0 + 3000),
            "attachments are not turns"
        );
        assert_eq!(act.tokens, Some(120));
        assert_eq!(act.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(
            read_activity(&subs.join("missing.jsonl")),
            Activity::default()
        );
    }

    #[test]
    fn notifications_keep_the_latest_per_id() {
        let (_c, _s, parent) = layout("notif", "s1");
        append(&parent, &notification_lines("a1", "completed", T0));
        append(&parent, &notification_lines("a2", "failed", T0 + 10));
        append(&parent, &notification_lines("a1", "killed", T0 + 5000));
        let got = read_completions(&parent);
        assert_eq!(got.len(), 2);
        assert_eq!(got["a1"].outcome, Outcome::Killed);
        assert_eq!(
            got["a1"].at_ms,
            T0 + 5030,
            "latest of the copies, dequeue ignored"
        );
        assert_eq!(got["a2"].outcome, Outcome::Failed);
        assert_eq!(got["a2"].summary.as_deref(), Some("Agent \"x\" finished"));
    }

    #[test]
    fn foreground_results_land_and_async_launches_do_not() {
        let (_c, _s, parent) = layout("fg", "s1");
        let line = |id: &str, status: &str, ms: u64| {
            serde_json::json!({"type": "user", "timestamp": iso(ms),
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t"}]},
                "toolUseResult": {"status": status, "agentId": id, "totalTokens": 5}})
            .to_string()
                + "\n"
        };
        append(&parent, &line("fg1", "completed", T0));
        append(&parent, &line("bg1", "async_launched", T0));
        let got = read_completions(&parent);
        assert_eq!(got.len(), 1);
        assert_eq!(got["fg1"].outcome, Outcome::Completed);
        assert_eq!(got["fg1"].at_ms, T0);
    }

    #[test]
    fn notification_tail_reads_only_appended_complete_lines() {
        let (_c, _s, parent) = layout("tail", "s1");
        let mut tail = NotificationTail::new(&parent);
        assert!(!tail.poll());
        let lines = notification_lines("a1", "completed", T0);
        let (head, rest) = lines.split_at(40);
        append(&parent, head);
        assert!(!tail.poll(), "half a line is held back");
        append(&parent, rest);
        assert!(tail.poll());
        assert_eq!(tail.completions()["a1"].outcome, Outcome::Completed);
        assert!(!tail.poll(), "nothing new");
        // Rewritten shorter: start over.
        std::fs::write(&parent, "{}\n").unwrap();
        tail.poll();
        assert!(tail.completions().is_empty());
    }

    #[test]
    fn finished_means_notified_after_the_last_turn() {
        let c = Completion {
            outcome: Outcome::Completed,
            at_ms: T0,
            summary: None,
        };
        assert!(is_finished(&c, Some(T0 - 5000)));
        assert!(is_finished(&c, Some(T0 + 500)), "within slack");
        assert!(!is_finished(&c, Some(T0 + 60_000)), "messaged again");
        assert!(is_finished(&c, None));
    }

    #[test]
    fn live_list_drives_running_and_done() {
        let mut t = Tracker::new(None);
        t.apply_live(
            &live("s1", &[("a1", "running", T0), ("a2", "running", T0 - 10)]),
            T0 + 1,
        );
        assert_eq!(t.rows().len(), 2);
        assert_eq!(t.rows()[0].id, "a2", "running rows oldest start first");
        assert!(t.rows().iter().all(|r| r.stage == Stage::Running));

        // a2 leaves the list: Done, outcome unknown until notified.
        t.apply_live(&live("s1", &[("a1", "running", T0)]), T0 + 60_000);
        let a2 = t.get("a2").unwrap();
        assert_eq!(a2.stage, Stage::Done);
        assert_eq!(a2.outcome, Some(Outcome::Unknown));
        assert_eq!(a2.ended_ms, Some(T0 + 60_000));
        assert_eq!(
            a2.elapsed_ms(T0 + 999_999),
            Some(60_010),
            "Done elapsed stops"
        );

        // a1 lists as completed: Done.
        t.apply_live(&live("s1", &[("a1", "completed", T0)]), T0 + 70_000);
        assert_eq!(t.get("a1").unwrap().outcome, Some(Outcome::Completed));

        // An empty list does not evict Done rows — nothing ages out.
        t.apply_live(&live("s1", &[]), T0 + 10_000_000);
        assert_eq!(t.rows().len(), 2);

        // Revived by a message: Running again.
        t.apply_live(&live("s1", &[("a2", "running", T0 - 10)]), T0 + 10_000_001);
        let a2 = t.get("a2").unwrap();
        assert_eq!(a2.stage, Stage::Running);
        assert_eq!(a2.ended_ms, None);
        assert_eq!(t.rows()[0].id, "a2");

        t.clear();
        assert!(t.is_empty());
        assert_eq!(t.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn session_re_id_keeps_rows_and_records_alias() {
        let mut t = Tracker::new(None);
        t.apply_live(&live("s1", &[("a1", "running", T0)]), T0);
        t.apply_live(&live("s2", &[("a1", "running", T0)]), T0 + 1);
        assert_eq!(t.session_id.as_deref(), Some("s2"));
        assert_eq!(t.aliases, vec!["s1".to_string()]);
        assert_eq!(t.rows().len(), 1);
        t.set_session("s1");
        assert_eq!(
            t.aliases,
            vec!["s2".to_string()],
            "no duplicates, current never an alias"
        );
    }

    #[test]
    fn refresh_fills_from_disk_and_notifications() {
        let (config, subs, parent) = layout("refresh", "s1");
        // a1: running, known from the live list.
        std::fs::write(
            agent_transcript(&subs, "a1"),
            assistant_line(
                T0 + 1000,
                Some(("Grep", serde_json::json!({"pattern": "fn main"}))),
            ) + "\n",
        )
        .unwrap();
        // a2: finished before Giverny looked; only disk knows it.
        std::fs::write(
            agent_transcript(&subs, "a2"),
            [
                serde_json::json!({"type":"user","timestamp": iso(T0 - 50_000)}).to_string(),
                assistant_line(T0 - 1000, None),
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        std::fs::write(
            subs.join("agent-a2.meta.json"),
            r#"{"agentType":"coo","description":"Work coo#1 thing","model":"opus"}"#,
        )
        .unwrap();
        // a3: a transcript but no notification and no live entry — ignored.
        std::fs::write(
            agent_transcript(&subs, "a3"),
            assistant_line(T0, None) + "\n",
        )
        .unwrap();
        append(&parent, &notification_lines("a2", "completed", T0));

        let mut t = Tracker::new(Some(config.clone()));
        t.apply_live(&live("s1", &[("a1", "running", T0)]), T0 + 2000);
        t.refresh();

        let a1 = t.get("a1").unwrap();
        assert_eq!(a1.stage, Stage::Running);
        assert_eq!(a1.activity.as_deref(), Some("Grep: fn main"));
        assert_eq!(a1.transcript, Some(agent_transcript(&subs, "a1")));

        let a2 = t.get("a2").unwrap();
        assert_eq!(a2.stage, Stage::Done);
        assert_eq!(a2.outcome, Some(Outcome::Completed));
        assert_eq!(a2.description.as_deref(), Some("Work coo#1 thing"));
        assert_eq!(a2.agent_type.as_deref(), Some("coo"));
        assert_eq!(a2.started_ms, Some(T0 - 50_000));
        assert_eq!(a2.ended_ms, Some(T0 + 30));
        assert!(t.get("a3").is_none());

        // a1 finishes: the notification lands before the live list catches up.
        append(&parent, &notification_lines("a1", "failed", T0 + 9000));
        t.refresh();
        let a1 = t.get("a1").unwrap();
        assert_eq!(a1.stage, Stage::Done);
        assert_eq!(a1.outcome, Some(Outcome::Failed));
        assert_eq!(a1.activity, None);

        // The live list then drops it: the known outcome is not downgraded.
        t.apply_live(&live("s1", &[]), T0 + 20_000);
        let a1 = t.get("a1").unwrap();
        assert_eq!(a1.outcome, Some(Outcome::Failed));
        assert_eq!(a1.ended_ms, Some(T0 + 9030));

        // Round-trips through serde, and rebuilds its readers after.
        let json = serde_json::to_string(&t).unwrap();
        let mut back: Tracker = serde_json::from_str(&json).unwrap();
        assert_eq!(back.rows(), t.rows());
        back.refresh();
        assert_eq!(back.rows(), t.rows());
    }

    #[test]
    fn a_worker_messaged_after_its_notification_is_running() {
        let (config, subs, parent) = layout("revive", "s1");
        append(&parent, &notification_lines("a1", "completed", T0));
        std::fs::write(
            agent_transcript(&subs, "a1"),
            assistant_line(
                T0 + 120_000,
                Some(("Edit", serde_json::json!({"file_path": "/f"}))),
            ) + "\n",
        )
        .unwrap();
        let mut t = Tracker::new(Some(config));
        t.set_session("s1");
        t.refresh();
        let a1 = t.get("a1").unwrap();
        assert_eq!(a1.stage, Stage::Running);
        assert_eq!(a1.activity.as_deref(), Some("Edit: /f"));
    }

    #[test]
    fn aliases_are_read_too() {
        let (config, subs, parent) = layout("alias", "old");
        std::fs::write(
            agent_transcript(&subs, "a1"),
            assistant_line(T0, None) + "\n",
        )
        .unwrap();
        append(&parent, &notification_lines("a1", "completed", T0 + 10));
        let mut t = Tracker::new(Some(config));
        t.set_session("old");
        t.set_session("new");
        t.refresh();
        assert_eq!(t.get("a1").map(|r| r.stage), Some(Stage::Done));
    }

    /// Against the real transcripts on this machine — read-only, and skipped
    /// unless `GIVERNY_REAL_CLAUDE_DIR` names a config dir. Every session with
    /// subagents is loaded; a worker whose transcript has been quiet for an
    /// hour must read Done, since nothing runs that long silently.
    #[test]
    #[ignore]
    fn real_transcripts() {
        let Ok(dir) = std::env::var("GIVERNY_REAL_CLAUDE_DIR") else {
            return;
        };
        let config = PathBuf::from(dir);
        let now = jiff::Timestamp::now().as_millisecond() as u64;
        let (mut sessions, mut done, mut running, mut stale) = (0, 0, 0, Vec::new());
        // Quiet for an hour and never landed: killed along with its parent,
        // which writes nothing. Reported, not failed — no row is right.
        let mut unlanded = Vec::new();
        let mut outcomes: HashMap<String, usize> = HashMap::new();
        for proj in std::fs::read_dir(config.join("projects"))
            .unwrap()
            .flatten()
        {
            for sess in std::fs::read_dir(proj.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                let sid = sess.file_name().to_string_lossy().into_owned();
                if !sess.path().join("subagents").is_dir() {
                    continue;
                }
                sessions += 1;
                let mut t = Tracker::new(Some(config.clone()));
                t.set_session(&sid);
                t.refresh();
                let subs = sess.path().join("subagents");
                for id in list_agent_ids(&subs) {
                    if t.get(&id).is_none() {
                        let quiet = read_activity(&agent_transcript(&subs, &id))
                            .last_turn_ms
                            .is_none_or(|l| now.saturating_sub(l) > 3_600_000);
                        if quiet {
                            unlanded.push(format!("{sid}/{id}"));
                        }
                    }
                }
                for r in t.rows() {
                    match r.stage {
                        Stage::Done => {
                            done += 1;
                            *outcomes.entry(format!("{:?}", r.outcome)).or_default() += 1;
                            assert!(r.started_ms.is_some(), "{sid}/{}: no start", r.id);
                            assert!(r.transcript.as_ref().is_some_and(|p| p.is_file()));
                        }
                        Stage::Running => {
                            running += 1;
                            if r.last_turn_ms
                                .is_some_and(|l| now.saturating_sub(l) > 3_600_000)
                            {
                                stale.push(format!("{sid}/{}", r.id));
                            }
                        }
                    }
                }
            }
        }
        eprintln!(
            "real: {sessions} sessions, {done} done {outcomes:?}, {running} running, {} stale, {} never landed: {unlanded:?}",
            stale.len(),
            unlanded.len()
        );
        assert!(stale.is_empty(), "quiet >1h yet Running: {stale:?}");
    }
}

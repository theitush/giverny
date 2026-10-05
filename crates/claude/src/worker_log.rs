//! One worker's transcript as a queue of tasks: the tokens
//! each of its turns added, and the messages its dispatcher sent it.
//!
//! A worker handed a second task by `SendMessage` holds one row per task in
//! the agents pane. Each finished task's row shows what *that task* spent —
//! the turns between its hand-off and the next one — never the worker's
//! whole count, which would be counted once per row. That needs every turn
//! with its time, which is what [`WorkerLog`] keeps, following the
//! transcript as it grows.
//!
//! A turn's tokens are what it **added**: its output plus the input read
//! fresh (uncached input and cache creation). Cache reads are the context
//! carried over from earlier turns and are left out, so the sum over a span
//! never counts a turn twice and never goes negative across a compaction.
//! This is a different number from the live count Claude Code's agents view
//! shows (context plus output), which is what a Running row keeps.
//!
//! The dispatcher's messages arrive as `user` lines with
//! `"origin": {"kind": "coordinator"}`. One that says it is a **new task**
//! and names one (`New task for you: acme#614, …`) is a hand-off even when
//! nothing recorded it in the feed; one that names a feed row's key first is
//! when that row's task reached the worker.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// One billed turn: when it was written and what it added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Turn {
    pub at_ms: u64,
    pub added: u64,
}

/// A message the dispatcher sent the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub at_ms: u64,
    /// The first task the message names (`acme#614`, `#70`), if any.
    pub key: Option<String>,
    /// It says it is a new task (`New task for you…`, `Next task: …`).
    pub new_task: bool,
    /// Its first line, cut short: a row's title when nothing else gives one.
    pub title: Option<String>,
}

impl Message {
    /// Is this the hand-off of task `key`: the message names it first.
    pub fn hands_off(&self, key: &str) -> bool {
        self.key
            .as_deref()
            .is_some_and(|k| crate::feed::names_key(k, key))
    }
}

/// What one `usage` object added: output, uncached input and cache creation.
pub fn added_tokens(u: &Value) -> u64 {
    let n = |k: &str| u.get(k).and_then(Value::as_f64).unwrap_or(0.0).max(0.0) as u64;
    n("output_tokens") + n("input_tokens") + n("cache_creation_input_tokens")
}

/// The first task a text names: `repo#12`, `owner/repo#12`, else a bare
/// `#12`. Trailing punctuation is not part of it.
pub fn first_key(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || "()[]{},;:`'\"".contains(c))
        .map(|w| w.trim_end_matches(|c: char| !c.is_ascii_digit()))
        .find_map(|w| {
            let (repo, n) = w.rsplit_once('#')?;
            let digits = !n.is_empty() && n.chars().all(|c| c.is_ascii_digit());
            let repo_ok = repo
                .chars()
                .all(|c| c.is_alphanumeric() || "-_./".contains(c));
            (digits && repo_ok).then(|| {
                // `owner/repo#12` is named by its `repo#12`.
                let repo = repo.rsplit('/').next().unwrap_or(repo);
                format!("{repo}#{n}")
            })
        })
}

/// The text of a message line, without Claude Code's framing.
fn message_text(v: &Value) -> String {
    let content = v.pointer("/message/content");
    let text = match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    // `The coordinator sent a message while you were working:\n<body>`
    match text.split_once("sent a message") {
        Some((head, rest)) if head.len() < 40 => rest
            .split_once(':')
            .map_or(rest, |(_, body)| body)
            .trim()
            .to_string(),
        _ => text.trim().to_string(),
    }
}

/// Longest title taken from a message, in characters.
const TITLE_MAX: usize = 100;

/// A dispatcher's message, from a transcript line.
fn message_of(v: &Value) -> Option<Message> {
    if v.get("type").and_then(Value::as_str) != Some("user")
        || v.pointer("/origin/kind").and_then(Value::as_str) != Some("coordinator")
    {
        return None;
    }
    let at_ms = v.get("timestamp").and_then(Value::as_str).and_then(ms_of)?;
    let body = message_text(v);
    let first_line = body.lines().next().unwrap_or("").to_lowercase();
    let new_task = ["new task", "next task"].iter().any(|p| {
        first_line
            .split_once(p)
            .is_some_and(|(pre, _)| pre.len() < 40)
    });
    let line = body.lines().next().unwrap_or("").trim();
    let title = (!line.is_empty()).then(|| {
        let mut t: String = line.chars().take(TITLE_MAX).collect();
        if line.chars().count() > TITLE_MAX {
            t.push('…');
        }
        t
    });
    Some(Message {
        at_ms,
        key: first_key(&body),
        new_task,
        title,
    })
}

/// Follows one worker's whole transcript: the first [`poll`] reads it from
/// the start, every later one only what was appended.
///
/// [`poll`]: WorkerLog::poll
#[derive(Debug, Clone, Default)]
pub struct WorkerLog {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    turns: Vec<Turn>,
    /// Message id → its turn: Claude Code writes one line per content
    /// block of a reply, each repeating the reply's usage.
    by_id: HashMap<String, usize>,
    messages: Vec<Message>,
}

impl WorkerLog {
    pub fn new(path: impl Into<PathBuf>) -> WorkerLog {
        WorkerLog {
            path: path.into(),
            ..WorkerLog::default()
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// When the worker first wrote: its first turn or the first message it
    /// was sent, whichever came first.
    pub fn first_ms(&self) -> Option<u64> {
        let turn = self.turns.iter().map(|t| t.at_ms).min();
        let msg = self.messages.iter().map(|m| m.at_ms).min();
        match (turn, msg) {
            (Some(t), Some(m)) => Some(t.min(m)),
            (t, m) => t.or(m),
        }
    }

    /// The tokens the turns in `[from_ms, to_ms)` added (`to_ms` `None`:
    /// every turn from `from_ms` on).
    pub fn added(&self, from_ms: u64, to_ms: Option<u64>) -> u64 {
        self.turns
            .iter()
            .filter(|t| t.at_ms >= from_ms && to_ms.is_none_or(|to| t.at_ms < to))
            .map(|t| t.added)
            .sum()
    }

    /// Read what was appended. True when anything was. A file that shrank
    /// (rewritten) is read again from the start.
    pub fn poll(&mut self) -> bool {
        let Ok(mut file) = std::fs::File::open(&self.path) else {
            return false;
        };
        let Ok(len) = file.metadata().map(|m| m.len()) else {
            return false;
        };
        if len < self.offset {
            *self = WorkerLog::new(std::mem::take(&mut self.path));
        }
        if len == self.offset {
            return false;
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return false;
        }
        let mut bytes = Vec::new();
        if file
            .take(len - self.offset)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return false;
        }
        self.offset += bytes.len() as u64;
        let mut chunk = std::mem::take(&mut self.partial);
        chunk.extend_from_slice(&bytes);
        let cut = chunk.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        for line in chunk[..cut].split(|&b| b == b'\n') {
            self.fold(line);
        }
        self.partial = chunk[cut..].to_vec();
        true
    }

    fn fold(&mut self, line: &[u8]) {
        if find(line, b"\"kind\":\"coordinator\"").is_some() {
            if let Some(m) = serde_json::from_slice::<Value>(line)
                .ok()
                .as_ref()
                .and_then(message_of)
            {
                self.messages.push(m);
            }
            return;
        }
        if find(line, b"\"usage\"").is_none() {
            return;
        }
        let Some((at_ms, id, added)) = parsed_turn(line) else {
            return;
        };
        self.push(at_ms, id, added);
    }

    fn push(&mut self, at_ms: u64, id: Option<String>, added: u64) {
        match id.as_ref().and_then(|id| self.by_id.get(id)) {
            // The same reply again: its usage is the same or later.
            Some(&i) => self.turns[i].added = self.turns[i].added.max(added),
            None => {
                if let Some(id) = id {
                    self.by_id.insert(id, self.turns.len());
                }
                self.turns.push(Turn { at_ms, added });
            }
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn ms_of(s: &str) -> Option<u64> {
    s.parse::<jiff::Timestamp>()
        .ok()
        .map(|t| t.as_millisecond().max(0) as u64)
}

/// A billed turn, from an assistant line: when, its message id, and what
/// it added.
fn parsed_turn(line: &[u8]) -> Option<(u64, Option<String>, u64)> {
    let v: Value = serde_json::from_slice(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let msg = v.get("message")?;
    if msg.get("model").and_then(Value::as_str) == Some("<synthetic>") {
        return None;
    }
    let at_ms = ms_of(v.get("timestamp").and_then(Value::as_str)?)?;
    let id = msg.get("id").and_then(Value::as_str).map(str::to_string);
    Some((at_ms, id, added_tokens(msg.get("usage")?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An assistant line: message `id`, at `ts`, with the usage given.
    fn reply(id: &str, ts: &str, input: u64, created: u64, read: u64, out: u64) -> String {
        json!({"type": "assistant", "timestamp": ts,
               "message": {"id": id, "model": "claude-opus", "usage": {
                   "input_tokens": input, "cache_creation_input_tokens": created,
                   "cache_read_input_tokens": read, "output_tokens": out}}})
        .to_string()
    }

    /// A dispatcher's message, as Claude Code writes a `SendMessage`.
    fn sent(ts: &str, body: &str) -> String {
        json!({"type": "user", "timestamp": ts, "isMeta": true,
               "origin": {"kind": "coordinator"},
               "message": {"role": "user", "content":
                   format!("The coordinator sent a message while you were working:\n{body}")}})
        .to_string()
    }

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("giverny-worker-log-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn keys_are_the_first_task_named() {
        assert_eq!(
            first_key("New task for you, Wren: acme#614, the follow-up to #613"),
            Some("acme#614".into())
        );
        assert_eq!(
            first_key("New task for you, #70 (already In Progress)"),
            Some("#70".into())
        );
        assert_eq!(first_key("see owner/demo#87."), Some("demo#87".into()));
        assert_eq!(first_key("no task here, just C# and #x"), None);
    }

    #[test]
    fn a_reply_written_as_several_lines_is_one_turn() {
        let d = scratch("dedupe");
        let p = d.join("agent-a.jsonl");
        let lines = [
            reply("m1", "2026-10-01T10:00:00Z", 3, 1000, 0, 10),
            reply("m1", "2026-10-01T10:00:01Z", 3, 1000, 0, 200),
            reply("m2", "2026-10-01T10:05:00Z", 2, 50, 1000, 100),
            // An API error is not a billed turn.
            json!({"type": "assistant", "timestamp": "2026-10-01T10:06:00Z",
                   "message": {"model": "<synthetic>", "usage": {"output_tokens": 999}}})
            .to_string(),
        ];
        std::fs::write(&p, lines.join("\n") + "\n").unwrap();
        let mut log = WorkerLog::new(&p);
        assert!(log.poll());
        assert_eq!(log.turns().len(), 2);
        assert_eq!(
            log.turns()[0].added,
            1203,
            "output + uncached + created, the last copy"
        );
        assert_eq!(
            log.turns()[1].added,
            152,
            "a cache read is carried context, not added"
        );
        assert_eq!(log.added(0, None), 1355);
        assert!(!log.poll(), "nothing appended");
    }

    #[test]
    fn a_hand_off_is_a_dispatchers_message_naming_a_new_task() {
        let d = scratch("handoff");
        let p = d.join("agent-a.jsonl");
        let lines = [
            reply("m1", "2026-10-01T10:00:00Z", 1, 100, 0, 10),
            sent("2026-10-01T10:01:00Z", "Yes — do the red run (acme#56)."),
            sent(
                "2026-10-01T10:30:00Z",
                "New task for you, Wren: acme#614, the follow-up to #613.",
            ),
            reply("m2", "2026-10-01T10:31:00Z", 1, 100, 0, 10),
        ];
        std::fs::write(&p, lines[..3].join("\n") + "\n" + &lines[3]).unwrap();
        let mut log = WorkerLog::new(&p);
        log.poll();
        let m = log.messages();
        assert_eq!(m.len(), 2);
        assert!(!m[0].new_task && m[0].hands_off("acme#56"));
        assert!(m[1].new_task && m[1].hands_off("acme#614"));
        assert_eq!(log.turns().len(), 1, "the unended last line waits");
        std::fs::write(&p, lines.join("\n") + "\n").unwrap();
        log.poll();
        assert_eq!(log.turns().len(), 2);
        let t = |s: &str| s.parse::<jiff::Timestamp>().unwrap().as_millisecond() as u64;
        assert_eq!(log.added(0, Some(t("2026-10-01T10:30:00Z"))), 111);
        assert_eq!(log.added(t("2026-10-01T10:30:00Z"), None), 111);
    }
}

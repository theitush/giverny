//! The agents-pane feed: what an orchestrator knows that Claude Code does not.
//!
//! Claude Code itself tells Giverny which subagents a session is running and
//! which have finished (see `subagents`). What it cannot say is what those
//! workers are *for* — the task each one holds, the work queued behind them,
//! when each is expected to land and who reviews it. An orchestrator that
//! knows that writes it to one JSON file per Claude session, and the pane
//! merges it with the live rows. The contract is `docs/agents-pane.md`; this
//! module is its reader, and the doc is what a writer codes against.
//!
//! The file is someone else's output and will drift, so it is read the way
//! `jobs.rs` reads Claude Code's state: through `serde_json::Value`, one field
//! at a time. A surprise costs that field, or at worst that row — never the
//! feed.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

/// The feed format this build reads. Bumped only by an *incompatible* change;
/// new fields are added without a bump, and readers ignore what they do not
/// know.
pub const FEED_VERSION: u64 = 1;

/// Where feeds live. Giverny exports this into every tab so a writer running
/// inside one needs no configuration; outside Giverny the writer falls back to
/// [`default_dir`] itself.
pub const DIR_ENV: &str = "GIVERNY_FEED_DIR";

/// `<config>/giverny/feeds` — the same base as Giverny's own state
/// (`~/.config/giverny` on Linux).
pub fn default_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("giverny")
        .join("feeds")
}

/// `$GIVERNY_FEED_DIR` when set and non-empty, else [`default_dir`].
pub fn feed_dir() -> PathBuf {
    std::env::var_os(DIR_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_dir)
}

/// The file a writer for `session` writes: `<dir>/<session>.json`.
pub fn feed_path(dir: &Path, session: &str) -> PathBuf {
    dir.join(format!("{session}.json"))
}

/// Which section of the pane a row sits in, in drawing order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    Running,
    Planned,
    Done,
}

impl Stage {
    /// Case-insensitive, with the obvious synonyms. `None` for anything else:
    /// a row that cannot be placed is dropped rather than guessed at.
    pub fn parse(s: &str) -> Option<Stage> {
        match s.trim().to_ascii_lowercase().as_str() {
            "running" | "live" | "working" | "in progress" | "in_progress" => Some(Stage::Running),
            "planned" | "queued" | "next" => Some(Stage::Planned),
            "done" | "finished" | "completed" | "landed" => Some(Stage::Done),
            _ => None,
        }
    }
}

/// One row as the feed wrote it. Every field but `key` and `stage` is
/// optional; see `docs/agents-pane.md` for what each one drives.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FeedRow {
    /// What the row is — a task id such as `coo#158`. Falls back to
    /// `agent_id` when the writer gave none.
    pub key: String,
    pub stage: Option<Stage>,
    pub title: Option<String>,
    /// The Claude Code subagent id holding this row — the join key with the
    /// live rows. Several rows may share one.
    pub agent_id: Option<String>,
    pub started_ms: Option<u64>,
    pub ended_ms: Option<u64>,
    /// The true spawn, when `started` was moved on by paused spans (coo#170).
    pub spawned_ms: Option<u64>,
    /// Seconds the row has spent paused, an open pause counted up to when
    /// the feed was written; `started` is already moved on by them.
    pub paused_s: Option<u64>,
    /// When the row's still-open pause began: its clock is stopped.
    pub paused_since_ms: Option<u64>,
    /// Estimated total duration, in seconds, from `started`.
    pub eta_s: Option<u64>,
    /// How late (`+`) or early (`-`) a Done row landed against `eta_s`, when
    /// the writer knows better than `(ended − started) − eta_s`.
    pub eta_delta_s: Option<i64>,
    pub landing: Option<String>,
    pub tokens: Option<u64>,
    pub group: Option<String>,
    pub brief: Option<PathBuf>,
    pub open: Option<String>,
    pub note: Option<String>,
}

impl FeedRow {
    /// The section this row is drawn in. Rows are only ever built with one;
    /// the `Option` is for `Default`.
    pub fn stage(&self) -> Stage {
        self.stage.unwrap_or(Stage::Planned)
    }
}

/// A parsed feed file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Feed {
    pub version: u64,
    /// The Claude session id the feed is for.
    pub session: Option<String>,
    /// Earlier ids of the same conversation (a resume or a re-id), so a tab
    /// still holding an old id finds the feed.
    pub aliases: Vec<String>,
    pub rows: Vec<FeedRow>,
    /// One line drawn under the table, verbatim.
    pub footer: Option<String>,
    /// When the file was last written (its mtime), for the reader's own
    /// arithmetic: a paused row's `started` is only true as of this instant.
    /// `None` for a feed parsed from bytes.
    pub written_ms: Option<u64>,
}

impl Feed {
    /// Is this feed about `session` — by its own id or by an alias?
    pub fn names(&self, session: &str) -> bool {
        self.session.as_deref() == Some(session) || self.aliases.iter().any(|a| a == session)
    }
}

/// Why a file is not a feed at all. Anything smaller costs a field or a row.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FeedError {
    #[error("feed is not JSON: {0}")]
    NotJson(String),
    #[error("feed is not a JSON object")]
    NotObject,
    #[error("feed version {0} is newer than this Giverny reads ({FEED_VERSION})")]
    Unsupported(u64),
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => Some(s.trim()).filter(|s| !s.is_empty()).map(Into::into),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A non-negative whole number, from a number (fractions truncated) or a
/// numeric string.
fn u64_field(v: &Value, key: &str) -> Option<u64> {
    match v.get(key)? {
        Value::Number(n) => n.as_u64().or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite() && *f >= 0.0)
                .map(|f| f as u64)
        }),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite() && *f >= 0.0)
            .map(|f| f as u64),
        _ => None,
    }
}

/// A signed whole number, from a number or a numeric string.
fn i64_field(v: &Value, key: &str) -> Option<i64> {
    match v.get(key)? {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f as i64)),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite())
            .map(|f| f as i64),
        _ => None,
    }
}

/// Milliseconds since the epoch, from an RFC 3339 string or a number of
/// epoch milliseconds.
fn millis_field(v: &Value, key: &str) -> Option<u64> {
    match v.get(key)? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s
            .trim()
            .parse::<jiff::Timestamp>()
            .ok()
            .map(|t| t.as_millisecond().max(0) as u64),
        _ => None,
    }
}

fn parse_row(v: &Value) -> Option<FeedRow> {
    if !v.is_object() {
        return None;
    }
    let stage = Stage::parse(v.get("stage")?.as_str()?)?;
    let agent_id = str_field(v, "agent_id");
    let key = str_field(v, "key").or_else(|| agent_id.clone())?;
    Some(FeedRow {
        key,
        stage: Some(stage),
        title: str_field(v, "title"),
        agent_id,
        started_ms: millis_field(v, "started"),
        ended_ms: millis_field(v, "ended"),
        spawned_ms: millis_field(v, "spawned"),
        paused_s: u64_field(v, "paused_s"),
        paused_since_ms: millis_field(v, "paused_since"),
        eta_s: u64_field(v, "eta_s"),
        eta_delta_s: i64_field(v, "eta_delta_s"),
        landing: str_field(v, "landing"),
        tokens: u64_field(v, "tokens"),
        group: str_field(v, "group"),
        brief: str_field(v, "brief").map(PathBuf::from),
        open: str_field(v, "open"),
        note: str_field(v, "note"),
    })
}

/// Parse a feed from its bytes.
///
/// Only three things refuse the whole file: it is not JSON, it is not an
/// object, or it declares a version newer than [`FEED_VERSION`]. A row with
/// no usable `stage`, or neither `key` nor `agent_id`, is dropped; a field of
/// the wrong type reads as absent.
pub fn parse(bytes: &[u8]) -> Result<Feed, FeedError> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| FeedError::NotJson(e.to_string()))?;
    if !v.is_object() {
        return Err(FeedError::NotObject);
    }
    let version = u64_field(&v, "version").unwrap_or(1);
    if version > FEED_VERSION {
        return Err(FeedError::Unsupported(version));
    }
    let aliases = v
        .get("aliases")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(Into::into)
                .collect()
        })
        .unwrap_or_default();
    let rows = v
        .get("rows")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(parse_row).collect())
        .unwrap_or_default();
    let footer = v.get("footer").and_then(|f| match f {
        Value::String(_) => str_field(&v, "footer"),
        Value::Object(_) => str_field(f, "text"),
        _ => None,
    });
    Ok(Feed {
        version,
        session: str_field(&v, "session"),
        aliases,
        rows,
        footer,
        written_ms: None,
    })
}

/// Read and parse one file. `None` when it is missing, unreadable or not a
/// feed — the pane then simply has no feed.
pub fn read(path: &Path) -> Option<Feed> {
    let bytes = std::fs::read(path).ok()?;
    let written_ms = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64);
    match parse(&bytes) {
        Ok(f) => Some(Feed { written_ms, ..f }),
        Err(e) => {
            tracing::debug!("agents feed {}: {e}", path.display());
            None
        }
    }
}

/// Find the feed for `session` in `dir`: `<session>.json` if it exists, else
/// the most recently modified `*.json` there whose `session` or `aliases`
/// names it (a writer that kept its old file name after a re-id).
pub fn find(dir: &Path, session: &str) -> Option<(PathBuf, Feed)> {
    let direct = feed_path(dir, session);
    if let Some(f) = read(&direct) {
        return Some((direct, f));
    }
    let mut best: Option<(SystemTime, PathBuf, Feed)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue; // `.tmp` files mid-write, and anything else
        }
        let Some(feed) = read(&path) else { continue };
        if !feed.names(session) {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _, _)| mtime > *t) {
            best = Some((mtime, path, feed));
        }
    }
    best.map(|(_, p, f)| (p, f))
}

/// Re-reads a session's feed only when its file changed. Call [`poll`] once
/// a second; it costs one `stat` when nothing moved.
///
/// [`poll`]: FeedCache::poll
#[derive(Debug, Default)]
pub struct FeedCache {
    session: String,
    path: Option<PathBuf>,
    stamp: Option<(SystemTime, u64)>,
    feed: Option<Feed>,
}

impl FeedCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current feed for `session` in `dir`, re-read if the file's mtime
    /// or length changed, searched for afresh if it vanished or the session
    /// changed.
    pub fn poll(&mut self, dir: &Path, session: &str) -> Option<&Feed> {
        if self.session != session {
            *self = FeedCache {
                session: session.into(),
                ..Default::default()
            };
        }
        let stamp_of = |p: &Path| {
            std::fs::metadata(p)
                .ok()
                .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
        };
        let fresh = self
            .path
            .as_deref()
            .and_then(|p| stamp_of(p).map(|s| (p.to_path_buf(), s)));
        match fresh {
            Some((_, s)) if Some(s) == self.stamp => {}
            Some((p, s)) => {
                // Changed in place: re-read. A half-written file (a writer
                // that ignored the rename rule) keeps the last good feed.
                if let Some(f) = read(&p) {
                    self.feed = Some(f);
                }
                self.stamp = Some(s);
            }
            None => {
                self.path = None;
                self.stamp = None;
                self.feed = None;
                if let Some((p, f)) = find(dir, session) {
                    self.stamp = stamp_of(&p);
                    self.path = Some(p);
                    self.feed = Some(f);
                }
            }
        }
        self.feed.as_ref()
    }
}

// ---------------------------------------------------------------- merge ----

/// What the pane needs from a live row (Claude Code's own view of a
/// subagent) to merge it with the feed. The live-row model implements this;
/// the join key is [`agent_id`], the subagent's id as Claude Code reports it
/// (`tasks[].id` on the `subagentStatusLine` payload, the `<id>` of
/// `subagents/agent-<id>.jsonl`).
///
/// [`agent_id`]: LiveAgent::agent_id
pub trait LiveAgent {
    fn agent_id(&self) -> &str;
    /// Still running (else it has finished).
    fn running(&self) -> bool;
    fn started_ms(&self) -> Option<u64>;
    fn tokens(&self) -> Option<u64>;
    /// What the spawn said the worker is for (the Agent tool's
    /// `description`, e.g. `Work giverny#82 …`): the second join key, for a
    /// feed row that carries no `agent_id` (giverny#83).
    fn description(&self) -> Option<&str> {
        None
    }
}

/// Does `text` name `key` as a whole word? `giverny#82` is named by
/// `Work giverny#82 pane` and by `theitush/giverny#82`, never by
/// `giverny#820` nor `xgiverny#82`.
pub fn names_key(text: &str, key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    text.match_indices(key).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + key.len()..].chars().next();
        before.is_none_or(|c| !word(c) && c != '#') && after.is_none_or(|c| !word(c))
    })
}

/// One row of the pane: a feed row, a live row, or both joined.
#[derive(Debug)]
pub struct PaneRow<'a, L> {
    pub stage: Stage,
    pub feed: Option<&'a FeedRow>,
    pub live: Option<&'a L>,
    /// Same worker as the row directly above: draw its per-worker cells
    /// (agent, elapsed, tokens, activity) as `"`.
    pub ditto: bool,
}

impl<L: LiveAgent> PaneRow<'_, L> {
    pub fn agent_id(&self) -> Option<&str> {
        self.feed
            .and_then(|f| f.agent_id.as_deref())
            .or_else(|| self.live.map(|l| l.agent_id()))
    }

    /// Start of the row's clock: the feed's `started` where it gives one,
    /// else the live row's (Claude Code saw the worker start). The feed's
    /// wins because it is per row — a worker holding several rows started
    /// each at a different time — and because it is moved on by the row's
    /// paused spans, which Claude Code knows nothing of (giverny#12).
    pub fn started_ms(&self) -> Option<u64> {
        self.row_started_ms()
    }

    /// When this row's clock stopped, if it is paused right now: the feed's
    /// `paused_since`, on a Running row only.
    pub fn paused_since_ms(&self) -> Option<u64> {
        if self.stage != Stage::Running {
            return None;
        }
        self.feed.and_then(|f| f.paused_since_ms)
    }

    fn row_started_ms(&self) -> Option<u64> {
        self.feed
            .and_then(|f| f.started_ms)
            .or_else(|| self.live.and_then(|l| l.started_ms()))
    }

    /// Tokens: the live count, else what the feed wrote.
    pub fn tokens(&self) -> Option<u64> {
        self.live
            .and_then(|l| l.tokens())
            .or_else(|| self.feed.and_then(|f| f.tokens))
    }

    /// A Done row's ETA cell, in seconds: `eta_delta_s` when the feed sent
    /// it, else `(ended − started) − eta_s`. `None` on any other stage, or
    /// when there is no estimate to compare with.
    pub fn eta_delta_s(&self) -> Option<i64> {
        if self.stage != Stage::Done {
            return None;
        }
        let f = self.feed?;
        if let Some(d) = f.eta_delta_s {
            return Some(d);
        }
        let eta = f.eta_s? as i64;
        let took_ms = f.ended_ms? as i64 - self.row_started_ms()? as i64;
        Some(took_ms.div_euclid(1000) - eta)
    }
}

/// Merge the feed with the live rows.
///
/// - Every feed row is a row, in feed order, in the stage the feed gave it.
///   It carries the live row its `agent_id` names; failing that (no
///   `agent_id`, or one Claude Code no longer lists), the live row whose
///   description names the row's key — `Work giverny#82 …` holds
///   `giverny#82`, and one naming several keys holds each of them
///   (giverny#83). The feed's stage wins: one worker may hold a Done row and
///   a Running one at once.
/// - A live row that no feed row carries, and whose description names no
///   feed key, is a row of its own, Running or Done by its own state, after
///   the feed's rows of that stage. One that names a key is that row's
///   worker and is never drawn twice.
/// - Rows are grouped Running, Planned, Done; order within a stage is kept.
/// - A row whose worker is the one directly above it is a ditto.
pub fn merge<'a, L: LiveAgent>(feed: Option<&'a Feed>, live: &'a [L]) -> Vec<PaneRow<'a, L>> {
    let feed_rows: &[FeedRow] = feed.map(|f| f.rows.as_slice()).unwrap_or(&[]);
    let find_live = |id: &str| live.iter().find(|l| l.agent_id() == id);
    let names = |l: &L, key: &str| l.description().is_some_and(|d| names_key(d, key));
    // A Running row wants the worker still running; any other the one that
    // finished. Either takes the other when that is all there is.
    let by_description = |f: &FeedRow| -> Option<&'a L> {
        let want_running = f.stage() == Stage::Running;
        let mut named = live.iter().filter(|l| names(l, &f.key));
        let first = named.clone().next()?;
        Some(named.find(|l| l.running() == want_running).unwrap_or(first))
    };
    let mut out: Vec<PaneRow<'a, L>> = feed_rows
        .iter()
        .map(|f| PaneRow {
            stage: f.stage(),
            feed: Some(f),
            live: f
                .agent_id
                .as_deref()
                .and_then(find_live)
                .or_else(|| by_description(f)),
            ditto: false,
        })
        .collect();
    for l in live {
        let carried = out
            .iter()
            .any(|r| r.live.is_some_and(|c| c.agent_id() == l.agent_id()));
        let named = carried
            || feed_rows
                .iter()
                .any(|f| f.agent_id.as_deref() == Some(l.agent_id()) || names(l, &f.key));
        if !named {
            out.push(PaneRow {
                stage: if l.running() {
                    Stage::Running
                } else {
                    Stage::Done
                },
                feed: None,
                live: Some(l),
                ditto: false,
            });
        }
    }
    out.sort_by_key(|r| r.stage); // stable: feed order, then live order
    for i in 1..out.len() {
        let same = match (out[i - 1].agent_id(), out[i].agent_id()) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        out[i].ditto = same;
    }
    out
}

// ----------------------------------------------------------- formatting ----

/// A span as the pane's clock columns write it: `5m`, `1h3m`, `1d3h12m`,
/// whole minutes (rounded), zero units left out, `0m` for nothing. A
/// negative span keeps a leading `-`. The caller adds `~` for an estimate.
pub fn fmt_span(secs: i64) -> String {
    let mins = (secs.abs() + 30) / 60;
    let (d, rest) = (mins / 1440, mins % 1440);
    let (h, m) = (rest / 60, rest % 60);
    let body: String = [(d, "d"), (h, "h"), (m, "m")]
        .iter()
        .filter(|(n, _)| *n != 0)
        .map(|(n, u)| format!("{n}{u}"))
        .collect();
    let body = if body.is_empty() { "0m".into() } else { body };
    if secs < 0 && mins != 0 {
        format!("-{body}")
    } else {
        body
    }
}

/// A Done row's ETA cell: `(+5m)` late, `(-1h20m)` early, `(±0m)` on the
/// minute.
pub fn fmt_delta(secs: i64) -> String {
    let s = fmt_span(secs);
    if s == "0m" {
        "(±0m)".into()
    } else if secs > 0 {
        format!("(+{s})")
    } else {
        format!("({s})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{"version":1,"session":"s-new","aliases":["s-old"],
      "rows":[
        {"key":"coo#158","stage":"running","title":"FEATURE: pane","agent_id":"a1",
         "started":"2026-09-23T10:00:00Z","eta_s":2400,"landing":"Review — ita",
         "tokens":123456,"group":"lane-1","brief":"/tmp/b.md","open":"claude --resume x","note":"n"},
        {"key":"coo#159","stage":"done","agent_id":"a1",
         "started":"2026-09-23T09:00:00Z","ended":"2026-09-23T09:45:00Z","eta_s":2400},
        {"key":"coo#160","stage":"planned","title":"later","eta_s":600}
      ],
      "footer":{"text":"session 3 · total: 1.2M"}}"#;

    struct Live {
        id: &'static str,
        running: bool,
        started: Option<u64>,
        tokens: Option<u64>,
        desc: Option<&'static str>,
    }

    impl LiveAgent for Live {
        fn agent_id(&self) -> &str {
            self.id
        }
        fn running(&self) -> bool {
            self.running
        }
        fn started_ms(&self) -> Option<u64> {
            self.started
        }
        fn tokens(&self) -> Option<u64> {
            self.tokens
        }
        fn description(&self) -> Option<&str> {
            self.desc
        }
    }

    fn live(id: &'static str, running: bool) -> Live {
        Live {
            id,
            running,
            started: Some(1_000),
            tokens: Some(7),
            desc: None,
        }
    }

    fn described(id: &'static str, running: bool, desc: &'static str) -> Live {
        Live {
            desc: Some(desc),
            ..live(id, running)
        }
    }

    fn shape<'a>(
        rows: &[PaneRow<'a, Live>],
    ) -> Vec<(Stage, Option<&'a str>, Option<&'static str>, bool)> {
        rows.iter()
            .map(|r| {
                (
                    r.stage,
                    r.feed.map(|f| f.key.as_str()),
                    r.live.map(|l| l.id),
                    r.ditto,
                )
            })
            .collect()
    }

    #[test]
    fn keys_are_named_as_whole_words() {
        assert!(names_key("Work giverny#82 pane link", "giverny#82"));
        assert!(names_key("giverny#82", "giverny#82"));
        assert!(names_key("Work theitush/giverny#82: x", "giverny#82"));
        assert!(names_key("Work giverny#82+giverny#84", "giverny#84"));
        assert!(!names_key("Work giverny#820", "giverny#82"));
        assert!(!names_key("Work xgiverny#82", "giverny#82"));
        assert!(!names_key("Work coo#giverny#82", "giverny#82"));
        assert!(!names_key("anything", ""));
    }

    /// giverny#83: the feed was written at `start`, before the worker's id
    /// was known, and nothing refreshed it. The worker Claude Code lists
    /// names the row's key in its description: one row, carrying both.
    #[test]
    fn a_stale_row_is_joined_to_the_worker_its_description_names() {
        let mut stale = row("giverny#82", Stage::Running, None);
        stale.started_ms = Some(500);
        stale.eta_s = Some(3600);
        let f = Feed {
            rows: vec![stale, row("giverny#84", Stage::Planned, None)],
            ..Default::default()
        };
        let lives = [
            described("w82", true, "Work giverny#82 open direct"),
            described("other", true, "Explore the relay"),
        ];
        let rows = merge(Some(&f), &lives);
        assert_eq!(
            shape(&rows),
            [
                (Stage::Running, Some("giverny#82"), Some("w82"), false),
                (Stage::Running, None, Some("other"), false),
                (Stage::Planned, Some("giverny#84"), None, false),
            ]
        );
        assert_eq!(rows[0].agent_id(), Some("w82"));
        assert_eq!(rows[0].tokens(), Some(7), "the worker's tokens");
        assert_eq!(rows[0].started_ms(), Some(500), "the feed's clock");
    }

    #[test]
    fn one_worker_naming_several_keys_holds_each_row() {
        let f = Feed {
            rows: vec![
                row("g#1", Stage::Running, None),
                row("g#2", Stage::Running, None),
            ],
            ..Default::default()
        };
        let lives = [described("w", true, "Work g#1 and g#2 together")];
        let rows = merge(Some(&f), &lives);
        assert_eq!(
            shape(&rows),
            [
                (Stage::Running, Some("g#1"), Some("w"), false),
                (Stage::Running, Some("g#2"), Some("w"), true),
            ]
        );
    }

    #[test]
    fn a_running_row_takes_the_running_worker_and_a_gone_id_falls_back() {
        // An earlier worker on the same task finished; the feed still names
        // it, and Claude Code no longer lists that id at all.
        let f = Feed {
            rows: vec![row("g#5", Stage::Running, Some("vanished"))],
            ..Default::default()
        };
        let lives = [
            described("old", false, "Work g#5 first try"),
            described("new", true, "Work g#5 again"),
        ];
        let rows = merge(Some(&f), &lives);
        assert_eq!(
            shape(&rows),
            [(Stage::Running, Some("g#5"), Some("new"), false)],
            "neither worker naming the key is drawn on its own"
        );
        // A Done row prefers the finished one.
        let f = Feed {
            rows: vec![row("g#5", Stage::Done, None)],
            ..Default::default()
        };
        let rows = merge(Some(&f), &lives);
        assert_eq!(
            shape(&rows),
            [(Stage::Done, Some("g#5"), Some("old"), false)]
        );
        // An agent_id that is listed still wins over any description.
        let f = Feed {
            rows: vec![row("g#5", Stage::Running, Some("old"))],
            ..Default::default()
        };
        assert_eq!(merge(Some(&f), &lives)[0].live.map(|l| l.id), Some("old"));
    }

    fn row(key: &str, stage: Stage, agent: Option<&str>) -> FeedRow {
        FeedRow {
            key: key.into(),
            stage: Some(stage),
            agent_id: agent.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn parses_every_field() {
        let f = parse(FULL.as_bytes()).unwrap();
        assert_eq!(f.version, 1);
        assert_eq!(f.session.as_deref(), Some("s-new"));
        assert_eq!(f.aliases, vec!["s-old".to_string()]);
        assert_eq!(f.footer.as_deref(), Some("session 3 · total: 1.2M"));
        assert_eq!(f.rows.len(), 3);
        let r = &f.rows[0];
        assert_eq!(r.key, "coo#158");
        assert_eq!(r.stage(), Stage::Running);
        assert_eq!(r.title.as_deref(), Some("FEATURE: pane"));
        assert_eq!(r.agent_id.as_deref(), Some("a1"));
        assert_eq!(r.started_ms, Some(1_790_157_600_000));
        assert_eq!(r.eta_s, Some(2400));
        assert_eq!(r.landing.as_deref(), Some("Review — ita"));
        assert_eq!(r.tokens, Some(123_456));
        assert_eq!(r.group.as_deref(), Some("lane-1"));
        assert_eq!(r.brief.as_deref(), Some(Path::new("/tmp/b.md")));
        assert_eq!(r.open.as_deref(), Some("claude --resume x"));
        assert_eq!(r.note.as_deref(), Some("n"));
        assert_eq!(f.rows[1].ended_ms, Some(1_790_156_700_000));
        assert!(f.names("s-new") && f.names("s-old") && !f.names("s-x"));
    }

    #[test]
    fn a_surprise_costs_a_field_or_a_row_never_the_feed() {
        let f = parse(
            br#"{"rows":[
              {"key":"k1","stage":"RUNNING","title":42,"eta_s":"90","tokens":-5,
               "started":"not a date","eta_delta_s":"-120","unknown":{"x":1}},
              {"key":"k2","stage":"sideways"},
              {"stage":"done"},
              {"stage":"done","agent_id":"a9"},
              "not an object",
              {"key":"k3"}
            ],"footer":"plain string","extra":[1,2]}"#,
        )
        .unwrap();
        assert_eq!(f.version, 1, "an absent version is 1");
        let keys: Vec<_> = f.rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            ["k1", "a9"],
            "unplaceable and unnamed rows are dropped"
        );
        let r = &f.rows[0];
        assert_eq!(r.stage(), Stage::Running);
        assert_eq!(r.title.as_deref(), Some("42"));
        assert_eq!(r.eta_s, Some(90));
        assert_eq!(r.tokens, None);
        assert_eq!(r.started_ms, None);
        assert_eq!(r.eta_delta_s, Some(-120));
        assert_eq!(f.footer.as_deref(), Some("plain string"));
    }

    #[test]
    fn epoch_millis_are_timestamps_too() {
        let f = parse(br#"{"rows":[{"key":"k","stage":"done","started":1000,"ended":61000}]}"#)
            .unwrap();
        assert_eq!(f.rows[0].started_ms, Some(1000));
        assert_eq!(f.rows[0].ended_ms, Some(61000));
    }

    #[test]
    fn only_three_things_refuse_the_file() {
        assert!(matches!(parse(b"{nope"), Err(FeedError::NotJson(_))));
        assert_eq!(parse(b"[]"), Err(FeedError::NotObject));
        assert_eq!(
            parse(br#"{"version":2,"rows":[]}"#),
            Err(FeedError::Unsupported(2))
        );
        assert!(parse(b"{}").unwrap().rows.is_empty());
    }

    #[test]
    fn merge_joins_on_agent_id_and_groups_by_stage() {
        let f = parse(FULL.as_bytes()).unwrap();
        let lives = [live("a1", true), live("zz", true), live("gone", false)];
        let rows = merge(Some(&f), &lives);
        let shape: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.stage,
                    r.feed.map(|f| f.key.as_str()),
                    r.live.map(|l| l.id),
                    r.ditto,
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                (Stage::Running, Some("coo#158"), Some("a1"), false),
                (Stage::Running, None, Some("zz"), false),
                (Stage::Planned, Some("coo#160"), None, false),
                (Stage::Done, Some("coo#159"), Some("a1"), false),
                (Stage::Done, None, Some("gone"), false),
            ]
        );
        // The feed's `started` is the row's clock (giverny#12); a live-only
        // row runs on the worker's own. Tokens: native first.
        assert_eq!(rows[0].started_ms(), rows[0].feed.unwrap().started_ms);
        assert_ne!(rows[0].started_ms(), Some(1_000));
        assert_eq!(rows[1].started_ms(), Some(1_000));
        assert_eq!(rows[0].tokens(), Some(7));
        assert_eq!(rows[2].tokens(), None);
    }

    #[test]
    fn a_worker_holding_adjacent_rows_is_dittoed() {
        let f = Feed {
            rows: vec![
                row("t1", Stage::Running, Some("a")),
                row("t2", Stage::Running, Some("a")),
                row("t3", Stage::Running, Some("b")),
                row("t4", Stage::Running, None),
                row("t5", Stage::Running, None),
            ],
            ..Default::default()
        };
        let rows = merge::<Live>(Some(&f), &[]);
        let d: Vec<_> = rows.iter().map(|r| r.ditto).collect();
        assert_eq!(d, [false, true, false, false, false]);
    }

    #[test]
    fn no_feed_is_just_the_live_rows() {
        let lives = [live("x", false), live("y", true)];
        let rows = merge(None, &lives);
        let ids: Vec<_> = rows.iter().map(|r| (r.stage, r.agent_id())).collect();
        assert_eq!(ids, [(Stage::Running, Some("y")), (Stage::Done, Some("x"))]);
    }

    #[test]
    fn done_row_eta_delta() {
        let f = parse(FULL.as_bytes()).unwrap();
        let rows = merge::<Live>(Some(&f), &[]);
        // 45 min taken against a 40 min estimate: 5 min late.
        let done = rows.iter().find(|r| r.stage == Stage::Done).unwrap();
        assert_eq!(done.eta_delta_s(), Some(300));
        assert_eq!(fmt_delta(done.eta_delta_s().unwrap()), "(+5m)");
        // Only Done rows carry one.
        assert!(
            rows.iter()
                .filter(|r| r.stage != Stage::Done)
                .all(|r| r.eta_delta_s().is_none())
        );

        let mut r = row("k", Stage::Done, None);
        r.eta_s = Some(7200);
        r.started_ms = Some(0);
        r.ended_ms = Some(2_400_000);
        let f = Feed {
            rows: vec![r.clone()],
            ..Default::default()
        };
        assert_eq!(merge::<Live>(Some(&f), &[])[0].eta_delta_s(), Some(-4800));
        // The writer's own delta wins.
        r.eta_delta_s = Some(60);
        let f = Feed {
            rows: vec![r.clone()],
            ..Default::default()
        };
        assert_eq!(merge::<Live>(Some(&f), &[])[0].eta_delta_s(), Some(60));
        // No estimate, no cell.
        r.eta_delta_s = None;
        r.eta_s = None;
        let f = Feed {
            rows: vec![r],
            ..Default::default()
        };
        assert_eq!(merge::<Live>(Some(&f), &[])[0].eta_delta_s(), None);
    }

    #[test]
    fn a_paused_row_carries_its_pause() {
        // What coo/orchestrate-status writes for a row paused at 10:30 and
        // still paused when the feed was written (coo#170).
        let f = parse(
            br#"{"rows":[
              {"key":"g#1","stage":"running","started":"2026-09-23T10:20:00Z",
               "spawned":"2026-09-23T10:00:00Z","paused_s":1200,
               "paused_since":"2026-09-23T10:30:00Z"},
              {"key":"g#2","stage":"planned","paused_since":"2026-09-23T10:30:00Z"}
            ]}"#,
        )
        .unwrap();
        let r = &f.rows[0];
        let ms = |s: &str| s.parse::<jiff::Timestamp>().unwrap().as_millisecond() as u64;
        assert_eq!(r.spawned_ms, Some(ms("2026-09-23T10:00:00Z")));
        assert_eq!(r.paused_s, Some(1200));
        assert_eq!(r.paused_since_ms, Some(ms("2026-09-23T10:30:00Z")));
        let rows = merge::<Live>(Some(&f), &[]);
        assert_eq!(rows[0].paused_since_ms(), r.paused_since_ms);
        // Only a Running row's clock can be stopped.
        assert_eq!(rows[1].paused_since_ms(), None);
        // A feed from bytes has no write time.
        assert_eq!(f.written_ms, None);
    }

    #[test]
    fn a_feed_read_from_disk_knows_when_it_was_written() {
        let dir = scratch("written");
        let p = feed_path(&dir, "s");
        std::fs::write(&p, br#"{"session":"s","rows":[]}"#).unwrap();
        let mtime = std::fs::metadata(&p)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert_eq!(read(&p).unwrap().written_ms, Some(mtime));
    }

    #[test]
    fn spans_and_deltas_read_like_the_orchestrator_writes_them() {
        assert_eq!(fmt_span(0), "0m");
        assert_eq!(fmt_span(29), "0m");
        assert_eq!(fmt_span(300), "5m");
        assert_eq!(fmt_span(3780), "1h3m");
        assert_eq!(fmt_span(3600), "1h");
        assert_eq!(fmt_span(86400 + 720), "1d12m");
        assert_eq!(fmt_span(97920), "1d3h12m");
        assert_eq!(fmt_span(-720), "-12m");
        assert_eq!(fmt_delta(300), "(+5m)");
        assert_eq!(fmt_delta(-4800), "(-1h20m)");
        assert_eq!(fmt_delta(10), "(±0m)");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("giverny-feed-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn find_by_name_then_by_alias() {
        let dir = scratch("find");
        std::fs::write(
            dir.join("s-old.json"),
            r#"{"session":"s-new","aliases":["s-old","s-older"],"rows":[]}"#,
        )
        .unwrap();
        std::fs::write(dir.join("junk.json"), "not json").unwrap();
        std::fs::write(dir.join("s-older.json.tmp"), "{}").unwrap();
        assert!(find(&dir, "s-old").is_some(), "direct file name");
        let (p, f) = find(&dir, "s-new").expect("found by its own session field");
        assert_eq!(p, dir.join("s-old.json"));
        assert_eq!(f.session.as_deref(), Some("s-new"));
        assert!(
            find(&dir, "s-older").is_some(),
            "found by alias, tmp ignored"
        );
        assert!(find(&dir, "nobody").is_none());
        assert!(find(&dir.join("missing"), "s-new").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_rereads_only_on_change_and_survives_a_bad_write() {
        let dir = scratch("cache");
        let path = feed_path(&dir, "s");
        let mut cache = FeedCache::new();
        assert!(cache.poll(&dir, "s").is_none());

        std::fs::write(&path, r#"{"rows":[{"key":"a","stage":"planned"}]}"#).unwrap();
        assert_eq!(cache.poll(&dir, "s").unwrap().rows[0].key, "a");

        // A different length is a change even within one mtime tick.
        std::fs::write(&path, r#"{"rows":[{"key":"bb","stage":"planned"}]}"#).unwrap();
        assert_eq!(cache.poll(&dir, "s").unwrap().rows[0].key, "bb");

        // A torn write keeps the last good feed.
        std::fs::write(&path, r#"{"rows":[{"key":"#).unwrap();
        assert_eq!(cache.poll(&dir, "s").unwrap().rows[0].key, "bb");

        // Gone is gone.
        std::fs::remove_file(&path).unwrap();
        assert!(cache.poll(&dir, "s").is_none());

        // A new session starts clean.
        std::fs::write(
            feed_path(&dir, "t"),
            r#"{"rows":[{"key":"t","stage":"done"}]}"#,
        )
        .unwrap();
        assert_eq!(cache.poll(&dir, "t").unwrap().rows[0].key, "t");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_dir_and_file_name() {
        // Only asserts the default's shape; the env var is process-global and
        // other tests run in parallel, so it is not set here.
        assert!(default_dir().ends_with("giverny/feeds"));
        assert_eq!(
            feed_path(Path::new("/d"), "abc"),
            PathBuf::from("/d/abc.json")
        );
    }
}

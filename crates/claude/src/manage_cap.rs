//! The working cap on a worker's context (giverny#536): a manager session's
//! Running worker is kept under `[management_panel.workers]
//! context_cap_tokens` of Giverny's `config.toml` (450k by default), its
//! context counted as the pane's TOKENS column counts it
//! ([`crate::tokens::tokens_of`] on its transcript: the context of its last
//! API response).
//!
//! **Giverny suggests, managers decide.** Nothing here stops, pauses or
//! messages a worker. On the dispatcher's own tool call ([`deliver`], from
//! the plugin's hook), a Running row whose worker has reached
//! [`NEAR_PERCENT`] of the cap is told to its manager, with what to do:
//! SendMessage the worker to come to a safe point, commit and push, write
//! its next step where the project keeps it and end its turn with a report;
//! then give the rest of the task to a fresh worker, its spawn description
//! naming the task (the revive path: `giverny manage` moves the row to it
//! once the old one has finished), with a brief saying where it stopped. A
//! worker that passes the cap anyway is told once more.
//!
//! **Once per worker per crossing.** The row keeps what was told and of
//! which worker: `cap_told`, `{"agent": <id>, "at": "near" | "over"}`. A
//! worker found back under the point (a compaction) has it cleared, so a
//! new crossing is told again; a row now held by another worker starts
//! over. A paused row is passed by, its stamp kept: its worker is stopped,
//! and the first look after its resume tells it.
//!
//! **Cheap.** A session with no feed of its own and no manager mark costs
//! two `stat`s; a manager's, one more between looks, at most one look every
//! [`LOOK_EVERY_MS`], timed by a marker under `cap/` beside the feeds. A
//! look reads the feed and, for each Running worker, the tail of its
//! transcript; the feed is written only when a stamp changes. The limits
//! are read from `config.toml` at each look, so a change counts at once.

use std::collections::HashMap;
use std::path::Path;

use giverny_core::config::WorkerContext;
use serde_json::{Map, Value, json};

use crate::manage::{self, Lock};
use crate::{continuation, feed};

/// Where the manager is told: this share of the cap, in percent (382.5k of
/// the default 450k). The notice reaches the manager on its next own tool
/// call, and the worker wraps up after that, so the point leaves room for
/// both. Measured on this machine's 1,310 worker transcripts (2026-11-17),
/// between 250k and 500k of context: a worker's context grew by 17k in five
/// minutes at the median and 47k at the 95th percentile, and a worker's
/// last eleven calls, about what a wrap-up takes (commit, push, a note, the
/// report), added 7k at the median and 16k at the 95th percentile. At 85%
/// the 67.5k left hold a manager that calls within five minutes plus a
/// wrap-up, both at the 95th percentile (63k); at 90% the 45k would not,
/// at the 90th (36k + 13k). A manager idle longer hears of it later.
pub const NEAR_PERCENT: u64 = 85;

/// At most one look per manager session in this span: a worker's context
/// grows about 2k in it at the median.
pub const LOOK_EVERY_MS: u64 = 30_000;

/// The look markers' directory, under the feed directory.
pub const DIR: &str = "cap";

/// How long a look marker is kept after its session last looked.
const MARKS_KEEP_MS: u64 = 24 * 60 * 60 * 1000;

/// The context at which a worker under `cap` is told of it:
/// [`NEAR_PERCENT`] of it.
pub fn near_point(cap: u64) -> u64 {
    (u128::from(cap) * u128::from(NEAR_PERCENT) / 100) as u64
}

/// How far a worker's context has come: near the cap, or past it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Near,
    Over,
}

impl Level {
    /// Where `context` stands against `cap`; `None` under the point.
    fn of(context: u64, cap: u64) -> Option<Level> {
        if context > cap {
            Some(Level::Over)
        } else if context >= near_point(cap) {
            Some(Level::Near)
        } else {
            None
        }
    }

    fn word(self) -> &'static str {
        match self {
            Level::Near => "near",
            Level::Over => "over",
        }
    }

    fn parse(s: &str) -> Option<Level> {
        match s {
            "near" => Some(Level::Near),
            "over" => Some(Level::Over),
            _ => None,
        }
    }
}

/// The level the row's `cap_told` says was told of worker `agent`.
fn told(row: &Map<String, Value>, agent: &str) -> Option<Level> {
    let t = row.get("cap_told")?;
    if t.get("agent").and_then(Value::as_str) != Some(agent) {
        return None;
    }
    Level::parse(t.get("at")?.as_str()?)
}

/// A row this looks at: Running, not paused, with a worker. Its key and
/// worker.
fn watched(row: &Map<String, Value>) -> Option<(String, String)> {
    if manage::stage_of(row) != Some(feed::Stage::Running) || row.contains_key("paused_since") {
        return None;
    }
    let agent = row
        .get("agent_id")
        .and_then(Value::as_str)
        .filter(|a| !a.is_empty())?;
    let key = row.get("key").and_then(Value::as_str).unwrap_or("?");
    Some((key.to_string(), agent.to_string()))
}

/// The line the manager reads.
fn line(key: &str, agent: &str, context: u64, cap: u64, level: Level) -> String {
    let (ctx, cap) = (
        crate::tokens::fmt_tokens(context),
        crate::tokens::fmt_tokens(cap),
    );
    let what = format!(
        "SendMessage {agent} to come to a safe point, commit and push its work, write its \
         next step where the project keeps it, and end its turn with a report; then give \
         the rest of `{key}` to a fresh worker, its description naming `{key}`, with a \
         brief saying where it stopped. Giverny stops nothing."
    );
    match level {
        Level::Near => format!(
            "Giverny: `{key}`'s worker {agent} carries {ctx} context, near the {cap} cap on a \
             worker's context (Settings → Management panel). Wrap it up before it passes it: \
             {what}"
        ),
        Level::Over => format!(
            "Giverny: `{key}`'s worker {agent} carries {ctx} context, past the {cap} cap on a \
             worker's context. If it is still working, wrap it up now: {what}"
        ),
    }
}

/// Tell and stamp, in `doc`, each watched row whose worker's context
/// (`contexts`, by worker) has crossed the point or the cap since it was
/// last told; clear the stamp of one found back under the point. The lines,
/// and whether `doc` changed.
fn apply(doc: &mut Value, cap: u64, contexts: &HashMap<String, u64>) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut changed = false;
    let Some(rows) = doc.get_mut("rows").and_then(Value::as_array_mut) else {
        return (lines, false);
    };
    for row in rows.iter_mut().filter_map(Value::as_object_mut) {
        let Some((key, agent)) = watched(row) else {
            continue;
        };
        let Some(&context) = contexts.get(&agent) else {
            continue; // nothing to read: nothing known
        };
        match Level::of(context, cap) {
            None => changed |= row.remove("cap_told").is_some(),
            Some(level) if told(row, &agent).is_some_and(|t| t >= level) => {}
            Some(level) => {
                row.insert(
                    "cap_told".into(),
                    json!({"agent": agent, "at": level.word()}),
                );
                changed = true;
                lines.push(line(&key, &agent, context, cap, level));
            }
        }
    }
    (lines, changed)
}

/// The look itself, on the manager session's `file`: the limits from
/// `limits` (read only when a worker is Running), each Running worker's
/// context from `context` (the session's ids, the worker). Stamps under the
/// feed's lock, on a fresh read. What to tell, if anything.
fn look(
    file: &Path,
    limits: impl FnOnce() -> WorkerContext,
    context: impl Fn(&[String], &str) -> Option<u64>,
) -> Option<String> {
    let read = || {
        std::fs::read(file)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .filter(|d| manage::writer_of(d) == Some(manage::WRITER))
    };
    let mut doc = read()?;
    let agents: Vec<String> = doc
        .get("rows")?
        .as_array()?
        .iter()
        .filter_map(Value::as_object)
        .filter_map(watched)
        .map(|(_, agent)| agent)
        .collect();
    if agents.is_empty() {
        return None;
    }
    let cap = limits().context_cap_tokens;
    let sessions = continuation::ids(&doc);
    let contexts: HashMap<String, u64> = agents
        .into_iter()
        .filter_map(|a| Some((a.clone(), context(&sessions, &a)?)))
        .collect();
    if !apply(&mut doc, cap, &contexts).1 {
        return None;
    }
    let _lock = Lock::take(file).ok()?;
    let mut doc = read()?;
    let (lines, changed) = apply(&mut doc, cap, &contexts);
    if changed {
        manage::write(file, &doc).ok()?;
    }
    (!lines.is_empty()).then(|| lines.join("\n\n"))
}

/// The context worker `agent` carries now: its transcript beside the
/// dispatcher's (`transcript`, `<project>/<session>.jsonl`), under the
/// dispatcher's session or one of `sessions` (its ids before a re-id);
/// without the dispatcher's transcript, as `giverny manage` finds it.
fn context_of(transcript: Option<&Path>, sessions: &[String], agent: &str) -> Option<u64> {
    let Some(t) = transcript else {
        return manage::worker_context(None, sessions, agent);
    };
    let project = t.parent()?;
    let own = t.file_stem().and_then(|s| s.to_str());
    own.into_iter()
        .chain(sessions.iter().map(String::as_str))
        .find_map(|s| {
            let dir = project.join(s).join("subagents");
            crate::tokens::tokens_of(&crate::subagents::agent_transcript(&dir, agent))
        })
}

/// Whether `session`'s look is due at `now`: none within
/// [`LOOK_EVERY_MS`] of the last, timed by the mtime of
/// `<feeds>/cap/<session>`, touched when it says yes. A new session's first
/// marker sweeps the ones a day old.
fn look_due(dir: &Path, session: &str, now: u64) -> bool {
    let marks = dir.join(DIR);
    let marker = marks.join(session);
    let ms_of = |t: std::time::SystemTime| {
        t.duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    };
    let last = std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .map(ms_of)
        .ok();
    if last.is_some_and(|t| t <= now && now - t < LOOK_EVERY_MS) {
        return false;
    }
    if last.is_none() {
        let _ = std::fs::create_dir_all(&marks);
        for e in std::fs::read_dir(&marks).into_iter().flatten().flatten() {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| now.saturating_sub(ms_of(t)) > MARKS_KEEP_MS);
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    if let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&marker)
    {
        let _ = f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_millis(now));
    }
    true
}

/// On the dispatcher's own call: each Running worker of `session`'s
/// manager session that has neared or passed the working cap, told once per
/// crossing (see the module's doc). `dir` is the feed directory,
/// `transcript` the dispatcher's own (the hook payload's
/// `transcript_path`).
pub fn deliver(dir: &Path, session: &str, transcript: Option<&Path>, now: u64) -> Option<String> {
    deliver_with(dir, session, transcript, now, WorkerContext::load)
}

/// [`deliver`], with the limits from `limits` (tests: not the machine's
/// `config.toml`).
pub fn deliver_with(
    dir: &Path,
    session: &str,
    transcript: Option<&Path>,
    now: u64,
    limits: impl FnOnce() -> WorkerContext,
) -> Option<String> {
    if session.is_empty() || session.contains(['/', '\\']) || session.starts_with('.') {
        return None;
    }
    let direct = feed::feed_path(dir, session);
    let file = if direct.is_file() {
        direct
    } else if crate::managers::is_marked(dir, session, &[], || None) {
        manage::file_for(dir, session)
    } else {
        return None;
    };
    if !look_due(dir, session, now) {
        return None;
    }
    look(&file, limits, |sessions, agent| {
        context_of(transcript, sessions, agent)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("giverny-cap-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn limits() -> WorkerContext {
        WorkerContext {
            reuse_max_tokens: 200_000,
            context_cap_tokens: 450_000,
        }
    }

    /// A manager session's feed: `t1` Running with worker `w1`, `t2` Running
    /// but paused (worker `w2`), `t3` Next up, `t4` Done (worker `w4`),
    /// `t5` Running with worker `w5`.
    fn feed(file: &Path) {
        let mut doc = manage::new_doc("s1");
        doc["rows"] = json!([
            {"key": "t1", "stage": "running", "agent_id": "w1"},
            {"key": "t2", "stage": "running", "agent_id": "w2",
             "paused_since": "2026-11-17T10:00:00Z"},
            {"key": "t3", "stage": "planned"},
            {"key": "t4", "stage": "done", "agent_id": "w4"},
            {"key": "t5", "stage": "running", "agent_id": "w5"},
        ]);
        manage::write(file, &doc).unwrap();
    }

    fn row(file: &Path, key: &str) -> Value {
        let doc: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        doc["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == key)
            .unwrap()
            .clone()
    }

    #[test]
    fn the_point_is_85_percent_of_the_cap() {
        assert_eq!(near_point(450_000), 382_500);
        assert_eq!(Level::of(382_499, 450_000), None);
        assert_eq!(Level::of(382_500, 450_000), Some(Level::Near));
        assert_eq!(Level::of(450_000, 450_000), Some(Level::Near));
        assert_eq!(Level::of(450_001, 450_000), Some(Level::Over));
        assert_eq!(
            near_point(u64::MAX),
            (u128::from(u64::MAX) * 85 / 100) as u64
        );
    }

    /// Told once when a Running worker crosses the point, not again on the
    /// next look, once more past the cap; never for a worker under it, a
    /// paused row, or a row not Running, however heavy its worker.
    #[test]
    fn a_running_worker_is_told_once_near_the_cap_and_once_past_it() {
        let dir = scratch("once");
        let file = dir.join("s1.json");
        feed(&file);
        let ctx: RefCell<HashMap<&str, u64>> = RefCell::new(HashMap::from([
            ("w1", 100_000),
            ("w2", 900_000),
            ("w4", 900_000),
            ("w5", 300_000),
        ]));
        let at = || look(&file, limits, |_, a| ctx.borrow().get(a).copied());
        assert_eq!(at(), None, "everyone Running is under the point");
        let before = std::fs::read(&file).unwrap();
        assert_eq!(at(), None);
        assert_eq!(std::fs::read(&file).unwrap(), before, "nothing written");

        ctx.borrow_mut().insert("w1", 390_000);
        let told = at().expect("w1 crossed 382.5k");
        assert!(
            told.contains("`t1`'s worker w1 carries 390k context"),
            "{told}"
        );
        assert!(told.contains("near the 450k cap"), "{told}");
        assert!(told.contains("SendMessage w1"), "{told}");
        assert!(told.contains("commit and push"), "{told}");
        assert!(told.contains("its description naming `t1`"), "{told}");
        assert!(told.contains("Giverny stops nothing"), "{told}");
        assert!(
            !told.contains("t2") && !told.contains("t4") && !told.contains("t5"),
            "{told}"
        );
        assert_eq!(
            row(&file, "t1")["cap_told"],
            json!({"agent": "w1", "at": "near"})
        );

        ctx.borrow_mut().insert("w1", 400_000);
        assert_eq!(at(), None, "told once near the cap");

        ctx.borrow_mut().insert("w1", 460_000);
        let past = at().expect("past the cap is told once more");
        assert!(
            past.contains("carries 460k context, past the 450k cap"),
            "{past}"
        );
        assert_eq!(row(&file, "t1")["cap_told"]["at"], "over");
        ctx.borrow_mut().insert("w1", 480_000);
        assert_eq!(at(), None, "and only once");

        // The paused row and the Done one never were told, nor stamped.
        assert!(row(&file, "t2").get("cap_told").is_none());
        assert!(row(&file, "t4").get("cap_told").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A compaction brings the worker back under the point: its stamp goes,
    /// and the next crossing is told again. A row handed to another worker
    /// is told of that one.
    #[test]
    fn a_new_crossing_or_a_new_worker_is_told_again() {
        let dir = scratch("again");
        let file = dir.join("s1.json");
        feed(&file);
        let ctx: RefCell<HashMap<&str, u64>> = RefCell::new(HashMap::from([("w1", 460_000)]));
        let at = || look(&file, limits, |_, a| ctx.borrow().get(a).copied());
        let jumped = at().expect("straight past the cap");
        assert!(jumped.contains("past the 450k cap"), "{jumped}");
        ctx.borrow_mut().insert("w1", 390_000);
        assert_eq!(at(), None, "back to near after past is no new crossing");
        ctx.borrow_mut().insert("w1", 120_000);
        assert_eq!(at(), None);
        assert!(
            row(&file, "t1").get("cap_told").is_none(),
            "cleared under the point"
        );
        ctx.borrow_mut().insert("w1", 385_000);
        assert!(at().is_some_and(|l| l.contains("near the 450k cap")));

        let mut doc: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        doc["rows"][0]["agent_id"] = json!("w9");
        manage::write(&file, &doc).unwrap();
        ctx.borrow_mut().insert("w9", 390_000);
        let fresh = at().expect("a new worker on the row");
        assert!(fresh.contains("worker w9"), "{fresh}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The hook's way in: only a manager session's own feed is looked at,
    /// at most once in [`LOOK_EVERY_MS`], and the worker's context is read
    /// from its transcript beside the dispatcher's.
    #[test]
    fn deliver_reads_the_workers_transcript_and_looks_at_most_every_30s() {
        let dir = scratch("deliver");
        let feeds = dir.join("feeds");
        std::fs::create_dir_all(&feeds).unwrap();
        let project = dir.join("projects").join("-work");
        let subagents = project.join("s1").join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let transcript = project.join("s1.jsonl");
        std::fs::write(&transcript, "").unwrap();
        let worker = |n: u64| {
            std::fs::write(
                subagents.join("agent-w1.jsonl"),
                format!(
                    "{{\"type\":\"user\"}}\n{{\"type\":\"assistant\",\"message\":{{\"usage\":\
                     {{\"input_tokens\":2,\"cache_creation_input_tokens\":0,\
                     \"cache_read_input_tokens\":{}}}}}}}\n",
                    n - 2
                ),
            )
            .unwrap();
        };
        let now = 1_790_000_000_000u64;
        let go =
            |session: &str, at: u64| deliver_with(&feeds, session, Some(&transcript), at, limits);
        worker(390_000);
        assert_eq!(go("s1", now), None, "no feed: no look, no marker");
        assert!(!feeds.join(DIR).join("s1").exists());
        feed(&feeds.join("s1.json"));
        let told = go("s1", now).expect("its worker is near the cap");
        assert!(
            told.contains("`t1`'s worker w1 carries 390k context"),
            "{told}"
        );
        worker(460_000);
        assert_eq!(go("s1", now + 10_000), None, "within 30s of the last look");
        let past = go("s1", now + LOOK_EVERY_MS).expect("the next look");
        assert!(past.contains("past the 450k cap"), "{past}");
        assert_eq!(go("other", now + 2 * LOOK_EVERY_MS), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

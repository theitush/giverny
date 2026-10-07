//! Agent ETAs: how long any subagent is expected to take, whether or not an
//! orchestrator session runs it.
//!
//! An orchestrator session keeps its tasks — plans, landings, the history its
//! estimates learn from — in its own file ([`crate::orchestrator_session`]).
//! A worker that is no orchestrator's task (a plain session's batch of
//! workers, an Explore search, a one-off helper) still sits on the agents
//! pane, and its estimate lives here: one file per Claude session,
//! `<feed dir>/agent-etas/<session>.json`, keyed by the agent id Claude Code
//! gave the worker. No task key, no landing, no history: an estimate, when it
//! was given, and the first one given.
//!
//! **Who gives it.** Whoever spawns a worker estimates it: the plugin's hook
//! ([`crate::plugin_hook`]) asks the dispatcher right after its `Agent` call
//! ([`dispatcher_ask`]), and the dispatcher runs `giverny eta <agent-id>
//! <minutes>`. Five minutes into its work the worker is asked, once, to
//! re-estimate ([`worker_ask`]), with the same command, and again as each
//! figure runs out: once with five minutes of it left, once past it
//! ([`deadline_ask`]). No ask is made for a worker an orchestrator session's
//! row holds: that row is its estimate.
//!
//! ```json
//! { "version": 1, "session": "<id>",
//!   "agents": { "<agent id>": { "left_s": 480, "at": "2026-10-06T10:06:30Z",
//!                               "first_left_s": 600, "first_at": "…", "note": "…" } } }
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Map, Value, json};

use crate::continuation;
use crate::orchestrator_session::{self, Lock};

/// Under the feed directory: a directory, so the pane's search for feeds
/// (`*.json` directly in the feed directory) never takes one for a feed.
pub const DIR: &str = "agent-etas";

/// The markers that say a worker was asked to re-estimate, under [`DIR`].
const ASKED_DIR: &str = "asked";

/// How long into its work a worker is asked to re-estimate.
pub const AFTER_MS: u64 = 5 * 60 * 1000;

/// How much of an estimate is left when the worker is asked about it again.
pub const DEADLINE_LEFT_MS: u64 = 5 * 60 * 1000;

/// No near-the-end ask this soon after a re-estimate or the five-minute ask.
pub const QUIET_MS: u64 = 3 * 60 * 1000;

/// How long an asked-marker is kept: a worker outlives no day of this.
const ASKED_KEEP_MS: u64 = 24 * 60 * 60 * 1000;

pub const USAGE: &str = "\
usage: giverny eta <agent-id> <dur left> [--note N] [--session <id>]

Give a subagent's estimate to the agents pane: this much is left from now.
The dispatcher runs it right after a spawn, with the agent id the Agent tool
returned; the worker runs it again to correct the figure. <dur> is minutes
(`25`) or `25m`, `1h30m`, `1.5h`. The session is --session, else
$CLAUDE_CODE_SESSION_ID (set inside Claude Code, and a worker's is its
dispatcher's). A worker that holds an orchestrator session's task is
estimated there instead (`giverny orchestrator-session eta`).";

/// One worker's estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Eta {
    /// What was left when it was given, in seconds.
    pub left_s: u64,
    /// When it was given.
    pub at_ms: u64,
    /// The first figure given for this worker, and when.
    pub first_left_s: u64,
    pub first_at_ms: u64,
}

impl Eta {
    /// The whole estimate counted from `start_ms`, the worker's start: the
    /// time it had run when the figure was given, plus what was left then.
    pub fn total_s(&self, start_ms: u64) -> u64 {
        self.at_ms.saturating_sub(start_ms) / 1000 + self.left_s
    }
}

/// A session's estimates, by agent id.
pub type Etas = HashMap<String, Eta>;

/// The file a session's estimates live in.
pub fn path(dir: &Path, session: &str) -> PathBuf {
    dir.join(DIR).join(format!("{session}.json"))
}

/// Read through `serde_json::Value`, one field at a time: a field that is
/// not what it should be costs that worker's estimate, never the file's.
pub fn parse(bytes: &[u8]) -> Etas {
    let Ok(v) = serde_json::from_slice::<Value>(bytes) else {
        return Etas::new();
    };
    let Some(agents) = v.get("agents").and_then(Value::as_object) else {
        return Etas::new();
    };
    agents
        .iter()
        .filter_map(|(id, e)| {
            let e = e.as_object()?;
            let left_s = orchestrator_session::u64_of(e, "left_s")?;
            let at_ms = orchestrator_session::ms_of(e, "at")?;
            Some((
                id.clone(),
                Eta {
                    left_s,
                    at_ms,
                    first_left_s: orchestrator_session::u64_of(e, "first_left_s").unwrap_or(left_s),
                    first_at_ms: orchestrator_session::ms_of(e, "first_at").unwrap_or(at_ms),
                },
            ))
        })
        .collect()
}

/// A session's estimates; none when the file is missing or unreadable.
pub fn read(dir: &Path, session: &str) -> Etas {
    find(dir, session)
        .and_then(|f| std::fs::read(f).ok())
        .map(|b| parse(&b))
        .unwrap_or_default()
}

/// The file holding `session`'s estimates: its own, else one that names it
/// as an alias — the conversation's file under an earlier id, which a
/// session re-id'd mid-run adopted ([`crate::continuation`]). The newest,
/// if several do.
pub fn find(dir: &Path, session: &str) -> Option<PathBuf> {
    let own = path(dir, session);
    if own.is_file() {
        return Some(own);
    }
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir.join(DIR)).ok()?.flatten() {
        let p = entry.path();
        if p.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let names = std::fs::read(&p)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .is_some_and(|d| continuation::names(&d, session));
        if !names {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
            best = Some((mtime, p));
        }
    }
    best.map(|(_, p)| p)
}

/// The first of `sessions` (a conversation's ids, the current one first)
/// with a file: [`find`] for each in turn.
pub fn find_any(dir: &Path, sessions: &[&str]) -> Option<PathBuf> {
    sessions.iter().find_map(|s| find(dir, s))
}

/// Whether `id` can name a file: an agent id is hex, a session id a UUID.
fn safe(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Record `agent`'s estimate: `left_s` from `now`. The first one given is
/// kept beside the latest.
pub fn set(
    dir: &Path,
    session: &str,
    agent: &str,
    left_s: u64,
    note: Option<&str>,
    now: u64,
) -> Result<Eta, String> {
    set_in(dir, None, session, agent, left_s, note, now)
}

/// [`set`], reading transcripts under the Claude config dir `cfg` (else
/// [`continuation::config_dir`]): a session no file names yet takes its
/// conversation's file under an earlier id, which it then names.
pub fn set_in(
    dir: &Path,
    cfg: Option<&Path>,
    session: &str,
    agent: &str,
    left_s: u64,
    note: Option<&str>,
    now: u64,
) -> Result<Eta, String> {
    if !safe(session) {
        return Err(format!("not a session id: {session}"));
    }
    if !safe(agent) {
        return Err(format!("not an agent id: {agent}"));
    }
    let root_cell = std::cell::OnceCell::new();
    let root = || {
        root_cell
            .get_or_init(|| continuation::root_of(cfg, session))
            .as_deref()
    };
    let file = find(dir, session)
        .or_else(|| continuation::continued(&dir.join(DIR), session, root()?, cfg, |_| true))
        .unwrap_or_else(|| path(dir, session));
    let parent = file.parent().expect("joined");
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let _lock = Lock::take(&file)?;
    let mut doc = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({ "version": 1, "session": session }));
    // The conversation's file under an earlier id is this id's now.
    continuation::adopt(&mut doc, session);
    if doc.get("root").is_none() {
        continuation::stamp_root(&mut doc, root());
    }
    let obj = doc.as_object_mut().expect("filtered to objects");
    let agents = obj
        .entry("agents")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("the file's `agents` is not an object")?;
    let entry = agents.entry(agent.to_string()).or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    let e: &mut Map<String, Value> = entry.as_object_mut().expect("made an object");
    let at = orchestrator_session::stamp(now);
    e.entry("first_left_s").or_insert(json!(left_s));
    e.entry("first_at").or_insert(json!(at));
    e.insert("left_s".into(), json!(left_s));
    e.insert("at".into(), json!(at));
    match note.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => e.insert("note".into(), json!(n)),
        None => e.remove("note"),
    };
    orchestrator_session::write(&file, &doc).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(parse(&serde_json::to_vec(&doc).unwrap_or_default())
        .remove(agent)
        .unwrap_or_default())
}

/// Re-reads a session's estimates only when the file changed: one `stat`
/// a poll when nothing moved.
#[derive(Debug, Default)]
pub struct Cache {
    sessions: Vec<String>,
    file: Option<PathBuf>,
    stamp: Option<(SystemTime, u64)>,
    etas: Etas,
}

impl Cache {
    pub fn poll(&mut self, dir: &Path, session: &str) -> &Etas {
        self.poll_any(dir, &[session])
    }

    /// The estimates of a conversation known by `sessions` (its current id
    /// first, then the ids it had before a re-id): the file of the first
    /// that has one, followed while it stays, searched for again when it
    /// goes — or when the conversation's own id gets a file.
    pub fn poll_any(&mut self, dir: &Path, sessions: &[&str]) -> &Etas {
        if self
            .sessions
            .iter()
            .map(String::as_str)
            .ne(sessions.iter().copied())
        {
            *self = Cache {
                sessions: sessions.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            };
        }
        let stamp_of = |p: &Path| {
            std::fs::metadata(p)
                .ok()
                .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
        };
        let own = sessions.first().map(|s| path(dir, s));
        let keep = self.file.as_ref().filter(|f| {
            f.is_file() && (own.as_ref() == Some(*f) || !own.as_ref().is_some_and(|o| o.is_file()))
        });
        if keep.is_none() {
            self.file = find_any(dir, sessions);
        }
        let stamp = self.file.as_deref().and_then(stamp_of);
        if stamp != self.stamp {
            self.stamp = stamp;
            self.etas = self
                .file
                .as_deref()
                .and_then(|f| std::fs::read(f).ok())
                .map(|b| parse(&b))
                .unwrap_or_default();
        }
        &self.etas
    }
}

/// What the plugin's hook asks a dispatcher that just spawned `agent` in
/// the background with no estimate anywhere: give it one. `None` when this
/// session holds one for it already.
pub fn dispatcher_ask(
    dir: &Path,
    session: &str,
    agent: &str,
    description: Option<&str>,
) -> Option<String> {
    if !safe(agent) || read(dir, session).contains_key(agent) {
        return None;
    }
    let what = description
        .map(|d| d.replace(['"', '`', '\\', '$'], "'"))
        .filter(|d| !d.trim().is_empty())
        .map(|d| format!("the worker you just started, \"{}\",", d.trim()))
        .unwrap_or_else(|| "the worker you just started".into());
    Some(format!(
        "Giverny: the agents pane shows {what} with no ETA. Give it one now: run \
         `giverny-eta {agent} <minutes>` with your estimate of how long it will take. \
         The worker corrects it itself a few minutes in."
    ))
}

/// What the plugin's hook asks a worker that holds no orchestrator task,
/// [`AFTER_MS`] after its spawn (`spawned_ms`), once: correct the estimate
/// it was given, or give the first one. `None` before then, once asked, and
/// for a worker that corrected its figure itself already.
pub fn worker_ask(
    dir: &Path,
    session: &str,
    agent: &str,
    spawned_ms: u64,
    now: u64,
) -> Option<String> {
    let worked = now.saturating_sub(spawned_ms);
    if worked < AFTER_MS || !safe(agent) {
        return None;
    }
    let marker = dir.join(DIR).join(ASKED_DIR).join(agent);
    if marker.exists() {
        return None;
    }
    let eta = read(dir, session).get(agent).copied();
    // A figure given past the five minutes is the worker's own correction.
    let corrected = eta.is_some_and(|e| e.at_ms >= spawned_ms.saturating_add(AFTER_MS));
    if !mark(&marker, now) || corrected {
        return None;
    }
    let span = |ms: u64| crate::feed::fmt_span((ms / 1000) as i64);
    let estimate = match eta {
        Some(e) => {
            let total = e.total_s(spawned_ms);
            format!(
                "Your estimate was {}, so the pane shows about {} left.",
                span(total * 1000),
                span((total * 1000).saturating_sub(worked))
            )
        }
        None => "You have no estimate yet.".into(),
    };
    Some(format!(
        "Giverny: you have been working for {}. {estimate} Now that you have read into the \
         task, estimate it once: run `giverny-eta {agent} <minutes left>`, even if the \
         figure stands. Then carry on.",
        span(worked)
    ))
}

/// What the plugin's hook asks a worker that holds no orchestrator task as
/// its current figure runs out: once when [`DEADLINE_LEFT_MS`] of it are
/// left (a figure given with more than that), once when the work has run
/// past it. Each figure — its `at` — is asked about once each way, by a
/// marker `asked/<agent>.deadline` holding the last ask (`<at ms> near` or
/// `<at ms> past`); a fresh figure starts over. Nothing within [`QUIET_MS`]
/// of the figure being given or of the five-minute ask, nothing before
/// [`AFTER_MS`] (the five-minute ask comes first), and nothing with no
/// figure. Told, never applied: the figure is the worker's to give.
pub fn deadline_ask(
    dir: &Path,
    session: &str,
    agent: &str,
    spawned_ms: u64,
    now: u64,
) -> Option<String> {
    // Before its five minutes, the five-minute ask is the one to come.
    if !safe(agent) || now.saturating_sub(spawned_ms) < AFTER_MS {
        return None;
    }
    let e = read(dir, session).get(agent).copied()?;
    let asked = dir.join(DIR).join(ASKED_DIR);
    // When the five-minute ask was made: the marker's text, else its mtime.
    let five = asked.join(agent);
    let five_min_ask = std::fs::read_to_string(&five)
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok())
        .or_else(|| {
            std::fs::metadata(&five)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|t| t.as_millis() as u64)
        });
    let quiet = |t: u64| now.saturating_sub(t) < QUIET_MS;
    if quiet(e.at_ms) || five_min_ask.is_some_and(quiet) {
        return None;
    }
    let end = e.at_ms + e.left_s * 1000;
    let tag = if now > end {
        "past"
    } else if end - now <= DEADLINE_LEFT_MS && e.left_s * 1000 > DEADLINE_LEFT_MS {
        "near"
    } else {
        return None;
    };
    let marker = asked.join(format!("{agent}.deadline"));
    let line = format!("{} {tag}", e.at_ms);
    if std::fs::read_to_string(&marker).is_ok_and(|m| m.trim() == line) {
        return None;
    }
    // Once a figure is past its end, its "near" ask can never come again.
    if std::fs::create_dir_all(&asked).is_err() || std::fs::write(&marker, &line).is_err() {
        return None; // never ask on every call
    }
    let span = |ms: u64| crate::feed::fmt_span((ms / 1000) as i64);
    let said = if tag == "past" {
        format!("You have run {} past your estimate", span(now - end))
    } else {
        format!("About {} is left of your estimate", span(end - now))
    };
    Some(format!(
        "Giverny: {said} ({} in all, {} so far). Re-estimate it once: run \
         `giverny-eta {agent} <minutes left>`, even if the figure stands. Then carry on.",
        span(e.total_s(spawned_ms) * 1000),
        span(now.saturating_sub(spawned_ms))
    ))
}

/// Make the asked-marker, once, holding `now`: `false` if it was there
/// already or cannot be made (so a worker is never asked on every call).
/// Markers a day old go now.
fn mark(marker: &Path, now: u64) -> bool {
    let parent = marker.parent().expect("joined");
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    if let Ok(entries) = std::fs::read_dir(parent) {
        for e in entries.flatten() {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .is_some_and(|t| now.saturating_sub(t.as_millis() as u64) > ASKED_KEEP_MS);
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)
        .map(|mut f| {
            let _ = std::io::Write::write_all(&mut f, now.to_string().as_bytes());
        })
        .is_ok()
}

/// `giverny eta`'s arguments: agent, seconds left, note, session.
fn parse_args(args: &[String]) -> Result<(String, u64, Option<String>, Option<String>), String> {
    let mut pos = Vec::new();
    let (mut note, mut session) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--note" => note = Some(it.next().ok_or("--note needs a value")?.clone()),
            "--session" => session = Some(it.next().ok_or("--session needs a value")?.clone()),
            "-h" | "--help" | "help" => return Err(USAGE.into()),
            f if f.starts_with("--") => return Err(format!("unknown flag {f}\n\n{USAGE}")),
            _ => pos.push(a.clone()),
        }
    }
    let [agent, left] = pos.as_slice() else {
        return Err(USAGE.into());
    };
    let left_s = orchestrator_session::parse_dur(left)
        .ok_or_else(|| format!("not a duration: {left} (minutes, or 25m, 1h30m)"))?;
    Ok((agent.clone(), left_s, note, session))
}

/// `giverny eta …`: record the estimate and say what the pane will show.
pub fn main(args: &[String]) -> i32 {
    let (agent, left_s, note, session) = match parse_args(args) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let session = session
        .or_else(|| std::env::var(orchestrator_session::SESSION_ENV).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(session) = session else {
        eprintln!(
            "giverny eta: no session — run it from inside Claude Code (it sets ${}) \
             or pass --session <id>",
            orchestrator_session::SESSION_ENV
        );
        return 2;
    };
    let now = orchestrator_session::now_ms();
    match set(
        &crate::feed::feed_dir(),
        &session,
        &agent,
        left_s,
        note.as_deref(),
        now,
    ) {
        Ok(_) => {
            println!(
                "{agent}: ~{} left from now, on the agents pane",
                crate::feed::fmt_span(left_s as i64)
            );
            0
        }
        Err(e) => {
            eprintln!("giverny eta: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_790_000_000_000;
    const MIN: u64 = 60_000;

    fn temp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("giverny-agent-eta-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn an_estimate_is_kept_with_its_first_and_counted_from_the_start() {
        let dir = temp("set");
        assert!(read(&dir, "s1").is_empty());
        let e = set(&dir, "s1", "a1", 600, None, T0 + MIN).unwrap();
        assert_eq!((e.left_s, e.first_left_s), (600, 600));
        // A minute after the spawn, ten minutes left: eleven in all.
        assert_eq!(e.total_s(T0), 660);
        let e = set(&dir, "s1", "a1", 900, Some("bigger"), T0 + 6 * MIN).unwrap();
        assert_eq!((e.left_s, e.first_left_s), (900, 600));
        assert_eq!(e.first_at_ms, T0 + MIN);
        assert_eq!(e.total_s(T0), 6 * 60 + 900);
        set(&dir, "s1", "a2", 60, None, T0).unwrap();
        assert_eq!(read(&dir, "s1").len(), 2);
        assert!(read(&dir, "s2").is_empty(), "per session");
        // Nothing a feed search would take for a feed.
        assert!(!dir.join("s1.json").exists());
        assert!(set(&dir, "s1", "../x", 60, None, T0).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_field_costs_that_worker_only() {
        let etas = parse(
            br#"{"agents":{"a":{"left_s":60,"at":"2026-10-06T10:00:00Z"},
                           "b":{"left_s":"soon"},"c":7}}"#,
        );
        assert_eq!(etas.len(), 1);
        assert_eq!(etas["a"].first_left_s, 60);
        assert!(parse(b"not json").is_empty());
    }

    #[test]
    fn the_cache_rereads_only_a_changed_file() {
        let dir = temp("cache");
        let mut c = Cache::default();
        assert!(c.poll(&dir, "s1").is_empty());
        set(&dir, "s1", "a1", 600, None, T0).unwrap();
        assert_eq!(c.poll(&dir, "s1")["a1"].left_s, 600);
        set(&dir, "s1", "a1", 1200, None, T0 + MIN).unwrap();
        assert_eq!(c.poll(&dir, "s1")["a1"].left_s, 1200);
        assert!(c.poll(&dir, "s2").is_empty(), "another session");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_dispatcher_is_asked_for_a_worker_with_no_estimate() {
        let dir = temp("dispatcher");
        let ask = dispatcher_ask(&dir, "s1", "a1", Some("Classify \"chunk\" 0")).unwrap();
        assert!(ask.contains("\"Classify 'chunk' 0\""), "{ask}");
        assert!(ask.contains("`giverny-eta a1 <minutes>`"), "{ask}");
        set(&dir, "s1", "a1", 480, None, T0).unwrap();
        assert_eq!(dispatcher_ask(&dir, "s1", "a1", None), None, "it has one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_worker_is_asked_once_five_minutes_in() {
        let dir = temp("worker");
        assert_eq!(
            worker_ask(&dir, "s1", "a1", T0, T0 + 4 * MIN),
            None,
            "too early"
        );
        let ask = worker_ask(&dir, "s1", "a1", T0, T0 + 5 * MIN).unwrap();
        assert!(ask.contains("no estimate yet"), "{ask}");
        assert!(ask.contains("`giverny-eta a1 <minutes left>`"), "{ask}");
        assert_eq!(worker_ask(&dir, "s1", "a1", T0, T0 + 9 * MIN), None, "once");

        // The dispatcher's figure, a minute after the spawn: 8m from then.
        set(&dir, "s1", "a2", 8 * 60, None, T0 + MIN).unwrap();
        let ask = worker_ask(&dir, "s1", "a2", T0, T0 + 6 * MIN).unwrap();
        assert!(ask.contains("was 9m") && ask.contains("3m left"), "{ask}");

        // A worker that corrected its own figure past five minutes is let be.
        set(&dir, "s1", "a3", 60, None, T0 + 5 * MIN).unwrap();
        assert_eq!(worker_ask(&dir, "s1", "a3", T0, T0 + 6 * MIN), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_figure_is_asked_about_near_its_end_and_past_it() {
        let dir = temp("deadline");
        let ask = |at: u64| deadline_ask(&dir, "s1", "a1", T0, at);
        assert_eq!(ask(T0 + 10 * MIN), None, "no figure, no ask");
        // The dispatcher's 20m, right after the spawn; the five-minute ask.
        set(&dir, "s1", "a1", 20 * 60, None, T0).unwrap();
        assert!(worker_ask(&dir, "s1", "a1", T0, T0 + 5 * MIN).is_some());
        assert_eq!(ask(T0 + 14 * MIN), None, "6m left");
        let a = ask(T0 + 15 * MIN).unwrap();
        assert!(
            a.contains("About 5m is left") && a.contains("20m in all, 15m so far"),
            "{a}"
        );
        assert!(a.contains("`giverny-eta a1 <minutes left>`"), "{a}");
        assert_eq!(ask(T0 + 16 * MIN), None, "asked once");
        // Past it: once more.
        let a = ask(T0 + 21 * MIN).unwrap();
        assert!(a.contains("run 1m past your estimate"), "{a}");
        assert_eq!(ask(T0 + 25 * MIN), None, "once");
        // A fresh figure: quiet a while, then its own asks.
        set(&dir, "s1", "a1", 12 * 60, None, T0 + 25 * MIN).unwrap();
        assert_eq!(ask(T0 + 26 * MIN), None);
        assert_eq!(ask(T0 + 31 * MIN), None, "7m left");
        assert!(ask(T0 + 32 * MIN).unwrap().contains("About 5m"));
        assert!(ask(T0 + 38 * MIN).unwrap().contains("past"));
        assert_eq!(ask(T0 + 39 * MIN), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_short_or_fresh_figure_is_let_be_until_it_runs_out() {
        let dir = temp("deadline-short");
        // 4m given ten minutes in: never "near", only past it.
        set(&dir, "s1", "a1", 4 * 60, None, T0 + 10 * MIN).unwrap();
        let ask = |at: u64| deadline_ask(&dir, "s1", "a1", T0, at);
        assert_eq!(ask(T0 + 11 * MIN), None, "just given");
        assert_eq!(ask(T0 + 13 * MIN), None, "short figure, not near");
        assert!(ask(T0 + 15 * MIN).unwrap().contains("1m past"));
        // Right after the five-minute ask: quiet, even near the end.
        set(&dir, "s1", "a2", 9 * 60, None, T0).unwrap();
        assert!(worker_ask(&dir, "s1", "a2", T0, T0 + 5 * MIN).is_some());
        let ask = |at: u64| deadline_ask(&dir, "s1", "a2", T0, at);
        assert_eq!(ask(T0 + 6 * MIN), None, "3m left, but just asked");
        assert!(ask(T0 + 8 * MIN).unwrap().contains("About 1m"));
        assert_eq!(deadline_ask(&dir, "s1", "../x", T0, T0 + 30 * MIN), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_command_reads_agent_and_duration() {
        let a = |s: &[&str]| parse_args(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&["a1", "8"]).unwrap(), ("a1".into(), 480, None, None));
        assert_eq!(
            a(&["a1", "1h30m", "--note", "big", "--session", "s"]).unwrap(),
            ("a1".into(), 5400, Some("big".into()), Some("s".into()))
        );
        assert!(a(&["a1"]).is_err());
        assert!(a(&["a1", "soon"]).is_err());
        assert!(a(&["a1", "8", "--why", "x"]).is_err());
    }

    /// giverny#228: a session re-id'd mid-run keeps its workers'
    /// estimates. The pane finds them by the old id until the next
    /// `giverny eta`, which adopts the file under the new one.
    #[test]
    fn a_re_id_session_adopts_its_conversations_estimates() {
        use crate::continuation::tests::transcript;
        let dir = temp("reid");
        let cfg = dir.join("claude");
        transcript(&cfg, "old", "root-1");
        let c = Some(cfg.as_path());
        set_in(&dir, c, "old", "a1", 15 * 60, None, T0).unwrap();
        set_in(&dir, c, "old", "a2", 5 * 60, None, T0).unwrap();

        // Right after the re-id: nothing under the new id yet; the pane
        // looks it up by the old one too.
        transcript(&cfg, "new", "root-1");
        assert!(read(&dir, "new").is_empty());
        let mut cache = Cache::default();
        assert_eq!(cache.poll_any(&dir, &["new", "old"]).len(), 2);

        // The dispatcher's next estimate, under the new id.
        set_in(&dir, c, "new", "a1", 9 * 60, None, T0 + 6 * MIN).unwrap();
        set_in(&dir, c, "new", "a3", 4 * 60, None, T0 + 6 * MIN).unwrap();
        assert!(!path(&dir, "new").exists(), "no second file");
        for id in ["new", "old"] {
            let e = read(&dir, id);
            assert_eq!(e.len(), 3, "{id}: every worker, once");
            assert_eq!(
                e["a1"].first_left_s,
                15 * 60,
                "{id}: the first estimate stands"
            );
            assert_eq!(e["a1"].left_s, 9 * 60);
        }
        let doc: Value =
            serde_json::from_slice(&std::fs::read(path(&dir, "old")).unwrap()).unwrap();
        assert_eq!(doc["session"], "new");
        assert_eq!(doc["aliases"], json!(["old"]));
        assert_eq!(doc["root"], "root-1");
        assert_eq!(
            cache.poll_any(&dir, &["new", "old"]).len(),
            3,
            "the pane follows"
        );

        // `/clear` starts a new conversation and a new file.
        transcript(&cfg, "cleared", "root-2");
        set_in(&dir, c, "cleared", "a9", 60, None, T0).unwrap();
        assert_eq!(read(&dir, "cleared").len(), 1);
        assert_eq!(read(&dir, "new").len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

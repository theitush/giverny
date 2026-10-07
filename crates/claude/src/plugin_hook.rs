//! `giverny hook`: the plugin's `PostToolUse` hook, which runs on every tool
//! call of every session the plugin is loaded in — the dispatcher's and its
//! workers' alike — and hands each part of its work to the side that owns it.
//!
//! - **Estimates.** Whoever spawns a worker gives its ETA. Right after a
//!   dispatcher's `Agent` call that started a worker in the background, if no
//!   estimate for it exists (no orchestrator session task holds it, no agent
//!   ETA names it), the dispatcher is asked, once, to give one
//!   ([`agent_eta::dispatcher_ask`]). Five minutes into its work a worker is
//!   asked, once, to re-estimate, and again as each figure runs out (five
//!   minutes left, then past it): on its orchestrator task's row when it
//!   holds one ([`orchestrator_session_nudge::check`]), else on its agent ETA
//!   ([`agent_eta::worker_ask`], [`agent_eta::deadline_ask`]). A worker
//!   spawned in the foreground blocks its dispatcher until it is done, so
//!   only the worker's asks reach it.
//! - **An orchestrator session's upkeep.** Its leases are renewed
//!   ([`orchestrator_session_nudge::beat`]) and, on the dispatcher's own
//!   calls, its unread `ask`/`reply` messages delivered
//!   ([`orchestrator_session_inbox`]).
//!
//! **Only where something tracks the worker.** Every ask is for the pane:
//! it goes out only when the session runs in a Giverny tab
//! (`$GIVERNY_TAB_ID`, whose pane shows the worker) or has an orchestrator
//! session (its file is this writer's). Anywhere else — a plain `claude` in
//! another terminal — the hook says nothing, where a `giverny-eta` nobody
//! reads would raise a permission prompt for it.
//!
//! **Cheap when idle.** A call that is not a spawn and not a worker's costs a
//! few `stat`s and reads no file; a worker's call before its five minutes, a
//! `stat` of its spawn metadata, and after them a read of its small agent-ETA
//! file. Whatever happens the hook exits 0, so it
//! never fails the tool call it rides on.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Map, Value, json};

use crate::orchestrator_session::{self, Lock};
use crate::orchestrator_session_nudge::{beat, check, holds_row, stamp_agent};
use crate::{agent_eta, orchestrator_session_history, orchestrator_session_inbox, resources};

/// What the hook payload says about who is calling.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Caller {
    pub session: String,
    pub agent_id: String,
    /// The dispatcher's transcript, whose `<stem>/subagents/` holds the
    /// workers' spawn metadata.
    pub transcript: Option<PathBuf>,
}

impl Caller {
    /// `None` for a call that is not a subagent's.
    pub fn of(payload: &Value) -> Option<Caller> {
        Some(Caller {
            agent_id: text(payload, "agent_id")?,
            session: text(payload, "session_id")?,
            transcript: text(payload, "transcript_path").map(PathBuf::from),
        })
    }

    /// Where the worker's `agent-<id>.meta.json` is.
    fn meta(&self) -> Option<PathBuf> {
        let t = self.transcript.as_ref()?;
        Some(
            t.with_extension("")
                .join("subagents")
                .join(format!("agent-{}.meta.json", self.agent_id)),
        )
    }

    /// The worker's spawn description, from its `agent-<id>.meta.json`.
    fn description(&self) -> Option<String> {
        let t = self.transcript.as_ref()?;
        let dir = t.with_extension("").join("subagents");
        crate::subagents::read_meta(&dir, &self.agent_id).description
    }

    /// When the worker was spawned: its metadata file, written once then.
    fn spawned_ms(&self) -> Option<u64> {
        let t = std::fs::metadata(self.meta()?).ok()?.modified().ok()?;
        Some(t.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_millis() as u64)
    }
}

/// A payload field as trimmed, non-empty text.
fn text(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
}

/// The few fields of a hook payload the hook reads, as a small JSON object:
/// a `PostToolUse` payload carries the whole tool response, which is not
/// built into a tree only to be dropped. Only a dispatcher's `Agent` call
/// is read twice, for the worker it started (`spawned_agent`, its
/// `spawned_description`, and `spawned_background` when it runs on).
pub fn payload_of(input: &str) -> Option<Value> {
    #[derive(serde::Deserialize)]
    struct Hook {
        session_id: Option<String>,
        agent_id: Option<String>,
        transcript_path: Option<String>,
        hook_event_name: Option<String>,
        tool_name: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Spawn {
        tool_input: Option<Value>,
        tool_response: Option<Value>,
    }
    let h: Hook = serde_json::from_str(input).ok()?;
    let mut m = Map::new();
    let spawn = matches!(h.tool_name.as_deref(), Some("Agent" | "Task")) && h.agent_id.is_none();
    for (k, v) in [
        ("session_id", h.session_id),
        ("agent_id", h.agent_id),
        ("transcript_path", h.transcript_path),
        ("hook_event_name", h.hook_event_name),
        ("tool_name", h.tool_name),
    ] {
        if let Some(v) = v {
            m.insert(k.into(), Value::String(v));
        }
    }
    if spawn && let Ok(s) = serde_json::from_str::<Spawn>(input) {
        let r = s.tool_response.unwrap_or_default();
        let field = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(String::from);
        if let Some(id) = field(&r, "agentId") {
            m.insert("spawned_agent".into(), json!(id));
        }
        let description = field(&r, "description")
            .or_else(|| s.tool_input.as_ref().and_then(|i| field(i, "description")));
        if let Some(d) = description {
            m.insert("spawned_description".into(), json!(d));
        }
        let background = r.get("status").and_then(Value::as_str) == Some("async_launched")
            || r.get("isAsync").and_then(Value::as_bool) == Some(true);
        m.insert("spawned_background".into(), json!(background));
    }
    Some(Value::Object(m))
}

/// Whether this process runs for a session a Giverny pane shows: a tab's
/// own — `$GIVERNY_TAB_ID`, which the app sets in a tab's shell and a hook
/// inherits, and no other claude between it and the tab's
/// ([`crate::lineage`]), as a claude nested in the tab is not the tab's —
/// or a background job's own session, which the tab showing the job shows
/// (giverny#244). A claude nested in a job is neither.
pub fn in_giverny_tab() -> bool {
    if crate::lineage::in_bg_job() {
        return crate::lineage::bg_job().is_some();
    }
    crate::lineage::giverny_var("GIVERNY_TAB_ID").is_some()
        && crate::lineage::of_this_process().is_tabs()
}

/// The session's orchestrator session, when it has one of this writer's:
/// its file and document. `Err` for a file another writer owns.
fn orchestrator_doc(dir: &Path, session: &str) -> Result<Option<(PathBuf, Value)>, ()> {
    let file = orchestrator_session::file_for(dir, session);
    let Some(bytes) = std::fs::read(&file).ok() else {
        return Ok(None);
    };
    let Ok(doc) = serde_json::from_slice::<Value>(&bytes) else {
        return Err(());
    };
    if orchestrator_session::writer_of(&doc) != Some(orchestrator_session::WRITER) {
        return Err(());
    }
    Ok(Some((file, doc)))
}

/// The hook's whole run: `payload` is its stdin ([`payload_of`]), `dir` the
/// feed directory, `in_tab` whether the session runs in a Giverny tab
/// ([`in_giverny_tab`]). Returns what to print (the hook reply), or nothing.
pub fn run(payload: &Value, dir: &Path, now: u64, in_tab: bool) -> Option<String> {
    run_with(payload, dir, now, in_tab, true)
}

/// [`run`], with the agent-ETA asks on or off: off under the hook's old
/// name ([`main`]), whose plugin has no `giverny-eta` to answer them with.
fn run_with(
    payload: &Value,
    dir: &Path,
    now: u64,
    in_tab: bool,
    agent_etas: bool,
) -> Option<String> {
    let session = payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.contains(['/', '\\']) && !s.starts_with('.'))?;
    let ledger = resources::ledger_path(dir);
    beat(&ledger, Some(dir), session, now);
    if let Some(caller) = Caller::of(payload) {
        return worker(&caller, dir, now, in_tab, agent_etas).map(|t| reply(&t));
    }
    // The dispatcher's own call: its messages, and the worker it just started.
    let asks: Vec<String> = [
        orchestrator_session_inbox::deliver(dir, &ledger, session, now),
        spawned(payload, dir, session, in_tab).filter(|_| agent_etas),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!asks.is_empty()).then(|| reply(&asks.join("\n\n")))
}

/// A dispatcher's `Agent` call that started a worker in the background with
/// no estimate anywhere: ask the dispatcher for one.
fn spawned(payload: &Value, dir: &Path, session: &str, in_tab: bool) -> Option<String> {
    let agent = text(payload, "spawned_agent")?;
    if payload.get("spawned_background").and_then(Value::as_bool) != Some(true) {
        return None; // it has run to its end already
    }
    let description = text(payload, "spawned_description");
    match orchestrator_doc(dir, session) {
        Err(()) => return None, // another writer's feed: its rows, its estimates
        Ok(Some((_, doc))) => {
            if holds_row(&doc, &agent, description.as_deref()) {
                return None; // an orchestrator session's task: its row is its estimate
            }
        }
        Ok(None) if !in_tab => return None, // nothing would show this worker
        Ok(None) => {}
    }
    agent_eta::dispatcher_ask(dir, session, &agent, description.as_deref())
}

/// A worker's call: its re-estimate ask, when something tracks it — a
/// Giverny tab, or an orchestrator session.
fn worker(caller: &Caller, dir: &Path, now: u64, in_tab: bool, agent_etas: bool) -> Option<String> {
    let description = caller.description();
    let desc = description.as_deref();
    match orchestrator_doc(dir, &caller.session) {
        Err(()) => return None,
        Ok(Some((file, d))) if holds_row(&d, &caller.agent_id, desc) => {
            // Under the lock, re-read: the dispatcher may be writing it.
            let _lock = Lock::take(&file).ok()?;
            let mut d: Value = serde_json::from_slice(&std::fs::read(&file).ok()?).ok()?;
            let stamped = stamp_agent(&mut d, &caller.agent_id, desc);
            let ask = check(
                &mut d,
                &caller.agent_id,
                desc,
                now,
                orchestrator_session_history::path(dir).as_deref(),
            );
            if stamped || ask.is_some() {
                orchestrator_session::write(&file, &d).ok()?;
            }
            return ask;
        }
        Ok(Some(_)) => {}
        Ok(None) if !in_tab => return None, // nothing would show this worker
        Ok(None) => {}
    }
    if !agent_etas {
        return None;
    }
    let spawned = caller.spawned_ms()?;
    agent_eta::worker_ask(dir, &caller.session, &caller.agent_id, spawned, now)
        .or_else(|| agent_eta::deadline_ask(dir, &caller.session, &caller.agent_id, spawned, now))
}

/// The `PostToolUse` reply carrying `text` into the caller's context.
pub fn reply(text: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": text
        }
    })
    .to_string()
}

/// `giverny hook`: the payload on stdin, the reply (if any) on stdout, exit 0
/// whatever happens. `agent_etas` is false under the hook's old name,
/// `giverny orchestrator-session nudge`: the plugin a Giverny of before
/// wrote, which has no `giverny-eta`, so only an orchestrator session's
/// part runs until that Giverny restarts and writes the new one.
pub fn main(agent_etas: bool) -> i32 {
    let mut input = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    if let Some(payload) = payload_of(&input)
        && let Some(out) = run_with(
            &payload,
            &crate::feed::feed_dir(),
            orchestrator_session::now_ms(),
            in_giverny_tab(),
            agent_etas,
        )
    {
        println!("{out}");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed;

    const T0: u64 = 1_790_000_000_000;
    const MIN: u64 = 60_000;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("giverny-hook-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn doc(rows: Value) -> Value {
        json!({"version": 1, "session": "s1", "writer": orchestrator_session::WRITER, "rows": rows})
    }

    fn context(out: &str) -> String {
        let v: Value = serde_json::from_str(out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// A worker's spawn metadata beside the dispatcher's transcript, made
    /// at `spawned`: the payload of that worker's calls.
    fn worker_payload(dir: &Path, session: &str, agent: &str, desc: &str, spawned: u64) -> Value {
        let t = dir.join(format!("proj/{session}.jsonl"));
        let sub = dir.join(format!("proj/{session}/subagents"));
        std::fs::create_dir_all(&sub).unwrap();
        let meta = sub.join(format!("agent-{agent}.meta.json"));
        std::fs::write(&meta, json!({"description": desc}).to_string()).unwrap();
        let f = std::fs::File::options().write(true).open(&meta).unwrap();
        f.set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(spawned))
            .unwrap();
        json!({"session_id": session, "agent_id": agent,
               "transcript_path": t.display().to_string(), "hook_event_name": "PostToolUse"})
    }

    /// What Claude Code sends after a dispatcher's `Agent` call.
    fn spawn_input(session: &str, agent: &str, desc: &str, background: bool) -> String {
        let status = if background {
            "async_launched"
        } else {
            "completed"
        };
        json!({"session_id": session, "hook_event_name": "PostToolUse", "tool_name": "Agent",
               "tool_input": {"description": desc, "prompt": "…"},
               "tool_response": {"isAsync": background, "status": status, "agentId": agent,
                                 "description": desc, "prompt": "…"}})
        .to_string()
    }

    #[test]
    fn a_spawn_is_read_from_the_agent_call_and_nothing_else() {
        let p = payload_of(&spawn_input("s1", "a1", "Classify chunk 0", true)).unwrap();
        assert_eq!(p["spawned_agent"], "a1");
        assert_eq!(p["spawned_description"], "Classify chunk 0");
        assert_eq!(p["spawned_background"], true);
        assert!(p.get("tool_response").is_none(), "the response is not kept");
        let p =
            payload_of(r#"{"session_id":"s1","tool_name":"Bash","tool_response":{"agentId":"x"}}"#)
                .unwrap();
        assert!(
            p.get("spawned_agent").is_none(),
            "only an Agent call spawns"
        );
        // A worker's own Agent call is a worker's call.
        let p = payload_of(
            r#"{"session_id":"s1","agent_id":"w","tool_name":"Agent","tool_response":{"agentId":"x"}}"#,
        )
        .unwrap();
        assert!(p.get("spawned_agent").is_none());
        // A response that is no object costs only the spawn.
        let p = payload_of(r#"{"session_id":"s1","tool_name":"Agent","tool_response":"done"}"#)
            .unwrap();
        assert_eq!(p["session_id"], "s1");
        assert!(p.get("spawned_agent").is_none());
    }

    #[test]
    fn the_dispatcher_is_asked_for_each_background_worker_with_no_estimate() {
        let dir = temp("dispatcher");
        let run_in = |input: &str, in_tab: bool| run(&payload_of(input).unwrap(), &dir, T0, in_tab);
        let out = run_in(&spawn_input("s1", "a1", "Classify chunk 0", true), true).unwrap();
        let ctx = context(&out);
        assert!(ctx.contains("\"Classify chunk 0\""), "{ctx}");
        assert!(ctx.contains("`giverny-eta a1 <minutes>`"), "{ctx}");
        // Each worker its own ask; none for one already estimated.
        assert!(run_in(&spawn_input("s1", "a2", "Classify chunk 1", true), true).is_some());
        agent_eta::set(&dir, "s1", "a3", 600, None, T0).unwrap();
        assert_eq!(run_in(&spawn_input("s1", "a3", "x", true), true), None);
        // A foreground worker has finished by the time the call returns.
        assert_eq!(run_in(&spawn_input("s1", "a4", "x", false), true), None);
        // Nothing would show it outside a tab, with no orchestrator session.
        assert_eq!(run_in(&spawn_input("s1", "a5", "x", true), false), None);
        // The dispatcher's other calls read nothing and say nothing.
        assert_eq!(run(&json!({"session_id": "s1"}), &dir, T0, true), None);
        assert!(!feed::feed_path(&dir, "s1").exists(), "no feed made");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_orchestrator_sessions_task_is_never_asked_about() {
        let dir = temp("orchestrated");
        orchestrator_session::write(
            &feed::feed_path(&dir, "s1"),
            &doc(json!([{"key": "acme#7", "stage": "running", "started": orchestrator_session::stamp(T0)}])),
        )
        .unwrap();
        let run_in = |input: &str| run(&payload_of(input).unwrap(), &dir, T0, false);
        // The spawn names its started task: its row is its estimate.
        assert_eq!(
            run_in(&spawn_input("s1", "w1", "acme#7: fix it", true)),
            None
        );
        // A helper beside the tasks is asked for, tab or not.
        let ctx = context(&run_in(&spawn_input("s1", "w2", "Find the loader", true)).unwrap());
        assert!(ctx.contains("giverny-eta w2"), "{ctx}");
        // Another writer's feed: none of ours to ask about.
        std::fs::write(
            feed::feed_path(&dir, "s3"),
            r#"{"version":1,"session":"s3","writer":"other/status-writer","rows":[]}"#,
        )
        .unwrap();
        assert_eq!(
            run(
                &payload_of(&spawn_input("s3", "w3", "x", true)).unwrap(),
                &dir,
                T0,
                true
            ),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_worker_with_no_task_re_estimates_its_agent_eta_five_minutes_in_and_at_its_end() {
        let dir = temp("worker");
        let feeds = dir.join("feeds");
        let p = worker_payload(&dir, "s1", "w1", "Classify chunk 0", T0);
        agent_eta::set(&feeds, "s1", "w1", 8 * 60, None, T0).unwrap();
        assert_eq!(run(&p, &feeds, T0 + 4 * MIN, true), None, "too early");
        let ctx = context(&run(&p, &feeds, T0 + 5 * MIN, true).unwrap());
        assert!(ctx.contains("`giverny-eta w1 <minutes left>`"), "{ctx}");
        assert!(ctx.contains("was 8m"), "{ctx}");
        assert_eq!(run(&p, &feeds, T0 + 7 * MIN, true), None, "asked once");
        // Past the 8m: asked once more.
        let ctx = context(&run(&p, &feeds, T0 + 9 * MIN, true).unwrap());
        assert!(
            ctx.contains("1m past") && ctx.contains("giverny-eta w1"),
            "{ctx}"
        );
        assert_eq!(run(&p, &feeds, T0 + 10 * MIN, true), None, "once");
        // Outside a tab, with no orchestrator session: silent, nothing made.
        let q = worker_payload(&dir, "s2", "w2", "x", T0);
        for k in 0..10 {
            assert_eq!(run(&q, &feeds, T0 + k * MIN, false), None);
        }
        assert!(!feed::feed_path(&feeds, "s2").exists(), "no feed made");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn under_its_old_name_the_hook_asks_for_no_agent_eta() {
        let dir = temp("old-name");
        let p = payload_of(&spawn_input("s1", "a1", "x", true)).unwrap();
        assert_eq!(run_with(&p, &dir, T0, true, false), None);
        let w = worker_payload(&dir, "s1", "w1", "x", T0);
        assert_eq!(run_with(&w, &dir, T0 + 6 * MIN, true, false), None);
        assert!(run_with(&w, &dir, T0 + 6 * MIN, true, true).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_task_worker_re_estimates_its_row_and_the_hook_round_trips() {
        let dir = temp("task");
        let feeds = dir.join("feeds");
        std::fs::create_dir_all(&feeds).unwrap();
        let f = feed::feed_path(&feeds, "s1");
        let d = doc(json!([{"key": "demo#143", "stage": "running",
                            "started": orchestrator_session::stamp(T0), "eta_s": 4500}]));
        orchestrator_session::write(&f, &d).unwrap();
        let p = worker_payload(&dir, "s1", "abc", "demo#143: better estimates", T0);
        assert_eq!(run(&p, &feeds, T0 + MIN, false), None);
        let ctx = context(&run(&p, &feeds, T0 + 6 * MIN, false).unwrap());
        assert!(
            ctx.contains("giverny-orchestrator-session eta demo#143"),
            "{ctx}"
        );
        assert_eq!(run(&p, &feeds, T0 + 7 * MIN, false), None, "asked once");
        let back = feed::read(&f).unwrap();
        assert_eq!(back.rows.len(), 1);
        assert_eq!(back.rows[0].agent_id.as_deref(), Some("abc"), "stamped");
        // Its estimate is the row's: no agent ETA is asked for or made.
        assert!(agent_eta::read(&feeds, "s1").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

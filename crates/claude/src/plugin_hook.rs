//! `giverny hook`: the plugin's `PostToolUse` hook, which runs on every tool
//! call of every session the plugin is loaded in — the dispatcher's and its
//! workers' alike — and hands each part of its work to the side that owns it.
//! The same command is the plugin's `PreToolUse` Bash hook, which refuses a
//! command that would leave its tab's resource cap ([`guard`],
//! [`crate::bash_guard`]), and, with the manage skill in, its `PreToolUse`
//! Agent hook, which nudges a session that is no manager toward the skill
//! when a subagent it starts looks like task work ([`crate::manage_hint`],
//! giverny#358), and the plugin's
//! `UserPromptSubmit` hook, which sends a prompt that asks to manage to the
//! `giverny:manage` skill ([`crate::manage_trigger`]), and the `SessionStart`
//! hook that, with names on, asks a tab's session to name itself
//! ([`crate::names`]), and puts a tab's claude under its cap before its
//! first command ([`crate::tab_cap::on_session_start`], giverny#270)
//! — or, for a session whose start ran no such name hook, its
//! next prompt does (giverny#356) — and tells a manager's compacted
//! session it is one ([`crate::managers::start_context`],
//! giverny#448). And it is the `SubagentStop` hook, which
//! holds a subagent's finish once to ask whether a background command of its
//! own should keep running after it reports ([`crate::leftovers`],
//! giverny#370), and the `Stop` hook, which holds a session's turn's end
//! once per background command of its own that its timeout moved there or
//! that went quiet ([`crate::strays`], giverny#450).
//!
//! - **Estimates.** Whoever spawns a worker gives its ETA. Right after a
//!   dispatcher's `Agent` call that started a worker in the background, if no
//!   estimate for it exists (no manager session task holds it, no agent
//!   ETA names it), the dispatcher is asked, once, to give one
//!   ([`agent_eta::dispatcher_ask`]). Five minutes into its work a worker is
//!   asked, once, to re-estimate, and again as each figure runs out (five
//!   minutes left, then past it): on its manager task's row when it
//!   holds one ([`manage_nudge::check`]), else on its agent ETA
//!   ([`agent_eta::worker_ask`], [`agent_eta::deadline_ask`]). A task
//!   worker's first call on its row also tells it how its instinct on its
//!   kind's re-estimates runs, before it gives a figure (giverny#366, #371). A worker
//!   spawned in the foreground blocks its dispatcher until it is done, so
//!   only the worker's asks reach it.
//! - **A manager session's upkeep.** Its leases are renewed
//!   ([`manage_nudge::beat`]) and, on the dispatcher's own
//!   calls, its unread `ask`/`reply` messages delivered
//!   ([`manage_inbox`]), and its rows' chores — a finished worker's task to
//!   land, a Next up task idle with nothing to wait on — told once each
//!   ([`manage_chores`]).
//! - **What a subagent leaves running.** A subagent finishing with a
//!   background shell or monitor of its own still running is asked, once,
//!   whether it should keep running ([`crate::leftovers::subagent_stop`]);
//!   if it does, its parent session hears of it once, on its next tool call
//!   or prompt ([`crate::leftovers::deliver`]). Every subagent in a Giverny
//!   tab, managed or not (giverny#370).
//! - **What a session leaves running itself.** A shell of its own still
//!   running at its turn's end, moved to the background by its timeout or
//!   quiet for ten minutes, holds the stop once with a question
//!   ([`crate::strays::stop`]); one that goes quiet later is told once on
//!   its next tool call or prompt ([`crate::strays::between`],
//!   giverny#450).
//! - **The manager mark.** The session's own `Skill` call running the
//!   manage skill marks it a manager ([`crate::managers`], giverny#357), as
//!   does the person typing `/giverny:manage` (the prompt hook): what lets
//!   its pane open before any worker, and draw its Next up.
//! - **What happened while it was away.** A session's next call after a
//!   reboot, a restart of its `claude`, a sleep or a long absence tells it
//!   once, in one short paragraph, what that took: its processes, `/tmp`,
//!   its connections and clocks, or only the person's patience to trust
//!   the last picture — and, for a manager, the Running rows whose workers
//!   went ([`crate::away`], giverny#373). On its start, its prompt or its
//!   own tool call, every session, in a tab or not; a subagent's call only
//!   keeps the record and passes what it saw to its parent.
//!
//! **Only where something tracks the worker.** Every ask is for the pane:
//! it goes out only when the session runs in a Giverny tab
//! (`$GIVERNY_TAB_ID`, whose pane shows the worker) or has a manager
//! session (its file is this writer's). Anywhere else — a plain `claude` in
//! another terminal — the hook says nothing, where a `giverny-eta` nobody
//! reads would raise a permission prompt for it.
//!
//! **Cheap when idle.** A call that is not a spawn and not a worker's costs a
//! few `stat`s, the clocks and the session's small away record; a worker's
//! call before its five minutes, a `stat` of its spawn metadata, and after
//! them a read of its small agent-ETA file. A `SubagentStop` with no
//! background command in flight reads no file either. Whatever happens the hook exits 0, so it
//! never fails the tool call it rides on.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Map, Value, json};

use crate::manage::{self, Lock};
use crate::manage_nudge::{beat, check, holds_row, stamp_agent, track_untold};
use crate::{agent_eta, away, manage_chores, manage_history, manage_inbox, resources};

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
    let skill = h.tool_name.as_deref() == Some("Skill") && h.agent_id.is_none();
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
    // The session's own `Skill` call: which skill, for the manager mark
    // ([`crate::managers`]). Its response is the skill's text, never read.
    if skill && let Ok(s) = serde_json::from_str::<Spawn>(input) {
        let name = s
            .tool_input
            .as_ref()
            .and_then(crate::managers::skill_of)
            .map(String::from);
        if let Some(name) = name {
            m.insert("skill".into(), json!(name));
        }
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

/// Whether this process runs for a Giverny tab's own session: not a
/// background job's, not a claude nested in the tab ([`in_giverny_tab`]).
pub fn tabs_own() -> bool {
    !crate::lineage::in_bg_job()
        && crate::lineage::giverny_var("GIVERNY_TAB_ID").is_some()
        && crate::lineage::of_this_process().is_tabs()
}

/// The session's manager session, when it has one of this writer's:
/// its file and document. `Err` for a file another writer owns.
fn manager_doc(dir: &Path, session: &str) -> Result<Option<(PathBuf, Value)>, ()> {
    let file = manage::file_for(dir, session);
    let Some(bytes) = std::fs::read(&file).ok() else {
        return Ok(None);
    };
    let Ok(doc) = serde_json::from_slice::<Value>(&bytes) else {
        return Err(());
    };
    if manage::writer_of(&doc) != Some(manage::WRITER) {
        return Err(());
    }
    Ok(Some((file, doc)))
}

/// The hook's whole run: `payload` is its stdin ([`payload_of`]), `dir` the
/// feed directory, `in_tab` whether the session runs in a Giverny tab
/// ([`in_giverny_tab`]). Returns what to print (the hook reply), or nothing.
pub fn run(payload: &Value, dir: &Path, now: u64, in_tab: bool) -> Option<String> {
    run_with(payload, dir, now, in_tab, None)
}

/// [`run`], with what happened while the session was away ([`crate::away`])
/// told first on the dispatcher's own call.
pub fn run_with(
    payload: &Value,
    dir: &Path,
    now: u64,
    in_tab: bool,
    away: Option<String>,
) -> Option<String> {
    let session = payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.contains(['/', '\\']) && !s.starts_with('.'))?;
    let ledger = resources::ledger_path(dir);
    beat(&ledger, Some(dir), session, now);
    // The manage skill ran: the session is a manager from now on
    // (giverny#357), whatever it has written yet.
    if crate::managers::skill_call(payload) {
        crate::managers::mark_from(dir, payload);
    }
    if let Some(caller) = Caller::of(payload) {
        return worker(&caller, dir, now, in_tab).map(|t| reply(&t));
    }
    // The dispatcher's own call: its messages, its rows' chores, the
    // worker it just started, the commands a finished subagent left
    // running (giverny#370), and its own that went quiet (giverny#450).
    let asks: Vec<String> = [
        away,
        manage_inbox::deliver(dir, &ledger, session, now),
        resources::news(&ledger, session, now),
        manage_chores::deliver(dir, session, now),
        spawned(payload, dir, session, in_tab),
        crate::leftovers::deliver(dir, session),
        crate::strays::between(dir, session, now, &crate::strays::System),
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
    match manager_doc(dir, session) {
        Err(()) => return None, // another writer's feed: its rows, its estimates
        Ok(Some((_, doc))) => {
            if holds_row(&doc, &agent, description.as_deref()) {
                return None; // a manager session's task: its row is its estimate
            }
        }
        Ok(None) if !in_tab => return None, // nothing would show this worker
        Ok(None) => {}
    }
    agent_eta::dispatcher_ask(dir, session, &agent, description.as_deref())
}

/// A worker's call: its re-estimate ask, when something tracks it — a
/// Giverny tab, or a manager session.
fn worker(caller: &Caller, dir: &Path, now: u64, in_tab: bool) -> Option<String> {
    let description = caller.description();
    let desc = description.as_deref();
    match manager_doc(dir, &caller.session) {
        Err(()) => return None,
        Ok(Some((file, d))) if holds_row(&d, &caller.agent_id, desc) => {
            // Under the lock, re-read: the dispatcher may be writing it.
            let _lock = Lock::take(&file).ok()?;
            let mut d: Value = serde_json::from_slice(&std::fs::read(&file).ok()?).ok()?;
            let stamped = stamp_agent(&mut d, &caller.agent_id, desc);
            let untold = track_untold(&mut d, &caller.agent_id, desc);
            let ask = check(
                &mut d,
                &caller.agent_id,
                desc,
                now,
                manage_history::path(dir).as_deref(),
            );
            if stamped || untold || ask.is_some() {
                manage::write(&file, &d).ok()?;
            }
            return ask;
        }
        Ok(Some(_)) => {}
        Ok(None) if !in_tab => return None, // nothing would show this worker
        Ok(None) => {}
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

/// A `PreToolUse` payload's Bash command; `None` for any other payload
/// (read whole only when it names that event: the others carry a tool's
/// whole response).
pub fn pre_bash(input: &str) -> Option<String> {
    let p = pre_tool(input)?;
    if p.tool != "Bash" {
        return Some(String::new());
    }
    Some(p.command.unwrap_or_default())
}

/// What a `PreToolUse` payload says: the tool, its Bash command or the file
/// it edits, who calls and where.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PreTool {
    pub tool: String,
    pub command: Option<String>,
    /// Edit/Write/MultiEdit's `file_path`, NotebookEdit's `notebook_path`.
    pub file: Option<String>,
    pub session: Option<String>,
    pub cwd: Option<String>,
}

/// The tools that write a file, refused in another session's worktree
/// ([`crate::reach_guard`]).
pub const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// A `PreToolUse` payload, or `None` for any other.
pub fn pre_tool(input: &str) -> Option<PreTool> {
    #[derive(serde::Deserialize)]
    struct Input {
        command: Option<String>,
        file_path: Option<String>,
        notebook_path: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Pre {
        hook_event_name: Option<String>,
        tool_name: Option<String>,
        tool_input: Option<Input>,
        session_id: Option<String>,
        cwd: Option<String>,
    }
    if !input.contains("\"PreToolUse\"") {
        return None;
    }
    let p: Pre = serde_json::from_str(input).ok()?;
    if p.hook_event_name.as_deref() != Some("PreToolUse") {
        return None;
    }
    let (command, file) = match p.tool_input {
        Some(i) => (i.command, i.file_path.or(i.notebook_path)),
        None => (None, None),
    };
    Some(PreTool {
        tool: p.tool_name.unwrap_or_default(),
        command,
        file,
        session: p.session_id.filter(|s| !s.trim().is_empty()),
        cwd: p.cwd,
    })
}

/// The `PreToolUse` reply refusing a tool call another session's work
/// would suffer ([`crate::reach_guard`], giverny#415): a Bash command
/// killing its processes, stopping its scopes, mutating git in its
/// worktree or speaking as it, or an edit in its worktree. Anywhere the
/// plugin runs, in a tab or not; with no session known, nothing. The
/// caller (`me`) is looked up only for a call that reaches for something.
pub fn reach(
    pre: &PreTool,
    w: &dyn crate::reach_guard::World,
    me: impl FnOnce() -> crate::reach_guard::Me,
) -> Option<String> {
    use crate::reach_guard::{edit_verdict, judge, linked_root, reaches};
    let why = if pre.tool == "Bash" {
        let cmd = pre.command.as_deref()?;
        let cwd = pre
            .cwd
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())?;
        let found = reaches(cmd, &cwd);
        if found.is_empty() {
            return None;
        }
        let me = me();
        found.iter().find_map(|r| judge(r, &me, w))?
    } else if EDIT_TOOLS.contains(&pre.tool.as_str()) {
        let file = PathBuf::from(pre.file.as_deref()?);
        let file = match (file.is_absolute(), pre.cwd.as_deref()) {
            (false, Some(cwd)) => Path::new(cwd).join(file),
            _ => file,
        };
        linked_root(file.parent().unwrap_or(&file))?;
        edit_verdict(&file, &me(), w)?
    } else {
        return None;
    };
    Some(deny(&why))
}

/// The `PreToolUse` reply denying the call, `why` shown to Claude.
fn deny(why: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": why
        }
    })
    .to_string()
}

/// The `PreToolUse` reply refusing a Bash `command` that would leave its
/// tab's resource cap ([`crate::bash_guard`]), or nothing. Only in a
/// Giverny tab (`in_tab`, asked only for a command it would refuse), whose
/// claude runs capped: a claude elsewhere has no cap to leave.
pub fn guard(command: &str, in_tab: impl FnOnce() -> bool) -> Option<String> {
    let why = crate::bash_guard::verdict(command)?;
    if !in_tab() {
        return None;
    }
    Some(deny(&why))
}

/// A prompt payload's `session_id`, read alone.
fn session_of(input: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Prompt {
        session_id: Option<String>,
    }
    serde_json::from_str::<Prompt>(input).ok()?.session_id
}

/// The `UserPromptSubmit` reply carrying every context given, one paragraph
/// each: a prompt can both ask to manage ([`crate::manage_trigger`]) and be
/// the first its session's name ask rides on ([`crate::names::first_prompt`]),
/// and bring what a finished subagent left running ([`crate::leftovers`])
/// and what happened while the session was away ([`crate::away`]).
fn prompt_reply(contexts: &[Option<&str>]) -> Option<String> {
    context_reply("UserPromptSubmit", contexts)
}

/// `event`'s reply carrying every context given, one paragraph each, or
/// nothing when none is.
fn context_reply(event: &str, contexts: &[Option<&str>]) -> Option<String> {
    let all: Vec<&str> = contexts.iter().flatten().copied().collect();
    (!all.is_empty()).then(|| {
        json!({
            "hookSpecificOutput": {
                "hookEventName": event,
                "additionalContext": all.join("\n\n")
            }
        })
        .to_string()
    })
}

/// `giverny hook`: the payload on stdin, the reply (if any) on stdout, exit 0
/// whatever happens.
pub fn main() -> i32 {
    let mut input = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    // The session's turn's end: held once per background command of its
    // own that its timeout moved there or that went quiet (giverny#450).
    if let Some(stop) = crate::strays::stop_payload(&input) {
        let feeds = crate::feed::feed_dir();
        let probe = crate::strays::System;
        if let Some(out) =
            crate::strays::stop(&stop, &feeds, manage::now_ms(), &probe, in_giverny_tab)
        {
            println!("{out}");
        }
        return 0;
    }
    // A subagent's finish: held once when a background command of its own
    // still runs (giverny#370).
    if let Some(stop) = crate::leftovers::stop_payload(&input) {
        let feeds = crate::feed::feed_dir();
        if let Some(out) =
            crate::leftovers::subagent_stop(&stop, &feeds, manage::now_ms(), in_giverny_tab)
        {
            println!("{out}");
        }
        return 0;
    }
    if let Some(prompt) = crate::manage_trigger::prompt_of(&input) {
        let feeds = crate::feed::feed_dir();
        // The person typed the skill's command: Claude Code expands it with
        // no `Skill` call, so the mark is made here (giverny#357).
        if crate::managers::typed_skill(&prompt)
            && let Ok(v) = serde_json::from_str::<Value>(&input)
        {
            crate::managers::mark_from(&feeds, &v);
        }
        let on = crate::names::is_on(&feeds);
        let name = on
            .then(|| serde_json::from_str::<Value>(&input).ok())
            .flatten()
            .and_then(|p| crate::names::first_prompt(&p, &feeds, on, tabs_own));
        let manage = crate::manage_trigger::asks_to_manage(&prompt)
            .then_some(crate::manage_trigger::CONTEXT);
        // What a finished subagent left running, if its parent has not
        // heard yet (giverny#370).
        let session = session_of(&input);
        let left = session
            .as_deref()
            .and_then(|s| crate::leftovers::deliver(&feeds, s));
        // Its own background commands that went quiet since its last turn
        // (giverny#450).
        let strays = session.as_deref().and_then(|s| {
            crate::strays::between(&feeds, s, manage::now_ms(), &crate::strays::System)
        });
        // What happened since its last turn, first (giverny#373).
        let away = session
            .as_deref()
            .and_then(|s| away::on_call(&feeds, s, manage::now_ms(), away::Call::Prompt));
        let contexts = [
            away.as_deref(),
            manage,
            name.as_deref(),
            left.as_deref(),
            strays.as_deref(),
        ];
        if let Some(out) = prompt_reply(&contexts) {
            println!("{out}");
        }
        return 0;
    }
    if let Some(start) = crate::names::start_payload(&input) {
        // A tab's claude is put under its cap before its first command,
        // not on the sampler's next pass (giverny#270).
        if start.get("agent_id").is_none() {
            let env_file = std::env::var_os("CLAUDE_ENV_FILE").map(PathBuf::from);
            crate::tab_cap::on_session_start(env_file.as_deref());
        }
        let feeds = crate::feed::feed_dir();
        let on = crate::names::is_on(&feeds);
        let name = crate::names::start_context(&start, &feeds, on, on && tabs_own());
        // A resume after a reboot or a relaunch hears of it here (giverny#373).
        let away = text(&start, "session_id")
            .filter(|_| start.get("agent_id").is_none())
            .and_then(|s| away::on_call(&feeds, &s, manage::now_ms(), away::Call::Start));
        // A manager's compacted session is told it is one
        // (giverny#448).
        let manager = crate::managers::start_context(&feeds, &start);
        if let Some(out) =
            context_reply("SessionStart", &[away.as_deref(), manager, name.as_deref()])
        {
            println!("{out}");
        }
        return 0;
    }
    // A session's own `Agent` call: maybe the nudge toward the manage
    // skill (giverny#358). A subagent's spawn goes on to the Bash check,
    // which has nothing to say about it.
    if let Some(spawn) = crate::manage_hint::spawn_of(&input) {
        if let Some(out) = crate::manage_hint::nudge(&spawn, &crate::feed::feed_dir()) {
            println!("{out}");
        }
        return 0;
    }
    if let Some(pre) = pre_tool(&input) {
        let capped = (pre.tool == "Bash")
            .then(|| guard(pre.command.as_deref().unwrap_or_default(), in_giverny_tab))
            .flatten();
        let out = capped.or_else(|| {
            let session = pre.session.as_deref()?;
            let w = crate::reach_guard::System::new(crate::feed::feed_dir());
            reach(&pre, &w, || w.me(session))
        });
        if let Some(out) = out {
            println!("{out}");
        }
        return 0;
    }
    if let Some(payload) = payload_of(&input) {
        let feeds = crate::feed::feed_dir();
        let now = manage::now_ms();
        // A subagent's call keeps its session's record and queues what it
        // saw for the parent; the parent's is told (giverny#373).
        let call = if payload.get("agent_id").is_some() {
            away::Call::Subagent
        } else {
            away::Call::Tool
        };
        let away = text(&payload, "session_id").and_then(|s| away::on_call(&feeds, &s, now, call));
        if let Some(out) = run_with(&payload, &feeds, now, in_giverny_tab(), away) {
            println!("{out}");
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed;

    #[test]
    fn a_prompt_reply_carries_every_context_given() {
        assert_eq!(prompt_reply(&[None, None]), None);
        let one: Value = serde_json::from_str(&prompt_reply(&[None, Some("b")]).unwrap()).unwrap();
        assert_eq!(
            one["hookSpecificOutput"]["hookEventName"],
            "UserPromptSubmit"
        );
        assert_eq!(one["hookSpecificOutput"]["additionalContext"], "b");
        let both: Value =
            serde_json::from_str(&prompt_reply(&[Some("a"), Some("b")]).unwrap()).unwrap();
        assert_eq!(both["hookSpecificOutput"]["additionalContext"], "a\n\nb");
    }

    const T0: u64 = 1_790_000_000_000;
    const MIN: u64 = 60_000;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("giverny-hook-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn doc(rows: Value) -> Value {
        json!({"version": 1, "session": "s1", "writer": manage::WRITER, "rows": rows})
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
    fn a_bash_command_that_leaves_its_cap_is_refused_in_a_tab() {
        let pre = |cmd: &str| {
            json!({"session_id": "s1", "hook_event_name": "PreToolUse", "tool_name": "Bash",
                   "tool_input": {"command": cmd, "description": "x"}})
            .to_string()
        };
        let cmd = pre_bash(&pre("systemd-run --user --scope make")).unwrap();
        assert_eq!(cmd, "systemd-run --user --scope make");
        let out = guard(&cmd, || true).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(
            v["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("giverny-manage run")
        );
        // Outside a tab there is no cap to leave.
        assert_eq!(guard(&cmd, || false), None);
        // A capped run passes, and the tab is not even asked.
        let ok = pre_bash(&pre("giverny-manage run t -- cargo build")).unwrap();
        assert_eq!(guard(&ok, || panic!("not asked")), None);
        // Another tool's PreToolUse: nothing to check; a PostToolUse: not ours.
        let read = json!({"hook_event_name": "PreToolUse", "tool_name": "Read",
                          "tool_input": {"file_path": "/x"}})
        .to_string();
        assert_eq!(pre_bash(&read).as_deref(), Some(""));
        assert_eq!(guard("", || true), None);
        assert_eq!(
            pre_bash(&spawn_input("s1", "a1", "systemd-run PreToolUse", true)),
            None
        );
    }

    /// giverny#415: the pre-tool hook refuses a reach into another
    /// session's processes, and asks who the caller is only when there is
    /// one.
    #[test]
    fn a_reach_into_another_sessions_work_is_denied_and_the_rest_passes() {
        use crate::reach_guard::tests::{me, world};
        let w = world();
        let pre = |tool: &str, input: Value| {
            pre_tool(
                &json!({"session_id": "my-session", "hook_event_name": "PreToolUse",
                        "tool_name": tool, "tool_input": input, "cwd": "/tmp"})
                .to_string(),
            )
            .unwrap()
        };
        let kill = pre("Bash", json!({"command": "kill -9 400"}));
        assert_eq!(kill.session.as_deref(), Some("my-session"));
        assert_eq!(kill.cwd.as_deref(), Some("/tmp"));
        let out = reach(&kill, &w, me).expect("denied");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        let why = v["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(why.contains("giverny-manage ask scale343"), "{why}");
        assert_eq!(
            reach(&pre("Bash", json!({"command": "kill 600"})), &w, me),
            None
        );
        let plain = pre("Bash", json!({"command": "cargo build"}));
        assert_eq!(reach(&plain, &w, || panic!("not asked")), None);
        let edit = pre("Edit", json!({"file_path": "/tmp/x.rs", "old_string": "a"}));
        assert_eq!(edit.file.as_deref(), Some("/tmp/x.rs"));
        assert_eq!(reach(&edit, &w, || panic!("not in a worktree")), None);
        let nb = pre("NotebookEdit", json!({"notebook_path": "/tmp/n.ipynb"}));
        assert_eq!(nb.file.as_deref(), Some("/tmp/n.ipynb"));
        let read = pre("Read", json!({"file_path": "/x"}));
        assert_eq!(reach(&read, &w, || panic!("not asked")), None);
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

    /// giverny#357: the session's own `Skill` call running the manage
    /// skill marks it a manager; any other skill, or a worker's call, marks
    /// nobody.
    #[test]
    fn the_manage_skill_call_marks_its_session_a_manager() {
        let dir = temp("mark");
        let skill = |session: &str, agent: Option<&str>, name: &str| {
            let agent = agent
                .map(|a| format!(r#""agent_id":"{a}","#))
                .unwrap_or_default();
            format!(
                r#"{{"session_id":"{session}",{agent}"hook_event_name":"PostToolUse","tool_name":"Skill","tool_input":{{"skill":"{name}","args":"task 12"}},"tool_response":{{"success":true}}}}"#
            )
        };
        let p = payload_of(&skill("s1", None, "giverny:manage")).unwrap();
        assert_eq!(p["skill"], "giverny:manage");
        run(&p, &dir, T0, true);
        assert!(crate::managers::is_manager(&dir, "s1", None));
        for (s, input) in [
            ("s2", skill("s2", None, "simplify")),
            ("s3", skill("s3", Some("w1"), "giverny:manage")),
        ] {
            run(&payload_of(&input).unwrap(), &dir, T0, true);
            assert!(!crate::managers::is_manager(&dir, s, None), "{input}");
        }
        let _ = std::fs::remove_dir_all(&dir);
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
        // Nothing would show it outside a tab, with no manager session.
        assert_eq!(run_in(&spawn_input("s1", "a5", "x", true), false), None);
        // The dispatcher's other calls read nothing and say nothing.
        assert_eq!(run(&json!({"session_id": "s1"}), &dir, T0, true), None);
        assert!(!feed::feed_path(&dir, "s1").exists(), "no feed made");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_manager_sessions_task_is_never_asked_about() {
        let dir = temp("managed");
        manage::write(
            &feed::feed_path(&dir, "s1"),
            &doc(json!([{"key": "acme#7", "stage": "running", "started": manage::stamp(T0)}])),
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
        // Outside a tab, with no manager session: silent, nothing made.
        let q = worker_payload(&dir, "s2", "w2", "x", T0);
        for k in 0..10 {
            assert_eq!(run(&q, &feeds, T0 + k * MIN, false), None);
        }
        assert!(!feed::feed_path(&feeds, "s2").exists(), "no feed made");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rows_chore_reaches_the_dispatcher_once_and_never_a_worker() {
        let dir = temp("chores");
        let f = feed::feed_path(&dir, "s1");
        manage::write(
            &f,
            &doc(
                json!([{"key": "demo#9", "stage": "running", "started": manage::stamp(T0),
                         "agent_id": "w1"}]),
            ),
        )
        .unwrap();
        let chore = manage_chores::Chore {
            key: "demo#9".into(),
            kind: manage_chores::Kind::Land,
            since: T0 + 10 * MIN,
            how: Some("finished".into()),
        };
        manage_chores::publish(&dir, "s1", std::slice::from_ref(&chore)).unwrap();
        // A worker's call under the same session hears nothing of it.
        let w = worker_payload(&dir, "s1", "w2", "other work", T0);
        let said = run(&w, &dir, T0 + 14 * MIN, false).map(|o| context(&o));
        assert!(!said.unwrap_or_default().contains("demo#9"));
        assert!(
            manage_chores::path(&dir, "s1").exists(),
            "kept for the dispatcher"
        );
        let me = json!({"session_id": "s1", "hook_event_name": "PostToolUse", "tool_name": "Bash"});
        let ctx = context(&run(&me, &dir, T0 + 14 * MIN, false).unwrap());
        assert_eq!(
            ctx,
            "Giverny: `demo#9`'s worker finished 4m ago: check its report and \
             `giverny-manage land demo#9`."
        );
        assert_eq!(run(&me, &dir, T0 + 15 * MIN, false), None, "told once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// giverny#370: what a finished subagent left running reaches its
    /// parent on the parent's own next tool call, once, and never a
    /// worker's call.
    #[test]
    fn a_leftover_reaches_the_parent_on_its_next_tool_call() {
        let dir = temp("leftover");
        let sub = dir.join("proj/s1/subagents");
        std::fs::create_dir_all(&sub).unwrap();
        let t = sub.join("agent-a1.jsonl");
        std::fs::write(
            &t,
            json!({"type": "user", "timestamp": "2026-10-13T09:47:31.823Z",
                   "message": {"content": [{"type": "tool_result",
                   "content": "Command running in background with ID: bgej3bpqh. …"}]}})
            .to_string()
                + "\n",
        )
        .unwrap();
        let stop = json!({"session_id": "s1", "agent_id": "a1",
            "hook_event_name": "SubagentStop", "agent_transcript_path": t.display().to_string(),
            "background_tasks": [
                {"id": "a1", "type": "subagent", "status": "running",
                 "description": "Start a sleeper"},
                {"id": "bgej3bpqh", "type": "shell", "status": "running",
                 "command": "sleep 371"}]})
        .to_string();
        let stop = crate::leftovers::stop_payload(&stop).unwrap();
        let now = T0;
        assert!(crate::leftovers::subagent_stop(&stop, &dir, now, || true).is_some());
        assert!(crate::leftovers::subagent_stop(&stop, &dir, now, || true).is_none());
        let w = worker_payload(&dir, "s1", "w2", "other work", T0);
        let said = run(&w, &dir, T0 + MIN, false).map(|o| context(&o));
        assert!(!said.unwrap_or_default().contains("sleep 371"));
        let me = json!({"session_id": "s1", "hook_event_name": "PostToolUse", "tool_name": "Read"});
        assert_eq!(
            context(&run(&me, &dir, T0 + MIN, false).unwrap()),
            "Giverny: your subagent \"Start a sleeper\" finished with `sleep 371` still \
             running in the background (task bgej3bpqh)."
        );
        assert_eq!(run(&me, &dir, T0 + 2 * MIN, false), None, "told once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// giverny#373: what happened while the session was away goes first
    /// in the dispatcher's reply, beside whatever else it is told.
    #[test]
    fn the_away_note_leads_the_dispatchers_reply() {
        let dir = temp("away");
        let me = json!({"session_id": "s1", "hook_event_name": "PostToolUse", "tool_name": "Read"});
        let away = Some("Giverny: the computer slept.".to_string());
        assert_eq!(
            context(&run_with(&me, &dir, T0, false, away.clone()).unwrap()),
            "Giverny: the computer slept."
        );
        let spawn = payload_of(&spawn_input("s1", "a1", "Classify chunk 0", true)).unwrap();
        let ctx = context(&run_with(&spawn, &dir, T0, true, away).unwrap());
        assert!(ctx.starts_with("Giverny: the computer slept.\n\n"), "{ctx}");
        assert!(ctx.contains("giverny-eta a1"), "{ctx}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_task_worker_re_estimates_its_row_and_the_hook_round_trips() {
        let dir = temp("task");
        let feeds = dir.join("feeds");
        std::fs::create_dir_all(&feeds).unwrap();
        let f = feed::feed_path(&feeds, "s1");
        let d = doc(json!([{"key": "demo#143", "stage": "running",
                            "started": manage::stamp(T0), "eta_s": 4500}]));
        manage::write(&f, &d).unwrap();
        let p = worker_payload(&dir, "s1", "abc", "demo#143: better estimates", T0);
        assert_eq!(run(&p, &feeds, T0 + MIN, false), None);
        // No history to tell, but the row is stamped so later calls read none.
        let raw: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
        assert!(raw["rows"][0].get("track_told").is_some(), "{raw}");
        let ctx = context(&run(&p, &feeds, T0 + 6 * MIN, false).unwrap());
        assert!(ctx.contains("giverny-manage eta demo#143"), "{ctx}");
        assert_eq!(run(&p, &feeds, T0 + 7 * MIN, false), None, "asked once");
        let back = feed::read(&f).unwrap();
        assert_eq!(back.rows.len(), 1);
        assert_eq!(back.rows[0].agent_id.as_deref(), Some("abc"), "stamped");
        // Its estimate is the row's: no agent ETA is asked for or made.
        assert!(agent_eta::read(&feeds, "s1").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

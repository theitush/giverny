//! The management panel's live rows, per tab.
//!
//! This keeps one [`Tracker`] per tab, and each reads its session's workers
//! off the disk once a second ([`Tracker::refresh`]): Running while one has
//! written since the claude now running the session started and has not
//! landed, Done once it has. Which claude that is, and when it started, the
//! session registry says ([`ManagementLive::set_hosts`], from
//! [`ClaudeWatch`]'s scan). Claude Code before 2.1.323 also ran
//! `giverny relay --subagent-line` with its live worker list, forwarded with
//! the tab it ran in; [`ClaudeWatch`] still hands any such tick here, and it
//! folds into the same rows ([`ManagementLive::apply_live`]). 2.1.323
//! removed that setting (giverny#523), so a tab gets its table from the
//! hooks that say its session has a worker ([`ManagementLive::worker_seen`]).
//! The pane reads them through [`ManagementLive::tracker`]; nothing here
//! draws anything.
//!
//! Rows are kept until the tab's conversation is cleared (`/clear`, which
//! Claude Code reports as `SessionStart` with `source: "clear"`), a fresh
//! `claude` starts in it (`source: "startup"`), or the tab is closed, and they are saved to disk so a Giverny restart keeps the Done
//! rows. They are part of the tab's session, though: after a restart they are
//! not shown until that session is back up — its `SessionStart`, or a tick
//! from it — and they go again when it ends ([`ManagementLive::shown`]).
//!
//! [`ClaudeWatch`]: crate::claude_watch::ClaudeWatch

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use giverny_claude::subagents::{
    LiveSnapshot, Stage, Tracker, conversation_root, session_transcript,
};
use giverny_core::tabs::TabId;
use serde::{Deserialize, Serialize};

/// How often trackers re-read the transcripts (activity, Done detection).
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
/// How soon a change is written to disk, at the latest.
const SAVE_INTERVAL: Duration = Duration::from_secs(2);

pub struct ManagementLive {
    trackers: HashMap<TabId, Tracker>,
    /// Tabs whose Claude session has been heard from in this run and not
    /// ended since: the ones whose rows are shown. Not saved — a restart
    /// starts every tab's session over.
    up: HashSet<TabId>,
    /// Tabs whose session started with no table and no worker yet: the
    /// session and its account. Such a start makes no table (a fresh
    /// `claude` in every tab would rescan for workers each second), until
    /// the session is marked a manager (giverny#357): then it gets one, so
    /// its pane shows its Next up before its first spawn.
    waiting: HashMap<TabId, (String, Option<PathBuf>)>,
    /// When the claude now running each live session started (its
    /// registry `startedAt`), by session id: what tells a worker that runs
    /// from one killed with an earlier claude of its conversation.
    hosts: HashMap<String, u64>,
    /// Where the trackers are saved; `None` keeps them in memory only.
    path: Option<PathBuf>,
    dirty: bool,
    last_refresh: Instant,
    last_save: Instant,
}

/// The on-disk form: a list rather than a map, since JSON keys are strings.
#[derive(Serialize, Deserialize, Default)]
struct Saved {
    tabs: Vec<(u64, Tracker)>,
}

impl ManagementLive {
    /// Trackers restored from `path` (or none), saved back there.
    pub fn load(path: PathBuf) -> ManagementLive {
        let trackers = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Saved>(&b).ok())
            .map(|s| s.tabs.into_iter().map(|(id, t)| (TabId(id), t)).collect())
            .unwrap_or_default();
        ManagementLive {
            trackers,
            up: HashSet::new(),
            waiting: HashMap::new(),
            hosts: HashMap::new(),
            path: Some(path),
            dirty: false,
            last_refresh: Instant::now() - REFRESH_INTERVAL,
            last_save: Instant::now(),
        }
    }

    /// Trackers that live in memory only (tests).
    #[cfg(test)]
    pub fn in_memory() -> ManagementLive {
        ManagementLive {
            trackers: HashMap::new(),
            up: HashSet::new(),
            waiting: HashMap::new(),
            hosts: HashMap::new(),
            path: None,
            dirty: false,
            last_refresh: Instant::now() - REFRESH_INTERVAL,
            last_save: Instant::now(),
        }
    }

    /// The subagents of `tab`'s Claude session, Running and Done — what the
    /// management panel draws. `None` until the tab's Claude has spawned a
    /// worker, resumed a conversation, or been marked a manager.
    ///
    /// [`Tracker::rows`] is the table; [`Tracker::session_id`] and
    /// [`Tracker::aliases`] name the feed file to merge with it.
    pub fn tracker(&self, tab: TabId) -> Option<&Tracker> {
        self.trackers.get(&tab)
    }

    /// [`ManagementLive::tracker`], but only while `tab`'s session is up: what
    /// the pane draws. Rows restored from disk wait for the session they
    /// belong to, so the pane does not show before it.
    pub fn shown(&self, tab: TabId) -> Option<&Tracker> {
        self.tracker(tab).filter(|_| self.up.contains(&tab))
    }

    /// Every tab whose session is up, with its tracker: what
    /// [`ManagementLive::shown`] would give for each.
    pub fn all_shown(&self) -> impl Iterator<Item = (TabId, &Tracker)> {
        self.trackers
            .iter()
            .filter(|(id, _)| self.up.contains(id))
            .map(|(id, t)| (*id, t))
    }

    /// `tab`'s session ended (`SessionEnd`): its pane goes with it; the rows
    /// are kept for the next start.
    pub fn session_ended(&mut self, tab: TabId) {
        self.up.remove(&tab);
        self.waiting.remove(&tab);
    }

    /// `tab` was stopped from the rail (giverny#474): its session is down,
    /// as [`ManagementLive::session_ended`] has it, and the workers that ran
    /// in it ended with it — its Running rows are Done, stopped. Only this
    /// tab's rows: no other manager's are looked at.
    pub fn stopped(&mut self, tab: TabId) {
        self.session_ended(tab);
        if let Some(t) = self.trackers.get_mut(&tab)
            && t.end_running(now_ms()) > 0
        {
            self.dirty = true;
        }
    }

    /// Running rows `tab`'s pane shows now: what a stop would end.
    pub fn running_workers(&self, tab: TabId) -> usize {
        self.shown(tab)
            .map_or(0, |t| t.rows().iter().filter(|r| r.running()).count())
    }

    /// The same, for the pane's own edits (a manual clear). Marks the store
    /// for saving.
    pub fn tracker_mut(&mut self, tab: TabId) -> Option<&mut Tracker> {
        let t = self.trackers.get_mut(&tab)?;
        self.dirty = true;
        Some(t)
    }

    /// One relayed tick: Claude Code's `subagentStatusLine` stdin as it
    /// arrived (`event` of the relay message), for `tab`, whose session runs
    /// under `config_dir` when known.
    pub fn apply_live(
        &mut self,
        tab: TabId,
        config_dir: Option<PathBuf>,
        event: &serde_json::Value,
    ) {
        let snap = LiveSnapshot::from_value(event);
        self.up.insert(tab);
        self.waiting.remove(&tab);
        let tracker = self
            .trackers
            .entry(tab)
            .or_insert_with(|| Tracker::new(None));
        // The session says which account it is on (and a reused tab may have
        // moved); a session that names none is on Claude Code's default.
        if config_dir.is_some() {
            tracker.config_dir = config_dir;
        } else if tracker.config_dir.is_none() {
            tracker.config_dir = default_config_dir();
        }
        // A tick from another conversation is a start this tab never heard
        // of — a `/clear` whose hook went astray (giverny#242): the table is
        // that conversation's, as [`ManagementLive::session_started`] would make it.
        if let Some(sid) = snap.session_id.as_deref()
            && tracker.continues(sid) == Some(false)
        {
            *tracker = Tracker::new(tracker.config_dir.clone());
        }
        tracker.apply_live(&snap, now_ms());
        self.dirty = true;
    }

    /// A hook from `tab`'s session names one of its workers: a tool call
    /// the worker made (`agent`, its id), or the session's own `Agent` call
    /// that launched one (its id when the result gave it). No live list says
    /// a worker runs since Claude Code 2.1.323 (giverny#523), so this is
    /// what gives a tab with no table one when the session's first worker
    /// starts, as the first live tick did: bound to the session, shown, and
    /// read from the disk on the next tick rather than within the second.
    /// A worker the table already has Running changes nothing.
    pub fn worker_seen(
        &mut self,
        tab: TabId,
        session_id: Option<&str>,
        config_dir: Option<PathBuf>,
        agent: Option<&str>,
    ) {
        self.up.insert(tab);
        let waiting = self.waiting.remove(&tab);
        let session = session_id
            .map(str::to_string)
            .or_else(|| waiting.as_ref().map(|(sid, _)| sid.clone()));
        let Some(tracker) = self.trackers.get_mut(&tab) else {
            let config = config_dir
                .or_else(|| waiting.and_then(|(_, c)| c))
                .or_else(default_config_dir);
            let mut fresh = Tracker::new(config);
            if let Some(sid) = &session {
                fresh.set_session(sid);
            }
            self.trackers.insert(tab, fresh);
            self.dirty = true;
            self.refresh_soon();
            return;
        };
        if config_dir.is_some() && tracker.config_dir != config_dir {
            tracker.config_dir = config_dir;
        }
        if let Some(sid) = session.as_deref() {
            let known = tracker.session_id.as_deref() == Some(sid)
                || tracker.aliases.iter().any(|a| a == sid);
            if !known {
                // Another conversation: a start this tab never heard of
                // (giverny#242), as a tick from it would say.
                if tracker.continues(sid) == Some(false) {
                    *tracker = Tracker::new(tracker.config_dir.clone());
                }
                tracker.set_session(sid);
                self.dirty = true;
                self.refresh_soon();
                return;
            }
        }
        let running = agent.is_some_and(|id| tracker.get(id).is_some_and(|r| r.running()));
        if !running {
            self.refresh_soon();
        }
    }

    /// Read the disk on the next tick.
    fn refresh_soon(&mut self) {
        self.last_refresh = Instant::now() - REFRESH_INTERVAL;
    }

    /// The live sessions and when the claude running each started, from the
    /// session registry: `(session id, startedAt)`. A session two claudes
    /// hold is taken at the earlier.
    pub fn set_hosts<'a>(&mut self, live: impl IntoIterator<Item = (&'a str, u64)>) {
        self.hosts.clear();
        for (sid, started) in live {
            if started == 0 {
                continue;
            }
            let at = self.hosts.entry(sid.to_string()).or_insert(started);
            *at = (*at).min(started);
        }
    }

    /// When the claude running `tracker`'s session started, under any of
    /// its ids: `None` when no live claude holds it.
    fn host_of(&self, tracker: &Tracker) -> Option<u64> {
        tracker
            .session_id
            .iter()
            .chain(tracker.aliases.iter())
            .filter_map(|sid| self.hosts.get(sid).copied())
            .min()
    }

    /// A `SessionStart` in `tab`. `/clear` (`source: "clear"`) starts the
    /// table over, bound to the new session — its old ids are not aliases,
    /// or their finished workers would come back as Done rows on the next
    /// refresh. So does a fresh `claude` (`source: "startup"`), whose
    /// conversation has no transcript yet to compare roots with,
    /// and a start in *another* conversation — `/resume` of
    /// an older one, whose transcript has a root of its own
    /// ([`Tracker::continues`]): the table is that conversation's, rebuilt
    /// from its own `subagents/` on the next refresh, and nothing of what the
    /// tab ran before. Any other start — the same conversation
    /// re-id'd (`compact`, or `fork`/`resume` for the agents view's switch
    /// and the move into a background host), or one whose root
    /// cannot be read yet — keeps the rows and records the new id beside the
    /// old.
    ///
    /// `startup` is safe to take as new because Claude Code (2.1.283) raises
    /// it only where no conversation is carried in: a launch without
    /// `--resume`/`--continue`, and the claim of a spare that has not run a
    /// turn. Every path that loads an existing conversation raises `resume`,
    /// or `fork` when the id changes — the switch recorded in real
    /// transcripts as `SessionStart:fork`.
    ///
    /// A tab with no table yet gets one for any start that carries a
    /// conversation in — a `resume`, a `fork` — bound to it, so its finished
    /// workers show from disk on the next refresh instead of waiting for a
    /// live one (giverny#242). `config_dir` is the account it runs under.
    pub fn session_started(
        &mut self,
        tab: TabId,
        source: Option<&str>,
        session_id: Option<&str>,
        config_dir: Option<PathBuf>,
    ) {
        self.up.insert(tab);
        self.waiting.remove(&tab);
        let Some(tracker) = self.trackers.get_mut(&tab) else {
            if let Some(sid) = session_id {
                if matches!(source, Some("clear" | "startup")) {
                    self.waiting.insert(tab, (sid.to_string(), config_dir));
                } else {
                    let mut fresh = Tracker::new(config_dir.or_else(default_config_dir));
                    fresh.set_session(sid);
                    self.trackers.insert(tab, fresh);
                    self.dirty = true;
                }
            }
            return;
        };
        let elsewhere = session_id.is_some_and(|sid| tracker.continues(sid) == Some(false));
        if matches!(source, Some("clear" | "startup")) || elsewhere {
            let mut fresh = Tracker::new(tracker.config_dir.clone());
            if let Some(sid) = session_id {
                fresh.set_session(sid);
            }
            *tracker = fresh;
        } else if let Some(sid) = session_id {
            tracker.set_session(sid);
        }
        self.dirty = true;
    }

    /// `tab` shows a background job now holding conversation `session` —
    /// what the job's own `state.json` says, needing no hook — resumed from
    /// `origin`, whose earlier workers are the job's history until the job
    /// starts a conversation of its own. A conversation the table's is not (a
    /// `/clear` whose hook never reached the tab, a switch to another job)
    /// starts the table over, bound to it; the same one re-id'd is recorded
    /// beside the old id. A tab with no table gets one (giverny#242).
    ///
    /// The job runs, so its session is up: the pane shows from now, not
    /// from the first hook or status tick that reaches the tab — a job
    /// shown by `claude attach` sends no `SessionStart`, and a manager
    /// that has only planned has no workers to tick (giverny#244).
    pub fn job_holds(
        &mut self,
        tab: TabId,
        session: &str,
        origin: Option<&str>,
        config_dir: Option<PathBuf>,
    ) {
        self.up.insert(tab);
        let config = self
            .trackers
            .get(&tab)
            .and_then(|t| t.config_dir.clone())
            .or(config_dir)
            .or_else(default_config_dir);
        // The origin is this conversation's past only while the session has
        // no root of its own, or the same root: a `/clear` in the job starts
        // one that owes the origin nothing.
        let origin = origin.filter(|o| {
            *o != session
                && config.as_deref().is_none_or(|c| {
                    let root =
                        |sid: &str| session_transcript(c, sid).and_then(|p| conversation_root(&p));
                    root(session).is_none_or(|r| root(o) == Some(r))
                })
        });
        let fresh = |config: Option<PathBuf>| {
            let mut t = Tracker::new(config);
            if let Some(o) = origin {
                t.set_session(o);
            }
            t.set_session(session);
            t
        };
        let Some(tracker) = self.trackers.get_mut(&tab) else {
            self.trackers.insert(tab, fresh(config));
            self.dirty = true;
            return;
        };
        let probe = tracker
            .continues(session)
            .or_else(|| origin.and_then(|o| tracker.continues(o)));
        match probe {
            Some(false) => *tracker = fresh(config),
            Some(true) => {
                let known = |t: &Tracker, sid: &str| {
                    t.session_id.as_deref() == Some(sid) || t.aliases.iter().any(|a| a == sid)
                };
                if tracker.session_id.as_deref() == Some(session)
                    && origin.is_none_or(|o| known(tracker, o))
                {
                    return;
                }
                if let Some(o) = origin
                    && !known(tracker, o)
                {
                    tracker.set_session(o);
                }
                tracker.set_session(session);
            }
            None => return,
        }
        self.dirty = true;
    }

    /// `giverny manage clear-done` in `tab`: drop its Done rows (and hide the
    /// feed's that landed by then), keeping what runs. `at_ms` is when the
    /// command ran, else now.
    pub fn clear_done(&mut self, tab: TabId, at_ms: Option<u64>) {
        let now = now_ms();
        let Some(tracker) = self.trackers.get_mut(&tab) else {
            return;
        };
        let gone = tracker.clear_done(at_ms.unwrap_or(now).min(now));
        tracing::info!("tab {tab:?}: {gone} Done row(s) cleared from the management panel");
        self.dirty = true;
    }

    /// Housekeeping, called every frame: re-read transcripts about once a
    /// second, drop trackers of tabs that no longer exist, save when changed.
    pub fn tick(&mut self, tab_exists: impl Fn(TabId) -> bool) {
        let before = self.trackers.len();
        self.trackers.retain(|id, _| tab_exists(*id));
        self.up.retain(|id| tab_exists(*id));
        self.waiting.retain(|id, _| tab_exists(*id));
        self.dirty |= self.trackers.len() != before;

        if self.last_refresh.elapsed() >= REFRESH_INTERVAL {
            self.last_refresh = Instant::now();
            self.adopt_managers(&giverny_claude::feed::feed_dir());
            let now = now_ms();
            let hosts: Vec<(TabId, Option<u64>)> = self
                .trackers
                .iter()
                .map(|(id, t)| (*id, self.host_of(t)))
                .collect();
            for (id, host) in hosts {
                let Some(tracker) = self.trackers.get_mut(&id) else {
                    continue;
                };
                let before = signature(tracker);
                tracker.refresh(host, now);
                self.dirty |= signature(tracker) != before;
            }
        }
        if self.dirty && self.last_save.elapsed() >= SAVE_INTERVAL {
            self.save();
        }
    }

    /// Give each [`ManagementLive::waiting`] tab whose session is marked a
    /// manager in `feeds` ([`giverny_claude::managers`]) its table, bound to
    /// that session: a `stat` per waiting tab.
    fn adopt_managers(&mut self, feeds: &Path) {
        let marked: Vec<TabId> = self
            .waiting
            .iter()
            .filter(|(_, (sid, _))| giverny_claude::managers::is_marked(feeds, sid, &[], || None))
            .map(|(tab, _)| *tab)
            .collect();
        for tab in marked {
            let Some((sid, config)) = self.waiting.remove(&tab) else {
                continue;
            };
            let mut fresh = Tracker::new(config.or_else(default_config_dir));
            fresh.set_session(&sid);
            self.trackers.insert(tab, fresh);
            self.dirty = true;
        }
    }

    /// Write the trackers out now.
    pub fn save(&mut self) {
        self.last_save = Instant::now();
        self.dirty = false;
        let Some(path) = &self.path else { return };
        let saved = Saved {
            tabs: self
                .trackers
                .iter()
                .filter(|(_, t)| t.worth_saving())
                .map(|(id, t)| (id.0, t.clone()))
                .collect(),
        };
        if let Err(err) = write_atomic(path, &saved) {
            tracing::warn!("management panel rows not saved: {err:#}");
        }
    }
}

/// What a refresh can change that is worth saving: a corrected token count
/// and a stop opening or closing among them, so a
/// restart comes back to them.
type Signature = (
    String,
    Stage,
    Option<u64>,
    Option<u64>,
    Option<(u64, Option<u64>)>,
);

fn signature(t: &Tracker) -> Vec<Signature> {
    t.rows()
        .iter()
        .map(|r| {
            (
                r.id.clone(),
                r.stage,
                r.ended_ms,
                r.tokens,
                r.stops.last().map(|s| (s.from_ms, s.to_ms)),
            )
        })
        .collect()
}

/// The account a session is on when it names none: Claude Code's default.
fn default_config_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn write_atomic(path: &Path, saved: &Saved) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(saved)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAB: TabId = TabId(7);

    fn tick_json(session: &str, ids: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": giverny_claude::hooks::SUBAGENT_LINE_EVENT,
            "session_id": session,
            "tasks": ids.iter().map(|id| serde_json::json!({
                "id": id, "type": "local_agent", "status": "running",
                "description": "work", "startTime": 1_790_000_000_000u64,
            })).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn a_worker_hook_gives_its_tab_a_table_that_finds_it_on_disk() {
        // Claude Code 2.1.323 and on: no tick ever comes. A fresh session
        // waits with no table; its first worker's hook makes one, and the
        // disk has the worker Running on the next tick.
        let config = std::env::temp_dir().join(format!("giverny-ml-523-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&config);
        let subs = config
            .join("projects")
            .join("-w")
            .join("s1")
            .join("subagents");
        std::fs::create_dir_all(&subs).unwrap();
        std::fs::write(
            subs.join("agent-a1.meta.json"),
            r#"{"description":"pics","spawnDepth":1}"#,
        )
        .unwrap();
        std::fs::write(
            subs.join("agent-a1.jsonl"),
            "{\"type\":\"user\",\"timestamp\":\"2026-11-15T17:12:05Z\"}\n",
        )
        .unwrap();
        let mut live = ManagementLive::in_memory();
        live.session_started(TAB, Some("startup"), Some("s1"), Some(config.clone()));
        assert!(live.tracker(TAB).is_none(), "a fresh session waits");
        live.worker_seen(TAB, Some("s1"), None, Some("a1"));
        let t = live.shown(TAB).expect("a table, shown");
        assert_eq!(t.session_id.as_deref(), Some("s1"));
        assert_eq!(t.config_dir.as_deref(), Some(config.as_path()));

        // No claude known for the session: nothing runs yet.
        live.tick(|_| true);
        assert!(live.tracker(TAB).unwrap().get("a1").is_none());
        // The registry has its claude, started before the worker wrote.
        live.set_hosts([("s1", now_ms() - 600_000), ("s1", now_ms() - 900_000)]);
        live.worker_seen(TAB, Some("s1"), None, Some("a1"));
        live.tick(|_| true);
        let r = live.tracker(TAB).unwrap().get("a1").expect("found on disk");
        assert!(r.running());
        assert_eq!(r.description.as_deref(), Some("pics"));
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn a_tick_becomes_rows_on_its_tab() {
        let mut live = ManagementLive::in_memory();
        assert!(live.tracker(TAB).is_none(), "no tracker before any worker");
        live.apply_live(
            TAB,
            Some("/nowhere".into()),
            &tick_json("s1", &["a1", "a2"]),
        );
        let t = live.tracker(TAB).unwrap();
        assert_eq!(t.session_id.as_deref(), Some("s1"));
        assert_eq!(t.rows().len(), 2);
        assert!(t.rows().iter().all(|r| r.running()));
        assert!(live.tracker(TabId(8)).is_none(), "other tabs untouched");

        // The next tick without a2: it is Done, not gone.
        live.apply_live(TAB, None, &tick_json("s1", &["a1"]));
        let t = live.tracker(TAB).unwrap();
        assert_eq!(t.rows().len(), 2);
        assert!(!t.get("a2").unwrap().running());
    }
}

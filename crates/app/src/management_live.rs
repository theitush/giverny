//! The management panel's live rows, per tab: where relayed `subagentStatusLine`
//! ticks land.
//!
//! `giverny relay --subagent-line` forwards Claude Code's live worker list
//! with the tab it ran in (`GIVERNY_TAB_ID`, which reaches that command:
//! verified against 2.1.280). [`ClaudeWatch`] hands each one here,
//! and this keeps one [`Tracker`] per tab — Running rows from the ticks, Done
//! rows from the transcripts once a worker leaves the list. The pane reads
//! them through [`ManagementLive::tracker`]; nothing here draws anything.
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
            path: None,
            dirty: false,
            last_refresh: Instant::now() - REFRESH_INTERVAL,
            last_save: Instant::now(),
        }
    }

    /// The subagents of `tab`'s Claude session, Running and Done — what the
    /// management panel draws. `None` until the tab's Claude has spawned a worker.
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

    /// `tab`'s session ended (`SessionEnd`): its pane goes with it; the rows
    /// are kept for the next start.
    pub fn session_ended(&mut self, tab: TabId) {
        self.up.remove(&tab);
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
        let Some(tracker) = self.trackers.get_mut(&tab) else {
            if let Some(sid) = session_id
                && !matches!(source, Some("clear" | "startup"))
            {
                let mut fresh = Tracker::new(config_dir.or_else(default_config_dir));
                fresh.set_session(sid);
                self.trackers.insert(tab, fresh);
                self.dirty = true;
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
        self.dirty |= self.trackers.len() != before;

        if self.last_refresh.elapsed() >= REFRESH_INTERVAL {
            self.last_refresh = Instant::now();
            for tracker in self.trackers.values_mut() {
                let before = signature(tracker);
                tracker.refresh();
                self.dirty |= signature(tracker) != before;
            }
        }
        if self.dirty && self.last_save.elapsed() >= SAVE_INTERVAL {
            self.save();
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

    /// A directory of the test's own: tests run in parallel, and one that
    /// removed another's directory mid-save made the restart tests flaky.
    fn scratch_dir(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("giverny-agents-live-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
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

    #[test]
    fn clear_starts_over_and_other_starts_keep_rows() {
        let mut live = ManagementLive::in_memory();
        live.apply_live(TAB, Some("/nowhere".into()), &tick_json("s1", &["a1"]));

        live.session_started(TAB, Some("resume"), Some("s2"), None);
        let t = live.tracker(TAB).unwrap();
        assert_eq!(t.rows().len(), 1, "a resume keeps the rows");
        assert_eq!(t.aliases, vec!["s1".to_string()]);

        live.session_started(TAB, Some("clear"), Some("s3"), None);
        let t = live.tracker(TAB).unwrap();
        assert!(t.is_empty(), "/clear empties the table");
        assert_eq!(t.session_id.as_deref(), Some("s3"));
        assert!(t.aliases.is_empty(), "and forgets the old ids");
        assert_eq!(t.config_dir.as_deref(), Some(Path::new("/nowhere")));

        // A session start in a tab with no workers creates nothing.
        live.session_started(TabId(9), Some("clear"), Some("x"), None);
        assert!(live.tracker(TabId(9)).is_none());
        // A resume brings a conversation in: its table is made, so its
        // finished workers show from disk (giverny#242).
        live.session_started(TabId(9), Some("resume"), Some("y"), Some("/c".into()));
        let t = live.tracker(TabId(9)).expect("a resume makes the table");
        assert_eq!(t.session_id.as_deref(), Some("y"));
        assert_eq!(t.config_dir.as_deref(), Some(Path::new("/c")));
    }

    /// Quitting claude and starting a fresh one in the tab
    /// empties the pane, though the new transcript is not on disk yet; a
    /// re-id of the same conversation (`fork`, `resume`, `compact`, no root
    /// to compare) keeps the rows.
    #[test]
    fn a_fresh_claude_starts_over_and_a_re_id_does_not() {
        let mut live = ManagementLive::in_memory();
        live.apply_live(TAB, Some("/nowhere".into()), &tick_json("s1", &["a1"]));
        live.apply_live(TAB, None, &tick_json("s1", &[]));
        assert_eq!(live.tracker(TAB).unwrap().rows().len(), 1, "a Done row");

        for (source, sid) in [("fork", "s2"), ("resume", "s3"), ("compact", "s4")] {
            live.session_started(TAB, Some(source), Some(sid), None);
            let t = live.tracker(TAB).unwrap();
            assert_eq!(t.rows().len(), 1, "{source} keeps the rows");
            assert_eq!(t.session_id.as_deref(), Some(sid));
        }
        assert_eq!(live.tracker(TAB).unwrap().aliases, ["s1", "s2", "s3"]);

        live.session_ended(TAB);
        live.session_started(TAB, Some("startup"), Some("n1"), None);
        let t = live.shown(TAB).expect("the new session is up");
        assert!(t.is_empty(), "a fresh claude shows an empty pane");
        assert_eq!(t.session_id.as_deref(), Some("n1"));
        assert!(t.aliases.is_empty(), "and none of the old ids");
        assert_eq!(t.config_dir.as_deref(), Some(Path::new("/nowhere")));
    }

    /// A `/clear` whose `SessionStart` never reached the tab — a session
    /// the background daemon hosts, before its hooks found their tab
    /// (giverny#242) — still empties the pane at the new conversation's
    /// first tick; a tick from the same conversation re-id'd keeps the rows.
    #[test]
    fn a_tick_from_another_conversation_starts_over() {
        let config =
            std::env::temp_dir().join(format!("giverny-agents-live-242-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&config);
        let proj = config.join("projects/-w");
        std::fs::create_dir_all(&proj).unwrap();
        let transcript = |sid: &str, root: &str| {
            let line = serde_json::json!({"type": "user", "parentUuid": null, "uuid": root});
            std::fs::write(proj.join(format!("{sid}.jsonl")), format!("{line}\n")).unwrap();
        };
        transcript("old", "root-1");
        transcript("forked", "root-1");
        transcript("cleared", "root-2");

        let mut live = ManagementLive::in_memory();
        live.apply_live(TAB, Some(config.clone()), &tick_json("old", &["a1"]));
        live.apply_live(TAB, None, &tick_json("forked", &[]));
        let t = live.tracker(TAB).unwrap();
        assert_eq!(
            t.rows().len(),
            1,
            "the same conversation keeps its Done row"
        );
        assert_eq!(t.aliases, ["old".to_string()]);

        live.apply_live(TAB, None, &tick_json("cleared", &["b1"]));
        let t = live.tracker(TAB).unwrap();
        let ids: Vec<&str> = t.rows().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["b1"], "only the cleared conversation's worker");
        assert_eq!(t.session_id.as_deref(), Some("cleared"));
        assert!(t.aliases.is_empty());
        let _ = std::fs::remove_dir_all(&config);
    }

    /// What a background job's `state.json` says it holds now brings the
    /// tab's table to that conversation, with no hook at all.
    #[test]
    fn a_parked_jobs_conversation_is_the_tables() {
        let config =
            std::env::temp_dir().join(format!("giverny-agents-live-242b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&config);
        let proj = config.join("projects/-w");
        std::fs::create_dir_all(&proj).unwrap();
        for (sid, root) in [("old", "r1"), ("forked", "r1"), ("cleared", "r2")] {
            let line = serde_json::json!({"type": "user", "parentUuid": null, "uuid": root});
            std::fs::write(proj.join(format!("{sid}.jsonl")), format!("{line}\n")).unwrap();
        }
        let mut live = ManagementLive::in_memory();
        live.apply_live(TAB, Some(config.clone()), &tick_json("old", &["a1"]));

        live.job_holds(TAB, "forked", None, None);
        let t = live.tracker(TAB).unwrap();
        assert_eq!(t.rows().len(), 1, "the same conversation keeps its rows");
        assert_eq!(t.session_id.as_deref(), Some("forked"));

        live.job_holds(TAB, "cleared", None, None);
        let t = live.tracker(TAB).unwrap();
        assert!(t.is_empty(), "a cleared conversation starts over");
        assert_eq!(t.session_id.as_deref(), Some("cleared"));
        assert!(t.aliases.is_empty());

        // A tab with no table gets one, for the job's history to show,
        // and its pane shows at once: the job is up (giverny#244).
        live.job_holds(TabId(9), "cleared", None, Some(config.clone()));
        assert_eq!(
            live.shown(TabId(9)).unwrap().session_id.as_deref(),
            Some("cleared")
        );

        // A job resumed from "old" with no transcript of its own yet: its
        // past is old's, so old's workers are its rows; a cleared job's
        // origin is no part of it.
        live.job_holds(TAB, "job-new", Some("old"), None);
        let t = live.tracker(TAB).unwrap();
        assert_eq!(t.session_id.as_deref(), Some("job-new"));
        assert_eq!(t.aliases, ["old".to_string()], "old's history comes along");
        live.job_holds(TAB, "cleared", Some("old"), None);
        let t = live.tracker(TAB).unwrap();
        assert_eq!(t.session_id.as_deref(), Some("cleared"));
        assert!(t.aliases.is_empty(), "a cleared job owes old nothing");
        let _ = std::fs::remove_dir_all(&config);
    }

    /// A tab resumed into A, then B, then A again shows each
    /// conversation's own finished workers, and nothing of the others'.
    #[test]
    fn a_resume_into_another_conversation_shows_only_its_rows() {
        let config =
            std::env::temp_dir().join(format!("giverny-agents-live-112-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&config);
        let proj = config.join("projects/-w");
        let finished = |sid: &str, root: &str, id: &str| {
            let subs = proj.join(sid).join("subagents");
            std::fs::create_dir_all(&subs).unwrap();
            let note = format!(
                "<task-notification>\n<task-id>{id}</task-id>\n<status>completed</status>\n</task-notification>"
            );
            let lines = [
                serde_json::json!({"type": "user", "parentUuid": null, "uuid": root}),
                serde_json::json!({"type": "user", "parentUuid": root, "uuid": format!("{root}-n"),
                                   "timestamp": "2026-09-27T10:00:00Z",
                                   "message": {"role": "user", "content": note}}),
            ];
            let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
            std::fs::write(proj.join(format!("{sid}.jsonl")), text).unwrap();
            std::fs::write(
                subs.join(format!("agent-{id}.jsonl")),
                "{\"type\":\"assistant\",\"timestamp\":\"2026-09-27T09:59:00Z\"}\n",
            )
            .unwrap();
        };
        finished("A", "root-a", "wa");
        finished("B", "root-b", "wb");
        let ids = |live: &ManagementLive| -> Vec<String> {
            let mut v: Vec<String> = live
                .tracker(TAB)
                .unwrap()
                .rows()
                .iter()
                .map(|r| r.id.clone())
                .collect();
            v.sort();
            v
        };
        let mut live = ManagementLive::in_memory();
        live.apply_live(TAB, Some(config.clone()), &tick_json("A", &["ra"]));
        live.tick(|_| true);
        assert_eq!(ids(&live), ["ra", "wa"]);

        live.session_started(TAB, Some("resume"), Some("B"), None);
        live.last_refresh -= REFRESH_INTERVAL;
        live.tick(|_| true);
        assert_eq!(ids(&live), ["wb"], "B's own history, nothing of A's");
        assert!(live.tracker(TAB).unwrap().aliases.is_empty());

        live.session_started(TAB, Some("resume"), Some("A"), None);
        live.last_refresh -= REFRESH_INTERVAL;
        live.tick(|_| true);
        assert_eq!(ids(&live), ["wa"], "back in A: A's again");

        // A session with no root of its own yet (the agents view's switch)
        // is the same session: the rows stay and A becomes an alias.
        std::fs::create_dir_all(proj.join("A2")).unwrap();
        std::fs::write(proj.join("A2.jsonl"), "{\"type\":\"mode\"}\n").unwrap();
        live.session_started(TAB, Some("resume"), Some("A2"), None);
        assert_eq!(ids(&live), ["wa"]);
        assert_eq!(live.tracker(TAB).unwrap().aliases, ["A".to_string()]);

        // A plain `claude` (startup, nothing on disk yet) is
        // empty; `/resume` from there back into A brings A's rows back
        // from disk. Claude Code sends startup, then resume, in that order.
        live.session_ended(TAB);
        live.session_started(TAB, Some("startup"), Some("N"), None);
        assert!(ids(&live).is_empty(), "a fresh claude starts empty");
        live.session_started(TAB, Some("resume"), Some("A"), None);
        live.last_refresh -= REFRESH_INTERVAL;
        live.tick(|_| true);
        assert_eq!(ids(&live), ["wa"], "resumed into A: A's rows again");
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn clear_done_keeps_what_runs() {
        let mut live = ManagementLive::in_memory();
        live.apply_live(
            TAB,
            Some("/nowhere".into()),
            &tick_json("s1", &["a1", "a2"]),
        );
        live.apply_live(TAB, None, &tick_json("s1", &["a1"]));
        live.clear_done(TAB, Some(1_790_000_100_000));
        let t = live.tracker(TAB).unwrap();
        assert_eq!(t.rows().len(), 1);
        assert!(t.rows()[0].running());
        assert_eq!(t.done_cleared_ms, Some(1_790_000_100_000));
        live.clear_done(TabId(99), None); // no tracker: nothing happens
    }

    #[test]
    fn closed_tabs_are_dropped() {
        let mut live = ManagementLive::in_memory();
        live.apply_live(TAB, Some("/nowhere".into()), &tick_json("s1", &["a1"]));
        live.apply_live(TabId(8), Some("/nowhere".into()), &tick_json("s2", &["b1"]));
        live.tick(|id| id == TAB);
        assert!(live.tracker(TAB).is_some());
        assert!(live.tracker(TabId(8)).is_none());
    }

    #[test]
    fn rows_survive_a_restart() {
        let path = scratch_dir("restart").join("agents.json");
        let mut live = ManagementLive::load(path.clone());
        live.apply_live(TAB, Some("/nowhere".into()), &tick_json("s1", &["a1"]));
        live.apply_live(TAB, None, &tick_json("s1", &[]));
        live.save();

        let back = ManagementLive::load(path.clone());
        let t = back.tracker(TAB).expect("restored");
        assert_eq!(t.session_id.as_deref(), Some("s1"));
        assert_eq!(t.rows().len(), 1);
        assert!(!t.rows()[0].running(), "the Done row came back Done");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// On a real worker's transcript (its first 120 lines,
    /// still working): the pane's row reads what Claude Code's agents view
    /// reads. Claude Code counts ELAPSED from the tick's `startTime` and
    /// shows the tick's `tokenCount` — the last turn's context (123,200
    /// here) plus every output token so far (734). The feed's `started`
    /// was stamped 75 s before the spawn, as a manager's `start`
    /// does, and is not the clock.
    #[test]
    fn a_worked_row_reads_as_claude_codes_agents_view() {
        use crate::management_panel::{build, fmt_tokens, stopwatch};
        use giverny_claude::subagents::LiveSnapshot;
        const REAL: &str = include_str!("../../claude/testdata/agent-api-error-resume.jsonl");
        /// 2026-09-26T17:10:37.249Z, its first line.
        const SPAWN_MS: u64 = 1_790_442_637_249;
        const CLAUDE_CODES: u64 = 123_200 + 734;
        let config =
            std::env::temp_dir().join(format!("giverny-agents-live-116-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&config);
        let subs = config.join("projects/-w/s/subagents");
        std::fs::create_dir_all(&subs).unwrap();
        std::fs::write(config.join("projects/-w/s.jsonl"), "{}\n").unwrap();
        let text: String = REAL.lines().take(120).map(|l| format!("{l}\n")).collect();
        std::fs::write(subs.join("agent-a84.jsonl"), text).unwrap();
        let feed = giverny_claude::feed::parse(
            format!(
                r#"{{"session":"s","rows":[{{"key":"demo#84","stage":"running",
                   "title":"FEATURE: selectable","started":{},"eta_s":3600}}]}}"#,
                SPAWN_MS - 75_000
            )
            .as_bytes(),
        )
        .unwrap();
        let mut t = Tracker::new(Some(config.clone()));
        let now = SPAWN_MS + 705_229;
        t.apply_live(
            &LiveSnapshot::from_value(&serde_json::json!({"session_id": "s", "tasks": [
                {"id": "a84", "type": "local_agent", "status": "running",
                 "description": "Work demo#84 select", "startTime": SPAWN_MS,
                 "tokenCount": CLAUDE_CODES}]})),
            now,
        );
        t.refresh();
        let a84 = t.get("a84").unwrap();
        assert_eq!(a84.tokens, Some(123_200), "the transcript's context");
        let l = build(Some(&feed), t.rows(), now).lines[0].clone();
        assert_eq!(l.id, "demo#84");
        assert_eq!(l.elapsed, stopwatch(705), "11:45, from the spawn");
        assert_eq!(l.tokens, fmt_tokens(CLAUDE_CODES));
        assert_eq!(l.tokens, "123.9k");
        let _ = std::fs::remove_dir_all(&config);
    }

    /// On a real worker's transcript (trimmed): cut off by
    /// `EAI_AGAIN` at 17:38:46, continued at 07:34:58 the next morning. Its
    /// row stands still with the reason while it is stopped, whatever the
    /// live list says and however long that lasts, and counts on from where
    /// it stood once it is continued.
    #[test]
    fn a_worker_stopped_by_an_api_error_freezes_its_row() {
        use crate::management_panel::build;
        use giverny_claude::subagents::LiveSnapshot;
        const REAL: &str = include_str!("../../claude/testdata/agent-api-error-resume.jsonl");
        const ERROR_MS: u64 = 1_790_444_326_816;
        const BACK_MS: u64 = 1_790_494_501_869;
        let resume_ms = BACK_MS - 3_146;
        let start = ERROR_MS - 1_700_000;
        let (before, after): (Vec<&str>, Vec<&str>) = {
            let lines: Vec<&str> = REAL.lines().collect();
            (lines[..143].to_vec(), lines[143..].to_vec())
        };
        let text = |ls: &[&str]| ls.iter().map(|l| format!("{l}\n")).collect::<String>();

        let config =
            std::env::temp_dir().join(format!("giverny-agents-live-91-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&config);
        let subs = config.join("projects/-w/s/subagents");
        std::fs::create_dir_all(&subs).unwrap();
        std::fs::write(config.join("projects/-w/s.jsonl"), "{}\n").unwrap();
        let path = subs.join("agent-a84.jsonl");
        std::fs::write(&path, text(&before)).unwrap();

        let tick = |status: &str, start: u64, tokens: u64| {
            LiveSnapshot::from_value(&serde_json::json!({"session_id": "s", "tasks": [
                {"id": "a84", "status": status, "description": "Work demo#84 select",
                 "startTime": start, "tokenCount": tokens}]}))
        };
        let feed = giverny_claude::feed::parse(
            format!(
                r#"{{"session":"s","rows":[{{"key":"demo#84","stage":"running",
                   "title":"FEATURE: selectable","started":{start},"eta_s":3600}}]}}"#
            )
            .as_bytes(),
        )
        .unwrap();
        let worked = |ms: u64| crate::management_panel::stopwatch(ms / 1000);

        let mut t = Tracker::new(Some(config.clone()));
        // Still listed running for a moment, then failed; the pane is looked
        // at ten seconds, an hour and fourteen hours on.
        t.apply_live(&tick("running", start, 131_000), ERROR_MS + 1_000);
        t.refresh();
        let mut seen = Vec::new();
        for (status, later) in [
            ("running", 10_000),
            ("failed", 3_600_000),
            ("failed", 50_000_000),
        ] {
            t.apply_live(&tick(status, start, 764), ERROR_MS + later);
            t.refresh();
            let l = build(Some(&feed), t.rows(), ERROR_MS + later).lines[0].clone();
            seen.push((l.elapsed, l.eta, l.now, l.tokens));
        }
        let frozen = (
            worked(ERROR_MS - start),
            "~32m".to_string(),
            "stopped: no network".to_string(),
            "131.8k".to_string(),
        );
        assert_eq!(seen, vec![frozen.clone(), frozen.clone(), frozen]);

        // Continued: counting on from 28:20, less the night it stood.
        std::fs::write(&path, text(&before) + &text(&after)).unwrap();
        t.apply_live(&tick("running", resume_ms, 812), BACK_MS + 60_000);
        t.refresh();
        let l = build(Some(&feed), t.rows(), BACK_MS + 60_000).lines[0].clone();
        assert_eq!(
            l.elapsed,
            worked((ERROR_MS - start) + (BACK_MS + 60_000 - resume_ms))
        );
        assert!(!l.now.starts_with("stopped"), "{}", l.now);
        assert_eq!(l.tokens, "134.8k");
        let _ = std::fs::remove_dir_all(&config);
    }
}

//! The agents pane's live rows, per tab: where relayed `subagentStatusLine`
//! ticks land.
//!
//! `giverny relay --subagent-line` forwards Claude Code's live worker list
//! with the tab it ran in (`GIVERNY_TAB_ID`, which reaches that command:
//! verified against 2.1.280, giverny#3). [`ClaudeWatch`] hands each one here,
//! and this keeps one [`Tracker`] per tab — Running rows from the ticks, Done
//! rows from the transcripts once a worker leaves the list. The pane reads
//! them through [`AgentsLive::tracker`]; nothing here draws anything.
//!
//! Rows are kept until the tab's conversation is cleared (`/clear`, which
//! Claude Code reports as `SessionStart` with `source: "clear"`) or the tab is
//! closed, and they are saved to disk so a Giverny restart keeps the Done
//! rows.
//!
//! [`ClaudeWatch`]: crate::claude_watch::ClaudeWatch

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use giverny_claude::subagents::{LiveSnapshot, Stage, Tracker};
use giverny_core::tabs::TabId;
use serde::{Deserialize, Serialize};

/// How often trackers re-read the transcripts (activity, Done detection).
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
/// How soon a change is written to disk, at the latest.
const SAVE_INTERVAL: Duration = Duration::from_secs(2);

pub struct AgentsLive {
    trackers: HashMap<TabId, Tracker>,
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

impl AgentsLive {
    /// Trackers restored from `path` (or none), saved back there.
    pub fn load(path: PathBuf) -> AgentsLive {
        let trackers = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Saved>(&b).ok())
            .map(|s| s.tabs.into_iter().map(|(id, t)| (TabId(id), t)).collect())
            .unwrap_or_default();
        AgentsLive {
            trackers,
            path: Some(path),
            dirty: false,
            last_refresh: Instant::now() - REFRESH_INTERVAL,
            last_save: Instant::now(),
        }
    }

    /// Trackers that live in memory only (tests).
    #[cfg(test)]
    pub fn in_memory() -> AgentsLive {
        AgentsLive {
            trackers: HashMap::new(),
            path: None,
            dirty: false,
            last_refresh: Instant::now() - REFRESH_INTERVAL,
            last_save: Instant::now(),
        }
    }

    /// The subagents of `tab`'s Claude session, Running and Done — what the
    /// agents pane draws. `None` until the tab's Claude has spawned a worker.
    ///
    /// [`Tracker::rows`] is the table; [`Tracker::session_id`] and
    /// [`Tracker::aliases`] name the feed file to merge with it.
    pub fn tracker(&self, tab: TabId) -> Option<&Tracker> {
        self.trackers.get(&tab)
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
        tracker.apply_live(&snap, now_ms());
        self.dirty = true;
    }

    /// A `SessionStart` in `tab`. `/clear` (`source: "clear"`) starts the
    /// table over, bound to the new session — its old ids are not aliases,
    /// or their finished workers would come back as Done rows on the next
    /// refresh. So does a start in *another* conversation — `/resume` of an
    /// older one, whose transcript has a root of its own
    /// ([`Tracker::continues`]): the table is that conversation's, rebuilt
    /// from its own `subagents/` on the next refresh, and nothing of what the
    /// tab ran before (giverny#112). Any other start — the same conversation
    /// re-id'd (compact, the agents view's switch, coo#198), or one whose
    /// root cannot be read yet — keeps the rows and records the new id beside
    /// the old (giverny#105).
    pub fn session_started(&mut self, tab: TabId, source: Option<&str>, session_id: Option<&str>) {
        let Some(tracker) = self.trackers.get_mut(&tab) else {
            return;
        };
        let elsewhere = session_id.is_some_and(|sid| tracker.continues(sid) == Some(false));
        if source == Some("clear") || elsewhere {
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

    /// `giverny pass clear-done` in `tab`: drop its Done rows (and hide the
    /// feed's that landed by then), keeping what runs. `at_ms` is when the
    /// command ran, else now.
    pub fn clear_done(&mut self, tab: TabId, at_ms: Option<u64>) {
        let now = now_ms();
        let Some(tracker) = self.trackers.get_mut(&tab) else {
            return;
        };
        let gone = tracker.clear_done(at_ms.unwrap_or(now).min(now));
        tracing::info!("tab {tab:?}: {gone} Done row(s) cleared from the agents pane");
        self.dirty = true;
    }

    /// Housekeeping, called every frame: re-read transcripts about once a
    /// second, drop trackers of tabs that no longer exist, save when changed.
    pub fn tick(&mut self, tab_exists: impl Fn(TabId) -> bool) {
        let before = self.trackers.len();
        self.trackers.retain(|id, _| tab_exists(*id));
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
            tracing::warn!("agents pane rows not saved: {err:#}");
        }
    }
}

/// What a refresh can change that is worth saving: a corrected token count
/// (giverny#92) and a stop opening or closing (giverny#91) among them, so a
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

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("giverny-agents-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_tick_becomes_rows_on_its_tab() {
        let mut live = AgentsLive::in_memory();
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
        let mut live = AgentsLive::in_memory();
        live.apply_live(TAB, Some("/nowhere".into()), &tick_json("s1", &["a1"]));

        live.session_started(TAB, Some("resume"), Some("s2"));
        let t = live.tracker(TAB).unwrap();
        assert_eq!(t.rows().len(), 1, "a resume keeps the rows");
        assert_eq!(t.aliases, vec!["s1".to_string()]);

        live.session_started(TAB, Some("clear"), Some("s3"));
        let t = live.tracker(TAB).unwrap();
        assert!(t.is_empty(), "/clear empties the table");
        assert_eq!(t.session_id.as_deref(), Some("s3"));
        assert!(t.aliases.is_empty(), "and forgets the old ids");
        assert_eq!(t.config_dir.as_deref(), Some(Path::new("/nowhere")));

        // A session start in a tab with no workers creates nothing.
        live.session_started(TabId(9), Some("clear"), Some("x"));
        assert!(live.tracker(TabId(9)).is_none());
    }

    /// giverny#112: a tab resumed into A, then B, then A again shows each
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
        let ids = |live: &AgentsLive| -> Vec<String> {
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
        let mut live = AgentsLive::in_memory();
        live.apply_live(TAB, Some(config.clone()), &tick_json("A", &["ra"]));
        live.tick(|_| true);
        assert_eq!(ids(&live), ["ra", "wa"]);

        live.session_started(TAB, Some("resume"), Some("B"));
        live.last_refresh -= REFRESH_INTERVAL;
        live.tick(|_| true);
        assert_eq!(ids(&live), ["wb"], "B's own history, nothing of A's");
        assert!(live.tracker(TAB).unwrap().aliases.is_empty());

        live.session_started(TAB, Some("resume"), Some("A"));
        live.last_refresh -= REFRESH_INTERVAL;
        live.tick(|_| true);
        assert_eq!(ids(&live), ["wa"], "back in A: A's again");

        // A session with no root of its own yet (the agents view's switch)
        // is the same pass: the rows stay and A becomes an alias.
        std::fs::create_dir_all(proj.join("A2")).unwrap();
        std::fs::write(proj.join("A2.jsonl"), "{\"type\":\"mode\"}\n").unwrap();
        live.session_started(TAB, Some("resume"), Some("A2"));
        assert_eq!(ids(&live), ["wa"]);
        assert_eq!(live.tracker(TAB).unwrap().aliases, ["A".to_string()]);
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn clear_done_keeps_what_runs() {
        let mut live = AgentsLive::in_memory();
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
        let mut live = AgentsLive::in_memory();
        live.apply_live(TAB, Some("/nowhere".into()), &tick_json("s1", &["a1"]));
        live.apply_live(TabId(8), Some("/nowhere".into()), &tick_json("s2", &["b1"]));
        live.tick(|id| id == TAB);
        assert!(live.tracker(TAB).is_some());
        assert!(live.tracker(TabId(8)).is_none());
    }

    #[test]
    fn rows_survive_a_restart() {
        let path = scratch_dir().join("agents.json");
        let mut live = AgentsLive::load(path.clone());
        live.apply_live(TAB, Some("/nowhere".into()), &tick_json("s1", &["a1"]));
        live.apply_live(TAB, None, &tick_json("s1", &[]));
        live.save();

        let back = AgentsLive::load(path.clone());
        let t = back.tracker(TAB).expect("restored");
        assert_eq!(t.session_id.as_deref(), Some("s1"));
        assert_eq!(t.rows().len(), 1);
        assert!(!t.rows()[0].running(), "the Done row came back Done");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// giverny#91, on giverny#84's real transcript (trimmed): cut off by
    /// `EAI_AGAIN` at 17:38:46, continued at 07:34:58 the next morning. Its
    /// row stands still with the reason while it is stopped, whatever the
    /// live list says and however long that lasts, and counts on from where
    /// it stood once it is continued.
    #[test]
    fn a_worker_stopped_by_an_api_error_freezes_its_row() {
        use crate::agents_pane::build;
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
                {"id": "a84", "status": status, "description": "Work giverny#84 select",
                 "startTime": start, "tokenCount": tokens}]}))
        };
        let feed = giverny_claude::feed::parse(
            format!(
                r#"{{"session":"s","rows":[{{"key":"giverny#84","stage":"running",
                   "title":"FEATURE: selectable","started":{start},"eta_s":3600}}]}}"#
            )
            .as_bytes(),
        )
        .unwrap();
        let worked = |ms: u64| crate::agents_pane::stopwatch(ms / 1000);

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

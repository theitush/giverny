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
    /// refresh. Any other start (resume, compact, a new `claude`) keeps the
    /// rows and records the new id beside the old.
    pub fn session_started(&mut self, tab: TabId, source: Option<&str>, session_id: Option<&str>) {
        let Some(tracker) = self.trackers.get_mut(&tab) else {
            return;
        };
        if source == Some("clear") {
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
                .filter(|(_, t)| !t.is_empty())
                .map(|(id, t)| (id.0, t.clone()))
                .collect(),
        };
        if let Err(err) = write_atomic(path, &saved) {
            tracing::warn!("agents pane rows not saved: {err:#}");
        }
    }
}

/// What a refresh can change that is worth saving.
fn signature(t: &Tracker) -> Vec<(String, Stage, Option<u64>)> {
    t.rows()
        .iter()
        .map(|r| (r.id.clone(), r.stage, r.ended_ms))
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
}

//! The management panel: a table under a tab's terminal listing that tab's Claude
//! Code subagents — Running, Planned and Done — one row each.
//!
//! Off by default (`claude.management_panel`); off, nothing here runs. On, it is
//! drawn as a resizable bottom panel inside the terminal's area, only when
//! subagents are involved (giverny#357): in a tab whose tracker holds a
//! real subagent, running or done this session, or whose session is a
//! manager — the manage skill ran in it ([`giverny_claude::managers`]) — so
//! its Next up shows before its first spawn. A feed never opens it alone
//! ([`feed::opens`]).
//!
//! The rows are a [`Tracker`] (Claude Code's own data: live list,
//! transcripts, notifications) merged with the optional feed file
//! ([`feed::merge`], `docs/management-panel.md`), and **every row drawn is a
//! real agent** ([`Drawn`]): a feed row with no subagent behind it is not
//! drawn, save a manager's own Next up and build rows. The manager's chores
//! are worked out from the same drawn rows, so a session that is no manager
//! is never told of a row it cannot see. This module owns only the look:
//! the columns STAGE, TASK, ELAPSED, ETA, NOW and TOKENS, every one but TASK a
//! fixed width, no column headings, the whole row tinted by its stage, a
//! stopwatch ELAPSED ticking every second, `~1h3m` ETAs, a Done row's
//! `(+5m)`/`(-1h20m)`, `"` for a cell that repeats the same worker's row
//! above it — a compact status table pinned under
//! Claude Code. The session's token total is not here: it is on the status
//! line's model row, and so is the manager's name (`giverny manage name`):
//! `Opus 5.5 (Juniper)`.
//!
//! **The rows are not kept here.** [`show`] is handed the tab's tracker —
//! `ClaudeWatch::agents` (`management_live.rs`), fed by the relay, persisted,
//! emptied on `/clear` and refreshed once a second — and keeps only what the
//! pane itself needs per tab: the feed it last read.
//!
//! **Clocks stop while nothing can run**: while the tab's
//! account is out of a usage limit ([`Limit`]), a Running row's ELAPSED
//! stops and its ETA holds instead of counting down past zero, and NOW says
//! `5h limit → 13:00`. The pane remembers each such span ([`Hold`]) and
//! both clocks carry on from where they stopped once the limit resets. A
//! feed row the manager has paused (`paused_since`) is held
//! the same way, and reads `paused since 12:58`. Planned ETAs are durations
//! and hold by themselves; a Done row is measured history and never moves.
//!
//! **Clicks** produce a [`RowClick`], which the app receives as
//! `Action::AgentRowClicked`. What a click *does* is not decided here, and
//! it leaves no mark: a row is tinted only while the pointer is on it
//! ([`row_tint`]), so nothing stays highlighted after a click.
//!
//! **Nesting** (giverny#464): an agent a worker spawned is drawn under
//! that worker's row, indented a step per level, and goes where that row
//! goes ([`nest`]); a toggle at the head of the worker's title folds them
//! away ([`Fold`]), kept per tab while Giverny runs.
//!
//! **Resources**: what a row's `giverny manage run`
//! commands use in a quiet column — live CPU and memory while one runs, the
//! memory peak once the row is Done, zero when there is neither — its lease from the machine ledger in
//! its overlay header, a Next up row's place in the ledger's queue in NOW,
//! and the machine's leases against its limits on a dim bottom line. The
//! ledger and the commands' cgroups are read off the UI thread
//! ([`LedgerWatch`]); the ledger wins over a row's own copy ([`with_ledger`]).
//!
//! **Text is selectable**: a drag — never a click — selects
//! the pane's text the way the terminal does, as a stream of cells across
//! rows, copies it as plain text line by line when the drag ends, and
//! `Ctrl+Shift+C` copies it again. The next press anywhere lets it go.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Color32, CursorIcon, Sense, Ui};
use giverny_claude::agent_eta::{self, Etas};
use giverny_claude::bg_shells;
use giverny_claude::feed::{
    self, Drawn, Feed, FeedCache, LeaseState, PaneRow, RowLease, RowUsage, Stage,
};
use giverny_claude::manage_chores::{self, Chore};
use giverny_claude::resources::{self, Ledger};
use giverny_claude::run_live::{RunLive, TaskLive};
use giverny_claude::session_use;
use giverny_claude::sleeps;
use giverny_claude::subagents::{Outcome, SubagentRow, Tracker};
use giverny_claude::worker_log::WorkerLog;
use giverny_core::config::{DoneRows, ManagementPanelConfig, PaneColumns};
use giverny_core::limits::{Limits, Machine, Mem, Resolved};
use giverny_core::tabs::TabId;
use giverny_term::widget::RenderShared;

use crate::chrome::Chrome;
use crate::claude_watch::ClaudeWatch;
use crate::sessions_load::{self, Load};

/// The three stage tints, the 256-colour codes 38;5;32, 38;5;67 and
/// 38;5;28, picked to read on dark and light
/// themes alike.
pub const RUNNING: Color32 = Color32::from_rgb(0x00, 0x87, 0xd7);
pub const PLANNED: Color32 = Color32::from_rgb(0x5f, 0x87, 0xaf);
pub const DONE: Color32 = Color32::from_rgb(0x00, 0x87, 0x00);

/// How often the feed file is stat'ed.
const POLL: Duration = Duration::from_secs(1);

// Column widths, in characters.
const STAGE_W: usize = 7;
const EL_W: usize = 8;
const ETA_W: usize = 10;
const NOW_W: usize = 28;
const TOK_W: usize = 6;
const GAP: usize = 2;
const MIN_TITLE: usize = 12;

/// What a Running row's ETA cell says when nobody gave it an estimate —
/// drawn dim, so a row the dispatcher forgot to estimate reads as missing
/// one rather than as a blank.
pub const NO_ETA: &str = "no ETA";
/// Where in [`Cols::segments`] the ETA cell sits.
const ETA_SEG: usize = 3;
/// Where in [`Cols::segments`] the NOW cell sits.
const NOW_SEG: usize = 4;
/// Where in [`Cols::segments`] the use cell sits.
const LEASE_SEG: usize = 6;

// --------------------------------------------------------- view state ----

/// What the pane itself keeps per tab: the feed it last read and the text
/// dragged over. Never the rows — those are the tracker's — and no row
/// selection: a click acts and leaves no mark.
#[derive(Default)]
struct View {
    feed: FeedCache,
    feed_now: Option<Feed>,
    /// The session's agent ETAs: the estimates of workers that are no
    /// manager session's task.
    etas: agent_eta::Cache,
    etas_now: Etas,
    feed_session: Option<String>,
    /// The session is a manager: the manage skill ran in it
    /// ([`giverny_claude::managers`], giverny#357). Looked up with the feed.
    manager: bool,
    /// The session's conversation root, once read: what a resumed
    /// manager's mark is found by, read from its transcript once per id.
    root: Option<String>,
    last_poll: Option<Instant>,
    /// The height the pane last sized itself to, while the user has not
    /// dragged it; `None` once they have.
    fit: Option<f32>,
    /// Every usage-limit span this tab's clocks were held through.
    holds: Vec<Hold>,
    /// The text dragged over, until the next press.
    sel: Option<Selection>,
    /// Where the rows were drawn last frame, and the cell width: what the
    /// debug `drag` command aims at.
    rows_at: Vec<egui::Rect>,
    cell_w: f32,
    /// The rows' clicks as drawn last frame: what the debug `hover`
    /// command finds a row by.
    #[cfg(debug_assertions)]
    clicks: Vec<RowClick>,
    /// The pointer over the rows last frame: `Some(None)` on the pane but
    /// not on a worker's row, `Some(Some(row))` on that row.
    hover: Option<Option<RowClick>>,
    /// The transcripts are due a poll (with the feed, once a [`POLL`]).
    logs_due: bool,
    /// Each worker's whole transcript, by agent id: its turns' tokens and
    /// its dispatcher's messages, to split a worker that took tasks one
    /// after another into a row per task.
    logs: Logs,
    /// The chores last kept for this tab's manager, and for which session
    /// ([`Views::keep_chores`]): written again only when they change.
    chores: Option<(String, Vec<Chore>)>,
    /// The Done rows this tab's pane has shown, by [`done_ident`]: what of
    /// it was seen, kept up every frame it is drawn (giverny#394). `None`
    /// until it is first drawn.
    seen_done: Option<HashSet<String>>,
    /// The tab was just looked at with its unseen-finish tick on
    /// ([`Views::flash_new_done`]): the next drawing flashes what landed
    /// since the last look.
    flash_due: bool,
    /// The newly done rows breathing now, if any.
    flash: Option<Flash>,
    /// The rows whose agents' own agents are folded away under them
    /// (giverny#464), by [`fold_ident`]: kept for as long as the tab is.
    folded: HashSet<String>,
    /// The session this view first saw, and when: a session's hook mark
    /// is looked for only after [`HOOKLESS_GRACE`].
    since: Option<(String, Instant)>,
    /// No Giverny plugin hook ever ran in the session (giverny#271): the
    /// pane says so ([`HOOKLESS`]). Looked up with the feed.
    hookless: bool,
    /// The workers' own background shells still running (giverny#476):
    /// what tells a worker asleep on its own work from one that ended.
    /// Looked at with the feed.
    shells: giverny_claude::bg_shells::Watch,
}

/// How long a session may run before its missing hook mark is shown: its
/// `SessionStart` hook writes it, but the pane may see the session first.
const HOOKLESS_GRACE: Duration = Duration::from_secs(20);

/// The pane's mark for a session no Giverny plugin hook ran in
/// ([`giverny_claude::escapes::hooked`], giverny#271).
pub const HOOKLESS: &str = "no guard: this session ran no Giverny plugin hook (plugin off, or an older claude); the memory cap still holds";

/// The table's footer with the hookless mark, when the session has it,
/// put first.
fn with_hookless(footer: Option<String>, hookless: bool) -> Option<String> {
    if !hookless {
        return footer;
    }
    Some(match footer {
        Some(f) => format!("{HOOKLESS} · {f}"),
        None => HOOKLESS.to_string(),
    })
}

/// The newly done rows' frames breathing together (giverny#394): which rows
/// ([`done_ident`]) and from when, on the animation clock.
struct Flash {
    rows: HashSet<String>,
    from: f64,
}

/// How many breaths the newly done rows' frames take: a few, together, then
/// still.
const FLASH_BREATHS: f64 = 3.0;

/// How long a flash lasts, in seconds: its breaths on the rail's calm
/// timing ([`crate::rail::CALM_PERIOD`]), so the pane and the rail's marks
/// breathe alike.
const FLASH_S: f64 = FLASH_BREATHS * crate::rail::CALM_PERIOD;

/// How lit a flashing row's frame is `t` seconds into the flash, 0 to 1:
/// the calm breath, out from nothing and back, [`FLASH_BREATHS`] times, so
/// it ends where it began, unframed. With `rail.animate` off (`animate`) a
/// steady frame for the same span instead. `None` once the flash is over
/// (or before it began).
fn flash_glow(t: f64, animate: bool) -> Option<f32> {
    if !(0.0..FLASH_S).contains(&t) {
        return None;
    }
    Some(if animate {
        crate::rail::calm_breath(Some(t))
    } else {
        1.0
    })
}

/// What a Done row is known by from one look at the pane to the next: the
/// feed's key, else its worker's id, else its name — as
/// [`RowClick::same_row`] tells rows apart.
fn done_ident(c: &RowClick) -> String {
    if !c.key.is_empty() {
        format!("k:{}", c.key)
    } else if let Some(a) = &c.agent_id {
        format!("a:{a}")
    } else {
        format!("n:{}", c.name)
    }
}

/// The Done rows of `lines` that landed since the pane was last looked at:
/// those it never showed (`seen`). A pane not drawn since Giverny started
/// (`seen` is `None`) knows nothing of what was seen before, so only rows
/// that landed since then (`born_ms`) count as new.
fn newly_done(lines: &[Line], seen: Option<&HashSet<String>>, born_ms: u64) -> HashSet<String> {
    lines
        .iter()
        .filter(|l| l.stage == Stage::Done)
        .filter(|l| match seen {
            Some(seen) => !seen.contains(&done_ident(&l.click)),
            None => l.landed_ms.is_some_and(|e| e >= born_ms),
        })
        .map(|l| done_ident(&l.click))
        .collect()
}

/// Note every Done row of `lines` as seen: the pane is showing them.
fn note_seen(seen: &mut Option<HashSet<String>>, lines: &[Line]) {
    let seen = seen.get_or_insert_with(HashSet::new);
    for l in lines.iter().filter(|l| l.stage == Stage::Done) {
        seen.insert(done_ident(&l.click));
    }
}

impl View {
    /// Re-read the feed for `session`, at most once a [`POLL`] (or at once
    /// when the session changed). A session Claude Code re-id'd is looked
    /// up by its earlier ids too, newest first, until its files name the
    /// new one (giverny#105).
    ///
    /// The manager mark is looked up with it ([`View::manager`]): a `stat`
    /// of the session's mark and its earlier ids', and of its
    /// conversation's, whose root is read from the transcript (under
    /// `config`) once.
    fn poll_feed(&mut self, session: Option<&str>, aliases: &[String], config: Option<&Path>) {
        let changed = self.feed_session.as_deref() != session;
        if !changed && self.last_poll.is_some_and(|t| t.elapsed() < POLL) {
            return;
        }
        self.last_poll = Some(Instant::now());
        if changed {
            self.root = None;
        }
        self.feed_session = session.map(str::to_string);
        let ids: Vec<&str> = session
            .into_iter()
            .chain(aliases.iter().rev().map(String::as_str))
            .collect();
        let dir = feed::feed_dir();
        self.manager = session.is_some_and(|s| {
            let root = &mut self.root;
            giverny_claude::managers::is_marked(&dir, s, &ids[1..], || {
                if root.is_none() {
                    *root = giverny_claude::continuation::root_of(config, s);
                }
                root.clone()
            })
        });
        self.feed_now = session.and_then(|_| self.feed.poll_any(&dir, &ids).cloned());
        if self.since.as_ref().map(|s| s.0.as_str()) != session {
            self.since = session.map(|s| (s.to_string(), Instant::now()));
        }
        self.hookless = self
            .since
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= HOOKLESS_GRACE)
            && !giverny_claude::escapes::hooked(&dir, &ids);
        self.etas_now = session
            .map(|_| self.etas.poll_any(&dir, &ids).clone())
            .unwrap_or_default();
        self.shells.poll(&dir, &ids, now_ms());
        self.logs_due = true;
    }
}

/// Every worker's [`WorkerLog`], by agent id.
pub type Logs = HashMap<String, WorkerLog>;

/// Follow each worker's transcript: a new one is read whole once, a known
/// one only for what was appended (a `stat` when nothing was). Workers no
/// longer listed are forgotten.
pub fn poll_logs(logs: &mut Logs, live: &[SubagentRow]) {
    logs.retain(|id, _| live.iter().any(|l| &l.id == id));
    for l in live {
        let Some(path) = &l.transcript else { continue };
        let log = logs
            .entry(l.id.clone())
            .or_insert_with(|| WorkerLog::new(path));
        if log.path() != path {
            *log = WorkerLog::new(path);
        }
        log.poll();
    }
}

/// Every tab's pane view state.
pub struct Views {
    tabs: HashMap<TabId, View>,
    /// The machine ledger, read off the UI thread: one for every tab, as
    /// there is one ledger for the whole machine.
    ledger: LedgerWatch,
    /// When every manager tab's chores were last worked out.
    chores_at: Option<Instant>,
    /// What the machine slept through: one log for the whole machine.
    sleeps: sleeps::Cache,
    /// When Giverny started (epoch ms): a pane not drawn since knows only
    /// that rows landed after it went unseen ([`newly_done`]).
    born_ms: u64,
}

impl Default for Views {
    fn default() -> Self {
        Views {
            tabs: HashMap::new(),
            ledger: LedgerWatch::default(),
            chores_at: None,
            sleeps: sleeps::Cache::default(),
            born_ms: now_ms(),
        }
    }
}

/// How often every manager tab's chores are worked out ([`Views::keep_chores`]).
const CHORES_EVERY: Duration = Duration::from_secs(10);

impl Views {
    /// Keep what each tab's manager has to be told ([`chores`]) where the
    /// plugin's hook finds it ([`manage_chores::publish`]): the due chores
    /// its rows were not told of yet, worked out from the very rows the
    /// pane draws, for every tab whose session is up (`tabs`) — the one in
    /// view or not. Call every frame; it works once a [`CHORES_EVERY`].
    pub fn keep_chores<'t>(&mut self, tabs: impl IntoIterator<Item = (TabId, &'t Tracker)>) {
        if self.chores_at.is_some_and(|t| t.elapsed() < CHORES_EVERY) {
            return;
        }
        self.chores_at = Some(Instant::now());
        let now = now_ms();
        let dir = feed::feed_dir();
        let ledger = giverny_claude::manage_inbox::read_ledger(&ledger_file(), now);
        for (tab, tracker) in tabs {
            let Some(session) = tracker.session_id.clone() else {
                continue;
            };
            let view = self.tabs.entry(tab).or_default();
            view.poll_feed(
                Some(&session),
                &tracker.aliases,
                tracker.config_dir.as_deref(),
            );
            if std::mem::take(&mut view.logs_due) {
                poll_logs(&mut view.logs, tracker.rows());
            }
            // Worked out from the rows the pane draws: a session with
            // nothing drawn has nothing to be told (giverny#357).
            let opens = feed::opens(!tracker.is_empty(), view.manager);
            let drawn = Drawn::of(view.manager);
            let due = match view.feed_now.as_ref().filter(|_| opens) {
                Some(f) => {
                    let cleared = tracker.done_cleared_ms.map(|c| f.without_done_by(c));
                    let f = cleared.as_ref().unwrap_or(f);
                    let leased = ledger.as_ref().map(|l| {
                        let mut sessions: Vec<&str> =
                            f.session.iter().map(String::as_str).collect();
                        sessions.extend(f.aliases.iter().map(String::as_str));
                        sessions.push(&session);
                        sessions.extend(tracker.aliases.iter().map(String::as_str));
                        with_ledger(f, &sessions, l)
                    });
                    let f = leased.as_ref().unwrap_or(f);
                    due_chores(
                        drawn,
                        f,
                        tracker.rows(),
                        &view.logs,
                        view.shells.live(),
                        now,
                    )
                }
                None => Vec::new(),
            };
            let same = view
                .chores
                .as_ref()
                .is_some_and(|(s, c)| *s == session && *c == due);
            if same {
                continue;
            }
            match manage_chores::publish(&dir, &session, &due) {
                Ok(()) => view.chores = Some((session, due)),
                Err(e) => tracing::warn!("management panel: chores not kept: {e}"),
            }
        }
    }

    /// The user looked at `tab` with its unseen-finish tick on
    /// (giverny#388): when its pane is next drawn, the Done rows that landed
    /// since they last looked breathe their frames together a few times,
    /// then go still (giverny#394).
    pub fn flash_new_done(&mut self, tab: TabId) {
        self.tabs.entry(tab).or_default().flash_due = true;
    }

    /// Forget a closed tab.
    pub fn forget(&mut self, tab: TabId) {
        self.tabs.remove(&tab);
    }

    /// Where the pointer was over `tab`'s rows when the pane was last drawn,
    /// taken so it is never read twice: `None` off the pane (or when the
    /// pane was not drawn), `Some(None)` on it but off every row,
    /// `Some(Some(row))` on a row.
    pub fn take_hover(&mut self, tab: TabId) -> Option<Option<RowClick>> {
        self.tabs.get_mut(&tab)?.hover.take()
    }

    /// Debug builds: the row of `tab`'s pane that `pick` names (its feed
    /// key, agent id or name), as drawn last frame.
    #[cfg(debug_assertions)]
    pub fn row_named(&self, tab: TabId, pick: &str) -> Option<usize> {
        let v = self.tabs.get(&tab)?;
        v.clicks
            .iter()
            .position(|c| c.key == pick || c.agent_id.as_deref() == Some(pick) || c.name == pick)
    }

    /// Debug builds: the point on `tab`'s pane at `row`'s `col`-th cell
    /// boundary, as drawn last frame, vertically centred on the row.
    #[cfg(debug_assertions)]
    pub fn cell_point(&self, tab: TabId, row: usize, col: usize) -> Option<egui::Pos2> {
        let v = self.tabs.get(&tab)?;
        let r = v.rows_at.get(row)?;
        Some(egui::pos2(r.left() + col as f32 * v.cell_w, r.center().y))
    }
}

// ------------------------------------------------------------ limits ----

/// A usage limit the tab's account is out on right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limit {
    /// When it reopens (epoch ms), when anything knows.
    pub reopens_ms: Option<u64>,
    /// Which window: `5h`, `7d`; `None` when the source does not say.
    pub window: Option<&'static str>,
}

impl Limit {
    /// Still out at `now_ms`: a reset time that has come round is no limit.
    fn out_at(&self, now_ms: u64) -> bool {
        self.reopens_ms.is_none_or(|r| r > now_ms)
    }
}

/// One span in which the account could run nothing. `until_ms` is `None`
/// while it is still out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hold {
    pub since_ms: u64,
    pub until_ms: Option<u64>,
    /// The reset the limit named, so a hold closed late (the tab was not
    /// being drawn when the window reopened) ends at the reset, not then.
    pub reopens_ms: Option<u64>,
}

/// How many holds a tab remembers; older ones are before any row's start.
const MAX_HOLDS: usize = 32;

/// Open a hold when a limit is first seen out, close it when it is gone.
pub fn track(holds: &mut Vec<Hold>, limit: Option<&Limit>, now_ms: u64) {
    let out = limit.filter(|l| l.out_at(now_ms));
    let open = holds.last_mut().filter(|h| h.until_ms.is_none());
    match (out, open) {
        (Some(l), Some(h)) => h.reopens_ms = l.reopens_ms.or(h.reopens_ms),
        (Some(l), None) => {
            holds.push(Hold {
                since_ms: now_ms,
                until_ms: None,
                reopens_ms: l.reopens_ms,
            });
            if holds.len() > MAX_HOLDS {
                holds.remove(0);
            }
        }
        (None, Some(h)) => {
            let end = h.reopens_ms.map_or(now_ms, |r| r.min(now_ms));
            h.until_ms = Some(end.max(h.since_ms));
        }
        (None, None) => {}
    }
}

/// Milliseconds of `[start, upto]` that fall inside a hold or a sleep —
/// what comes off a row's clock. A sleep inside a hold comes off once.
pub fn off_ms(holds: &[Hold], slept: &[sleeps::Span], start: u64, upto: u64) -> u64 {
    let held = holds
        .iter()
        .map(|h| (h.since_ms, h.until_ms.unwrap_or(upto)));
    let slept = slept.iter().map(|z| (z.since, z.until));
    sleeps::off_ms(held.chain(slept), start, upto)
}

/// The usage limit `tab` is stopped by, if any: the app's own record of a
/// tab that stopped on a limit (`stopped`, its reset time if known), else
/// the tab's account's meters showing a window used up with a reset still
/// to come. Read-only on `claude`.
pub fn limit_for(
    claude: &ClaudeWatch,
    tab: TabId,
    stopped: Option<Option<jiff::Timestamp>>,
    now: jiff::Timestamp,
) -> Option<Limit> {
    let account = claude
        .tabs
        .get(&tab)
        .and_then(|t| t.account.as_deref())
        .and_then(|name| claude.accounts.iter().find(|a| a.profile.name == name));
    let mut windows: Vec<(&'static str, f64, Option<jiff::Timestamp>)> = Vec::new();
    if let Some(panel) = account {
        for (kind, short, pick, reset) in [
            (
                "session",
                "5h",
                (|l: &crate::claude_watch::LiveUsage| l.five_hour)
                    as fn(&crate::claude_watch::LiveUsage) -> Option<f64>,
                (|l: &crate::claude_watch::LiveUsage| l.five_hour_resets)
                    as fn(&crate::claude_watch::LiveUsage) -> Option<jiff::Timestamp>,
            ),
            ("weekly_all", "7d", |l| l.seven_day, |l| l.seven_day_resets),
        ] {
            let cached = panel
                .usage
                .as_ref()
                .and_then(|u| u.limits.iter().find(|l| l.kind == kind));
            match cached {
                Some(entry) => {
                    let r = ClaudeWatch::reading(panel, entry, now);
                    windows.push((short, r.percent, r.resets));
                }
                None => {
                    if let Some(live) = &panel.live
                        && let Some(pct) = pick(live)
                    {
                        windows.push((short, pct, reset(live)));
                    }
                }
            }
        }
    }
    let ms = |t: jiff::Timestamp| t.as_millisecond().max(0) as u64;
    spent(&windows, ms(now)).or_else(|| {
        stopped.map(|reopens| Limit {
            reopens_ms: reopens.map(ms),
            window: None,
        })
    })
}

/// The window that is used up, from `(window, percent, resets)` readings:
/// out is 100%, and only with a reset still to come (a meter whose reset has
/// passed is describing a window that is over). Two out at once: the one
/// that reopens last, since nothing runs until both have.
pub fn spent(
    windows: &[(&'static str, f64, Option<jiff::Timestamp>)],
    now_ms: u64,
) -> Option<Limit> {
    windows
        .iter()
        .filter(|(_, pct, _)| *pct >= 100.0)
        .filter_map(|(w, _, at)| {
            let at = at.map(|t| t.as_millisecond().max(0) as u64)?;
            (at > now_ms).then_some((*w, at))
        })
        .max_by_key(|(_, at)| *at)
        .map(|(w, at)| Limit {
            reopens_ms: Some(at),
            window: Some(w),
        })
}

/// NOW on a held row: `5h limit → 13:00`, the weekday in front for a reset
/// not today, `limit` alone when nothing says when.
pub fn limit_note(limit: &Limit, now_ms: u64, tz: &jiff::tz::TimeZone) -> String {
    let what = match limit.window {
        Some(w) => format!("{w} limit"),
        None => "limit".to_string(),
    };
    match limit.reopens_ms {
        Some(at) => format!("{what} → {}", clock_at(at, now_ms, tz)),
        None => what,
    }
}

/// `13:00` for an instant later today, `Mon 08:00` for one further off.
fn clock_at(at_ms: u64, now_ms: u64, tz: &jiff::tz::TimeZone) -> String {
    let z = |ms: u64| {
        jiff::Timestamp::from_millisecond(ms as i64)
            .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
            .to_zoned(tz.clone())
    };
    let (at, now) = (z(at_ms), z(now_ms));
    if at.date() == now.date() {
        at.strftime("%H:%M").to_string()
    } else {
        at.strftime("%a %H:%M").to_string()
    }
}

/// What the table's clocks need beyond `now`: the holds, the limit out now
/// (for NOW), and the zone NOW writes times in.
pub struct Clock<'a> {
    pub holds: &'a [Hold],
    /// What the machine slept through ([`sleeps`]): off every clock.
    pub sleeps: &'a [sleeps::Span],
    pub limit: Option<&'a Limit>,
    pub tz: jiff::tz::TimeZone,
    /// Each worker's processes' use now, by agent id: what a Running row
    /// with no feed figure of its own shows.
    pub workers: &'a HashMap<String, RunLive>,
    /// Each worker's agent ETA, by agent id: what a row with no feed
    /// estimate counts down from.
    pub etas: &'a Etas,
    /// Each worker's own background shells still running, by agent id
    /// (giverny#476): a worker whose turn ended with one is asleep.
    pub shells: &'a Shells,
}

/// The workers' live background shells, by agent id ([`bg_shells::Watch`]).
pub type Shells = HashMap<String, Vec<bg_shells::Shell>>;

/// No worker measured.
#[cfg(any(test, debug_assertions))]
static NO_WORKERS: std::sync::LazyLock<HashMap<String, RunLive>> =
    std::sync::LazyLock::new(HashMap::new);

/// No agent ETAs.
#[cfg(any(test, debug_assertions))]
static NO_ETAS: std::sync::LazyLock<Etas> = std::sync::LazyLock::new(Etas::new);

/// No background shells.
#[cfg(any(test, debug_assertions))]
static NO_SHELLS: std::sync::LazyLock<Shells> = std::sync::LazyLock::new(Shells::new);

#[cfg(any(test, debug_assertions))]
impl Clock<'_> {
    pub(crate) fn plain() -> Clock<'static> {
        Clock {
            holds: &[],
            sleeps: &[],
            limit: None,
            tz: jiff::tz::TimeZone::system(),
            workers: &NO_WORKERS,
            etas: &NO_ETAS,
            shells: &NO_SHELLS,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// --------------------------------------------------------- resources ----

/// How often the ledger is re-read while a pane is up.
const LEDGER_POLL: Duration = Duration::from_secs(2);
/// No pane drawn for this long: the reader stops reading until one is.
const LEDGER_IDLE_MS: u64 = 10_000;
/// How often the machine itself (cores, RAM, `nvidia-smi`) is re-detected.
const MACHINE_EVERY: Duration = Duration::from_secs(300);

/// What the machine ledger said when it was last read: its live leases and
/// queue (expired entries dropped, nothing written back) and the limits
/// they count against.
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerView {
    pub ledger: Ledger,
    /// `[manager.limits]` resolved for this machine; `None` when the
    /// config could not be read.
    pub limits: Option<Resolved>,
}

/// The ledger, read by a thread of its own: the read takes the
/// ledger's `flock`, which a writer may hold, and resolving the limits runs
/// `nvidia-smi` — neither belongs on the UI thread. The pane
/// only ever takes the last snapshot.
#[derive(Default)]
struct LedgerWatch {
    shared: Option<Arc<LedgerShared>>,
}

#[derive(Default)]
struct LedgerShared {
    snap: Mutex<Option<Arc<LedgerView>>>,
    /// When a pane last asked: the reader idles once nothing has for
    /// [`LEDGER_IDLE_MS`].
    wanted_ms: AtomicU64,
}

impl LedgerWatch {
    /// The last snapshot, starting the reader on first use. `None` until
    /// its first read.
    fn get(&mut self, ctx: &egui::Context) -> Option<Arc<LedgerView>> {
        let shared = self.shared.get_or_insert_with(|| {
            let shared = Arc::new(LedgerShared::default());
            let (s, ctx) = (shared.clone(), ctx.clone());
            if let Err(err) = std::thread::Builder::new()
                .name("management-panel-ledger".into())
                .spawn(move || read_ledger_loop(&s, &ctx))
            {
                tracing::warn!("management panel: the ledger reader did not start: {err}");
            }
            shared
        });
        shared.wanted_ms.store(now_ms(), Ordering::Relaxed);
        shared.snap.lock().ok().and_then(|s| s.clone())
    }
}

fn read_ledger_loop(shared: &LedgerShared, ctx: &egui::Context) {
    let mut machine: Option<(Machine, Instant)> = None;
    loop {
        let now = now_ms();
        if now.saturating_sub(shared.wanted_ms.load(Ordering::Relaxed)) <= LEDGER_IDLE_MS {
            let m = match &machine {
                Some((m, at)) if at.elapsed() < MACHINE_EVERY => m.clone(),
                _ => {
                    let m = Machine::detect();
                    machine = Some((m.clone(), Instant::now()));
                    m
                }
            };
            if let Some(view) = read_ledger(&m, now) {
                let mut snap = match shared.snap.lock() {
                    Ok(s) => s,
                    Err(p) => p.into_inner(),
                };
                if snap.as_deref() != Some(&view) {
                    *snap = Some(Arc::new(view));
                    drop(snap);
                    ctx.request_repaint();
                }
            }
        }
        std::thread::sleep(LEDGER_POLL);
    }
}

/// One read of the ledger, under its lock, changing nothing on disk. No
/// ledger is an empty one; an unreadable one is `None` (the last good read
/// stays).
fn read_ledger(machine: &Machine, now: u64) -> Option<LedgerView> {
    let path = ledger_file();
    let mut ledger = if path.exists() {
        let _lock = resources::LedgerLock::take(&path).ok()?;
        Ledger::parse(&std::fs::read(&path).ok()?).ok()?
    } else {
        Ledger::default()
    };
    ledger.expire(now);
    let limits = Limits::load().ok().map(|l| l.resolve(machine));
    Some(LedgerView { ledger, limits })
}

fn ledger_file() -> PathBuf {
    resources::ledger_path(&feed::feed_dir())
}

/// `feed` with each row's `live` use: its task's running commands under any
/// of `sessions`, matched as [`with_ledger`] matches leases.
pub fn with_live(mut feed: Feed, sessions: &[&str], live: &[TaskLive]) -> Feed {
    for row in &mut feed.rows {
        row.live = live
            .iter()
            .find(|t| t.task == row.key && sessions.contains(&t.session.as_str()))
            .map(|t| t.live);
    }
    feed
}

/// `feed` with each row whose worker's processes were measured showing
/// them: everything its Bash commands started ([`giverny_claude::worker_pids`]),
/// whether or not they ran under `giverny manage run`. A run of the row's
/// task under that worker is part of the worker's figure already; one
/// started elsewhere is added to it.
pub fn with_workers(
    mut feed: Feed,
    sessions: &[&str],
    live: &[TaskLive],
    workers: &HashMap<String, RunLive>,
) -> Feed {
    for row in &mut feed.rows {
        let Some(w) = row.agent_id.as_deref().and_then(|id| workers.get(id)) else {
            continue;
        };
        let run = live
            .iter()
            .find(|t| t.task == row.key && sessions.contains(&t.session.as_str()));
        row.live = Some(match run {
            Some(t) if t.agent.is_some() && t.agent == row.agent_id => *w,
            Some(t) => w.plus(t.live),
            None => *w,
        });
    }
    feed
}

/// `feed` with every row's `lease` taken from the ledger rather than the
/// row's own copy, which is only as fresh as its session's last `claim`: a
/// lease another session released, or one that expired, is not pushed back
/// to rows. A row's lease is the ledger's entry for its key
/// under any of `sessions` (the feed's and the tab's ids); a row with none
/// holds nothing. A queued row's place is its place now. It is short of
/// what its copy says (its last claim), and waits behind whoever is in the
/// way of that now ([`resources::in_the_way`]). A copy that says nothing
/// short (from before giverny#333) waits behind the task it named while
/// that one still holds or waits ahead, else whoever holds a slot it asks
/// for, else the request queued before it.
pub fn with_ledger(feed: &Feed, sessions: &[&str], ledger: &Ledger) -> Feed {
    let mut f = feed.clone();
    let mine = |s: &str| sessions.contains(&s);
    let queue = ledger.ordered_queue();
    for row in &mut f.rows {
        let copy = row.lease.take();
        if let Some(l) = ledger
            .leases
            .iter()
            .find(|l| l.task == row.key && mine(&l.session))
        {
            // Smaller is only on the copy: the ledger keeps what was granted.
            let smaller = copy
                .as_ref()
                .filter(|c| c.state == LeaseState::Smaller && c.id.as_deref() == Some(&l.id));
            row.lease = Some(RowLease {
                state: if smaller.is_some() {
                    LeaseState::Smaller
                } else {
                    LeaseState::Granted
                },
                id: Some(l.id.clone()),
                ram_mb: l.ram_mb,
                gpus: l.gpus.clone(),
                vram_mb: l.vram_mb,
                slots: l.slots.clone(),
                granted_ms: Some(l.granted_at),
                wanted_ram_mb: smaller.and_then(|c| c.wanted_ram_mb),
                position: None,
                behind: None,
                short: Vec::new(),
                uncollected: l.uncollected.is_some(),
                lapsed: false,
            });
        } else if let Some(i) = queue
            .iter()
            .position(|w| w.task == row.key && mine(&w.session))
        {
            let w = queue[i];
            let ahead = &queue[..i];
            // What it is short of is as of its last claim (the ledger keeps
            // no verdict); who is in the way of that is looked up now.
            let short = copy
                .as_ref()
                .filter(|c| c.state == LeaseState::Queued)
                .map(|c| c.short.clone())
                .unwrap_or_default();
            let behind = if short.is_empty() {
                let still = |t: &str| {
                    ledger.leases.iter().any(|l| l.task == t) || ahead.iter().any(|a| a.task == t)
                };
                copy.as_ref()
                    .and_then(|c| c.behind.clone())
                    .filter(|b| still(b))
                    .or_else(|| {
                        ledger
                            .leases
                            .iter()
                            .find(|l| l.slots.iter().any(|s| w.request.slots.contains(s)))
                            .map(|l| l.task.clone())
                    })
                    .or_else(|| ahead.last().map(|a| a.task.clone()))
            } else {
                // Which leases are building the pane does not know; a busy
                // CPU is then waited out behind the request ahead, if any.
                resources::in_the_way(
                    &ledger.leases,
                    ahead.iter().copied(),
                    &w.request,
                    &short,
                    &|_| false,
                )
            };
            row.lease = Some(RowLease {
                state: LeaseState::Queued,
                id: None,
                ram_mb: w.request.ram_mb,
                gpus: Vec::new(),
                vram_mb: w.request.vram_mb,
                slots: w.request.slots.clone(),
                granted_ms: None,
                wanted_ram_mb: None,
                position: Some(i as u64 + 1),
                behind,
                short,
                uncollected: false,
                lapsed: w.lapsed.is_some(),
            });
        }
    }
    f
}

/// An asked or leased size, as it was set ([`Mem`]): `3G`, `1.5G`,
/// `512M`, and `0` for nothing. Measured sizes are [`gb`].
fn mem(mb: u64) -> String {
    if mb == 0 {
        "0".into()
    } else {
        Mem(mb).to_string()
    }
}

/// A measured size in exactly four characters, for [`usage_cell`]: always
/// in G (T from a thousand G), `0.0G` for nothing, `0.1G` at the least for
/// anything, `4.2G`, ` 12G`, `999G`, `1.5T`. Tenths only below ten, so the
/// figure never grows a fifth character.
fn mem4(mb: u64) -> String {
    session_use::gb4(mb)
}

/// A measured size in G, unpadded: [`mem4`]'s figure, for the overlay
/// header's `peak 0.5G of 3G`.
fn gb(mb: u64) -> String {
    giverny_claude::session_use::gb(mb)
}

/// The OOM slot at the head of a [`usage_cell`].
const OOM_SLOT: &str = "OOM ";

/// The GPU slot at the tail of a [`usage_cell`] on a machine with a GPU:
/// `  ·  gpu 1.2G`.
const GPU_W: usize = 13;

/// How wide every [`usage_cell`] is: `OOM 100% CPU  ·  4.2G`, and
/// [`GPU_W`] more with a GPU.
#[cfg(test)]
fn usage_w(gpu: bool) -> usize {
    // `·` is two bytes and one column.
    let w = OOM_SLOT.len() + 8 + session_use::SEP.chars().count() + 4;
    if gpu { w + GPU_W } else { w }
}

/// A row's use in its cell: a Running row's commands now,
/// ` 14% CPU  ·  4.2G` (CPU as a share of the whole machine, so at most
/// `100%`), a Done row's memory peak alone in the same place, `4.2G`; on a
/// machine with a GPU (`gpu`), a Running row's GPU memory after,
/// `  ·  gpu 1.2G`. `OOM`
/// leads when the memory cap killed a run, so the figures keep their
/// places. Nothing measured reads as zero, `0.0G`, never as a blank: the
/// cell is always there. Every slot has a fixed width, so every cell is
/// as wide as every other and the column never jumps as the figures
/// change. The lease is not here: the overlay header says it
/// ([`lease_fact`]).
pub fn usage_cell(live: Option<RunLive>, peak_mb: Option<u64>, oom: bool, gpu: bool) -> String {
    let blank = |n: usize| " ".repeat(n);
    let sep = session_use::SEP;
    let oom = if oom {
        OOM_SLOT.to_string()
    } else {
        blank(OOM_SLOT.len())
    };
    let (cpu, memory) = match live {
        Some(l) => (
            format!("{}{sep}", session_use::cpu8(l.cpu_pct)),
            mem4(l.mem_mb),
        ),
        None => (blank(8 + sep.chars().count()), mem4(peak_mb.unwrap_or(0))),
    };
    let gpu = match live.and_then(|l| l.gpu_mb) {
        Some(g) if gpu => format!("{sep}gpu {}", mem4(g)),
        _ if gpu => blank(GPU_W),
        _ => String::new(),
    };
    format!("{oom}{cpu}{memory}{gpu}")
}

/// How heavy a run's live CPU and memory are on the whole machine of
/// `ram_mb`, as the rail weighs a tab's ([`sessions_load::machine_shares`]).
fn run_load(l: RunLive, ram_mb: u64) -> (Load, Load) {
    let u = giverny_claude::use_reading::Use {
        cpu_pct: l.cpu_pct,
        mem_mb: l.mem_mb,
        gpu_mb: None,
        gpu_pct: None,
    };
    let (cpu, mem) = sessions_load::machine_shares(&u, ram_mb);
    (Load::of(cpu), Load::of(mem))
}

/// A use cell as drawn, in runs of one colour: `(column in the cell, text,
/// colour)`. Everything is `ink`, the cell's own colour, but a Running
/// row's CPU and memory figures (`load`), each coloured by its weight on
/// the machine ([`Load::color`]): the same columns as [`usage_cell`]'s
/// slots, so nothing moves. The OOM tag, the separator and the GPU slot
/// keep `ink`.
fn usage_inks(
    cell: &str,
    load: Option<(Load, Load)>,
    ink: Color32,
    c: &Chrome,
) -> Vec<(usize, String, Color32)> {
    let chars: Vec<char> = cell.chars().collect();
    let piece = |a: usize, b: usize| -> String {
        chars[a.min(chars.len())..b.min(chars.len())]
            .iter()
            .collect()
    };
    let Some((cpu, mem)) = load else {
        return vec![(0, cell.to_string(), ink)];
    };
    let cpu_at = OOM_SLOT.len();
    let mem_at = cpu_at + 8 + session_use::SEP.chars().count();
    let runs = [
        (0, cpu_at, ink),
        (cpu_at, cpu_at + 8, cpu.color(ink, *c)),
        (cpu_at + 8, mem_at, ink),
        (mem_at, mem_at + 4, mem.color(ink, *c)),
        (mem_at + 4, chars.len(), ink),
    ];
    runs.into_iter()
        .map(|(a, b, color)| (a, piece(a, b), color))
        .filter(|(_, t, _)| !t.trim().is_empty())
        .collect()
}

/// The usage in full for the row's overlay header: `peak 2.1G of 3G, 45s
/// CPU, killed by the memory cap (OOM) x2`.
fn usage_fact(u: &RowUsage) -> String {
    let mut s = String::new();
    if let Some(p) = u.peak_mb.filter(|&p| p > 0) {
        s = format!("peak {}", gb(p));
        if let Some(cap) = u.cap_ram_mb.filter(|&c| c > 0) {
            s.push_str(&format!(" of {}", mem(cap)));
        }
    }
    if u.cpu_s > 0 {
        if !s.is_empty() {
            s.push_str(", ");
        }
        s.push_str(&match u.cpu_s {
            n if n < 60 => format!("{n}s CPU"),
            n => format!("{} CPU", feed::fmt_span(n as i64)),
        });
    }
    if u.oom_kills > 0 {
        if !s.is_empty() {
            s.push_str(", ");
        }
        s.push_str(&format!("OOM-killed x{}", u.oom_kills));
        // A later clean run or a bigger grant closed it (giverny#452).
        if !u.oom_open {
            s.push_str(" earlier");
        }
    }
    s
}

/// A Next up row waiting in the ledger, in its NOW cell: `queued for 3G
/// behind 899x`, `queued for CPU` — what it is short of, in its own sizes
/// ([`resources::short_need`], as `claim` says it), and who is really in
/// the way. Nothing named for a lease whose shortage is not known.
pub fn queued_note(l: &RowLease) -> String {
    let need = resources::short_need(&l.short, l.ram_mb, l.vram_mb);
    let mut s = "queued".to_string();
    if l.lapsed {
        // giverny#429: its grant went untaken; nothing is held for it now.
        s.push_str(", grant lapsed, waiting for its manager");
        return s;
    }
    if !need.is_empty() {
        s.push_str(&format!(" for {need}"));
    }
    if let Some(b) = &l.behind {
        s.push_str(&format!(" behind {b}"));
    }
    s
}

/// [`queued_note`] in the row's hover (giverny#423): `queued — waiting for
/// 3G behind 899x in the machine ledger`.
fn queued_why(l: &RowLease) -> String {
    if l.lapsed {
        return "queued — its grant lapsed untaken; waiting for its manager".into();
    }
    let need = resources::short_need(&l.short, l.ram_mb, l.vram_mb);
    let mut s = "queued — waiting".to_string();
    if !need.is_empty() {
        s.push_str(&format!(" for {need}"));
    }
    if let Some(b) = &l.behind {
        s.push_str(&format!(" behind {b}"));
    }
    s.push_str(" in the machine ledger");
    s
}

/// What a row whose lease was granted on another session's call, and not
/// yet taken up, waits on (giverny#429).
pub const GRANTED_WAITING: &str = "granted, waiting for its manager";

/// The lease in full for the row's overlay header: `holds 3G, slot
/// cargo:/x`, or `queued #2 for …`.
fn lease_fact(l: &RowLease) -> String {
    match l.state {
        LeaseState::Queued => {
            let note = queued_note(l);
            match l.position {
                Some(p) => note.replacen("queued", &format!("queued #{p}"), 1),
                None => note,
            }
        }
        _ => {
            let mut s = format!(
                "holds {}",
                resources::describe_held(l.ram_mb, &l.gpus, l.vram_mb, &l.slots)
            );
            if l.state == LeaseState::Smaller
                && let Some(w) = l.wanted_ram_mb
            {
                s.push_str(&format!(" (asked {})", mem(w)));
            }
            if l.uncollected {
                s.push_str(&format!("; {GRANTED_WAITING}"));
            }
            s
        }
    }
}

// ------------------------------------------------------------- clicks ----

/// What a click on a row names — everything the action behind it
/// ([`crate::agent_open`]) could want, so deciding what to do never needs the pane again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowClick {
    pub stage: Stage,
    /// The feed's key (`acme#158`), else empty.
    pub key: String,
    /// The Claude Code subagent id, when a worker holds the row.
    pub agent_id: Option<String>,
    /// A Running row whose worker has finished — handed back, failed,
    /// killed or stopped — and nobody has landed it (NOW says `worker
    /// finished — not landed`). Claude Code has dropped that worker from
    /// its agent list, so there is no view to walk into: a click opens its
    /// transcript as a Done row's does (giverny#274).
    pub finished: bool,
    /// That finished worker is asleep on background shells of its own
    /// (giverny#476): Claude Code wakes it when one ends, so its transcript
    /// is not final and is followed as it grows.
    pub asleep: bool,
    /// The worker's name or description, for a tab title.
    pub name: String,
    /// The worker's `agent-<id>.jsonl`, once known.
    pub transcript: Option<PathBuf>,
    /// The feed's `open` command (Running/Done).
    pub open: Option<String>,
    /// The feed's `brief` (Planned).
    pub brief: Option<PathBuf>,
    /// The feed's `note`.
    pub note: Option<String>,
    /// The feed's `review` line (Done): shown at the overlay's top as is.
    pub review: Option<String>,
    /// The row's state in words, for the overlay's header: how it landed,
    /// how long it took against its estimate, its tokens.
    pub facts: Vec<String>,
}

impl RowClick {
    /// Whether `other` is a click on the same row: the feed's key when
    /// there is one, else the worker's id, else its name. The stage is left
    /// out — a row viewed while Running is still that row once Done.
    pub fn same_row(&self, other: &RowClick) -> bool {
        if !self.key.is_empty() || !other.key.is_empty() {
            return self.key == other.key;
        }
        if self.agent_id.is_some() || other.agent_id.is_some() {
            return self.agent_id == other.agent_id;
        }
        self.name == other.name
    }
}

/// The worker a tab shows: in Claude Code's own view, by its id; on
/// Giverny's page (`worker_page`), by its row (giverny#477), which a Next up
/// row with no worker yet is known by.
#[derive(Debug, Clone, Copy)]
pub enum Shown<'a> {
    Worker(&'a str),
    Row(&'a RowClick),
}

impl Shown<'_> {
    /// Whether `click` is the row of the worker shown.
    pub fn is(&self, click: &RowClick) -> bool {
        match self {
            Shown::Worker(id) => click.agent_id.as_deref() == Some(*id),
            Shown::Row(row) => row.same_row(click),
        }
    }
}

/// What a row click does: a click on the row the tab is
/// already showing takes it back to the manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    /// Open the row: its worker's view, its overlay, its brief.
    Open,
    /// The tab shows (or is walking to) this row's worker: walk it home.
    Home,
    /// This row's overlay is up over the tab: close it.
    Close,
}

/// Decide a click on `click`, given the worker id the tab shows or is on
/// its way to (`viewed`) and the row whose overlay is up (`overlay`).
pub fn toggle(click: &RowClick, viewed: Option<&str>, overlay: Option<&RowClick>) -> Toggle {
    if overlay.is_some_and(|o| o.same_row(click)) {
        return Toggle::Close;
    }
    if viewed.is_some() && click.agent_id.as_deref() == viewed {
        return Toggle::Home;
    }
    Toggle::Open
}

// -------------------------------------------------------------- table ----

/// One drawn row, every cell already formatted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub stage: Stage,
    pub id: String,
    pub title: String,
    pub elapsed: String,
    pub eta: String,
    /// A Running row with no estimate: `eta` is empty, and the cell is
    /// drawn as a dim [`NO_ETA`].
    pub no_eta: bool,
    pub now: String,
    pub tokens: String,
    /// What the row's commands use ([`usage_cell`]): live on a Running row
    /// (zero between its commands), the memory peak on a Done one (zero when
    /// nothing was measured); empty on a Next up row.
    pub usage: String,
    /// How heavy a Running row's live CPU and memory are on the whole
    /// machine ([`Load`]), each drawn in its own colour over the cell's
    /// ([`usage_inks`]); `None` on any other row, whose cell is one colour.
    pub usage_load: Option<(Load, Load)>,
    /// A `giverny manage run` of the row was killed by its memory cap: the
    /// lease cell is drawn in the warning colour.
    pub oom: bool,
    /// A Running row the person has to know about ([`unworked`]): its
    /// worker stopped on an API error, or it has none. NOW says which, in
    /// the warning colour. A worker that finished is its manager's to land,
    /// not the person's ([`chores`]): its row is drawn as any Running row.
    pub flag: bool,
    /// When a Done row landed (epoch ms), when known; `None` on any other
    /// row.
    pub landed_ms: Option<u64>,
    /// The hover's lines under the title (giverny#423): where the row is
    /// and why, in words ([`running_why`], [`Wait::why`], [`done_why`]).
    pub why: Vec<String>,
    /// How deep under another row it hangs (giverny#464): 0 for a row of
    /// its own, 1 for an agent a worker spawned, 2 for one that agent
    /// spawned. Its title is drawn indented a step per level.
    pub depth: usize,
    /// The row's agent spawned agents of its own, drawn under it: whether
    /// they are folded away and how many there are.
    pub fold: Option<Fold>,
    pub click: RowClick,
}

/// A row whose agent spawned agents of its own (giverny#464): the toggle
/// on its row folds them away under it or shows them again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fold {
    /// What the pane keeps the fold by ([`fold_ident`]).
    pub ident: String,
    /// They are folded away.
    pub folded: bool,
    /// How many rows hang under it, every level down.
    pub under: usize,
    /// How many of those still run.
    pub running: usize,
}

impl Fold {
    /// The toggle drawn at the head of the title: `▸` while folded,
    /// `▾` while open.
    pub fn glyph(&self) -> char {
        if self.folded { '▸' } else { '▾' }
    }

    /// What a folded row says of what it hides: `+6`, `+6, 2 running`.
    /// Nothing while open.
    pub fn note(&self) -> String {
        if !self.folded {
            return String::new();
        }
        match self.running {
            0 => format!("+{}", self.under),
            r => format!("+{}, {r} running", self.under),
        }
    }

    /// [`Fold::note`] for a narrow row: just `+6`.
    pub fn short_note(&self) -> String {
        if self.folded {
            format!("+{}", self.under)
        } else {
            String::new()
        }
    }
}

impl Line {
    /// The row's tooltip: its whole title, then [`Line::why`].
    pub fn tip(&self) -> String {
        let head = if self.title.is_empty() {
            &self.id
        } else {
            &self.title
        };
        std::iter::once(head.as_str())
            .chain(self.why.iter().map(String::as_str))
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The whole table: rows and the feed's footer line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Table {
    pub lines: Vec<Line>,
    pub footer: Option<String>,
    /// Some row has agents hung under it ([`Line::fold`]): every
    /// top-level title leaves room for the toggle, so they stay aligned.
    pub nested: bool,
}

impl Table {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
    fn any_running(&self) -> bool {
        self.lines.iter().any(|l| l.stage == Stage::Running)
    }
}

/// Merge and format with no limit ever seen. Pure: `now_ms` in, text
/// out.
#[cfg(any(test, debug_assertions))]
pub fn build(feed: Option<&Feed>, live: &[SubagentRow], now_ms: u64) -> Table {
    build_at(feed, live, now_ms, &Clock::plain(), &Logs::new())
}

/// [`build`], with the clocks held through `clock`'s usage-limit spans and
/// each worker's transcript ([`Logs`]) to give a worker that took tasks one
/// after another a row per task.
#[cfg(any(test, debug_assertions))]
pub fn build_at(
    feed: Option<&Feed>,
    live: &[SubagentRow],
    now_ms: u64,
    clock: &Clock,
    logs: &Logs,
) -> Table {
    build_with(
        feed,
        live,
        now_ms,
        clock,
        logs,
        DoneRows::All,
        Drawn::Every,
        &HashSet::new(),
    )
}

/// [`build_at`], showing only the Done rows `done` keeps
/// (`management_panel.done_rows`) and the rows `drawn` keeps (giverny#357).
/// Rows are dropped before they are formatted, so a `"` never stands for a
/// row that is not drawn.
///
/// An agent another drawn agent spawned hangs under that agent's row
/// ([`nest`], giverny#464), unless that row is in `folded`.
#[allow(clippy::too_many_arguments)]
pub fn build_with(
    feed: Option<&Feed>,
    live: &[SubagentRow],
    now_ms: u64,
    clock: &Clock,
    logs: &Logs,
    done: DoneRows,
    drawn: Drawn,
    folded: &HashSet<String>,
) -> Table {
    with_rows(feed, live, logs, now_ms, |feed, rows| {
        // What a Next up row waits for is read off every row, drawn or not:
        // the task it was planned after may have no worker yet.
        let waits: Vec<Option<Wait>> = rows.iter().map(|r| planned_wait(r, &rows)).collect();
        let order = start_order(&rows, &waits);
        let mut kept: Vec<_> = rows
            .into_iter()
            .zip(waits)
            .zip(order)
            .filter(|((r, _), _)| drawn.keeps(r))
            .collect();
        // Next up in the order its rows will start, the next on top
        // (giverny#442); Running and Done as they were.
        kept.sort_by_key(|(_, o)| *o);
        let (rows, waits): (Vec<_>, Vec<_>) = kept.into_iter().map(|(rw, _)| rw).unzip();
        let nested = nest(&rows, done, folded);
        let mut slots: Vec<Option<_>> = rows.into_iter().zip(waits).map(Some).collect();
        let (mut rows, waits): (Vec<_>, Vec<_>) =
            nested.iter().filter_map(|n| slots[n.at].take()).unzip();
        reditto(&mut rows);
        let written = feed.and_then(|f| f.written_ms);
        let mut lines: Vec<Line> = Vec::with_capacity(rows.len());
        for ((row, wait), n) in rows.iter().zip(waits).zip(&nested) {
            let mut line = format_row(row, now_ms, written, clock, wait);
            line.depth = n.depth;
            line.fold = n.fold.clone();
            if let Some(f) = &n.fold {
                line.why.push(fold_why(f));
            }
            if let Some(p) = &n.parent {
                line.why.push(format!("spawned by {p}"));
            }
            lines.push(line);
        }
        dittos(&rows, &mut lines);
        Table {
            nested: lines.iter().any(|l| l.fold.is_some()),
            lines,
            footer: feed.and_then(|f| f.footer.clone()),
        }
    })
}

/// One row as [`nest`] places it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Nested {
    /// Its place among the rows handed to [`nest`].
    at: usize,
    depth: usize,
    fold: Option<Fold>,
    /// The row it hangs under, by name, for its hover.
    parent: Option<String>,
}

/// What a row's fold is kept by: its feed key, else its agent's id — as
/// [`done_ident`] knows a row, so a worker's fold holds from Running to
/// Done.
fn fold_ident(row: &PaneRow<'_, SubagentRow>) -> String {
    match (
        row.feed.map(|f| f.key.as_str()).filter(|k| !k.is_empty()),
        row.agent_id(),
    ) {
        (Some(k), _) => format!("k:{k}"),
        (None, Some(a)) => format!("a:{a}"),
        (None, None) => String::new(),
    }
}

/// A row by name, for a hover: its feed key, else its agent's name.
fn row_name(row: &PaneRow<'_, SubagentRow>) -> String {
    row.feed
        .map(|f| f.key.clone())
        .filter(|k| !k.is_empty())
        .or_else(|| row.live.map(|l| l.display_name().to_string()))
        .unwrap_or_default()
}

/// The row each row hangs under, by place: the drawn row of the agent that
/// spawned it ([`SubagentRow::parent_id`]). A worker that holds several
/// rows (tasks one after another) takes it under the one whose task was
/// under way when it spawned it — the latest to start no later — else its
/// first. An agent whose spawner holds no row stands on its own, and so
/// does one caught in a loop of parents (never seen; never trusted).
fn parent_rows(rows: &[PaneRow<'_, SubagentRow>]) -> Vec<Option<usize>> {
    let mut up: Vec<Option<usize>> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let l = r.live?;
            let p = l.parent_id.as_deref()?;
            let holds = |(j, o): &(usize, &PaneRow<'_, SubagentRow>)| {
                *j != i && o.stage != Stage::Planned && o.agent_id() == Some(p)
            };
            let under_way = rows
                .iter()
                .enumerate()
                .filter(holds)
                .filter(|(_, o)| match (o.started_ms(), l.started_ms) {
                    (Some(a), Some(b)) => a <= b,
                    _ => false,
                })
                .max_by_key(|(_, o)| o.started_ms());
            under_way
                .or_else(|| rows.iter().enumerate().find(holds))
                .map(|(j, _)| j)
        })
        .collect();
    for i in 0..up.len() {
        let mut at = up[i];
        let mut steps = 0;
        while let Some(j) = at {
            if j == i || steps > up.len() {
                up[i] = None;
                break;
            }
            at = up[j];
            steps += 1;
        }
    }
    up
}

/// Place the rows as the pane draws them (giverny#464): each agent another
/// drawn agent spawned right under its spawner's row, a level deeper, in
/// the order the rows came; its spawner's Running or Done row carries it
/// wherever that row is. A row in `folded` hides everything under it and
/// says how much ([`Fold`]).
///
/// `done` keeps the newest Done rows of their own (`Last(n)` counts only
/// those) and what hangs under them. What hung under a Done row that is
/// not kept goes with it, but an agent still running there stands on its
/// own instead: a running agent is never hidden by the Done setting.
/// `Hide` hides every Done row, nested or not.
fn nest(
    rows: &[PaneRow<'_, SubagentRow>],
    done: DoneRows,
    folded: &HashSet<String>,
) -> Vec<Nested> {
    let n = rows.len();
    let mut up = parent_rows(rows);
    let level = |up: &[Option<usize>], i: usize| {
        let (mut d, mut at) = (0, up[i]);
        while let Some(j) = at {
            d += 1;
            at = up[j];
        }
        d
    };
    let mut dropped = vec![false; n];
    if done != DoneRows::All {
        let keep = match done {
            DoneRows::Last(k) => k,
            _ => 0,
        };
        let mut seen = 0;
        for i in 0..n {
            if rows[i].stage != Stage::Done {
                continue;
            }
            if done == DoneRows::Hide {
                dropped[i] = true;
            } else if up[i].is_none() {
                seen += 1;
                dropped[i] = seen > keep;
            }
        }
        // Shallowest first, so a row's spawner is settled before it is.
        let mut by_level: Vec<usize> = (0..n).collect();
        let levels: Vec<usize> = (0..n).map(|i| level(&up, i)).collect();
        by_level.sort_by_key(|&i| levels[i]);
        for i in by_level {
            if let Some(p) = up[i]
                && dropped[p]
            {
                if rows[i].stage == Stage::Done {
                    dropped[i] = true;
                } else {
                    up[i] = None;
                }
            }
        }
    }
    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in (0..n).filter(|&i| !dropped[i]) {
        if let Some(p) = up[i] {
            kids[p].push(i);
        }
    }
    // Everything under a row, every level down, and how much of it runs.
    fn under(i: usize, kids: &[Vec<usize>], rows: &[PaneRow<'_, SubagentRow>]) -> (usize, usize) {
        kids[i].iter().fold((0, 0), |(all, run), &k| {
            let (a, r) = under(k, kids, rows);
            let me = usize::from(rows[k].stage == Stage::Running);
            (all + 1 + a, run + me + r)
        })
    }
    #[allow(clippy::too_many_arguments)]
    fn place(
        i: usize,
        depth: usize,
        parent: Option<String>,
        kids: &[Vec<usize>],
        rows: &[PaneRow<'_, SubagentRow>],
        folded: &HashSet<String>,
        out: &mut Vec<Nested>,
    ) {
        let fold = (!kids[i].is_empty()).then(|| {
            let (under, running) = under(i, kids, rows);
            let ident = fold_ident(&rows[i]);
            Fold {
                folded: folded.contains(&ident),
                ident,
                under,
                running,
            }
        });
        let open = fold.as_ref().is_none_or(|f| !f.folded);
        out.push(Nested {
            at: i,
            depth,
            fold,
            parent,
        });
        if open {
            for &k in &kids[i] {
                place(
                    k,
                    depth + 1,
                    Some(row_name(&rows[i])),
                    kids,
                    rows,
                    folded,
                    out,
                );
            }
        }
    }
    let mut out = Vec::with_capacity(n);
    for i in (0..n).filter(|&i| !dropped[i] && up[i].is_none()) {
        place(i, 0, None, &kids, rows, folded, &mut out);
    }
    out
}

/// A row with agents under it, in its hover: how many, how many run, and
/// what the toggle does.
fn fold_why(f: &Fold) -> String {
    let agents = if f.under == 1 { "agent" } else { "agents" };
    let running = match f.running {
        0 => String::new(),
        r => format!(", {r} running"),
    };
    if f.folded {
        format!(
            "{} {agents} of its own folded under it{running} — ▸ shows them",
            f.under
        )
    } else {
        format!(
            "{} {agents} of its own under it{running} — ▾ folds them",
            f.under
        )
    }
}

/// The pane's rows, as it places them at `now_ms`: the feed merged with
/// the live rows, and the Running rows nobody works on moved to Next up
/// ([`benched`]): every row, drawn or not ([`Drawn`] says which are). A
/// row is a worker's only by the feed (`start <task> --agent <worker>`) or
/// the worker's spawn: no message to a worker hands it a row
/// (giverny#485). `f` gets the feed it was merged from.
fn with_rows<T>(
    feed: Option<&Feed>,
    live: &[SubagentRow],
    logs: &Logs,
    now_ms: u64,
    f: impl FnOnce(Option<&Feed>, Vec<PaneRow<'_, SubagentRow>>) -> T,
) -> T {
    let log = |id: &str| logs.get(id);
    let mut rows = feed::merge_with(feed, live, log);
    bench(&mut rows, now_ms);
    f(feed, rows)
}

/// A Running row nobody works on (giverny#334): no worker at all once a
/// spawn would have come ([`Unworked::NoWorker`]); one its manager paused
/// (or held) with no worker on it, at once; or a worker that handed it back
/// and a row its manager paused or held rather than landed. It is
/// drawn under Next up, its NOW saying why ([`planned_wait`]): `held: …`,
/// `paused since …`, [`NO_WORKER_YET`]. A worker stopped on an API error, or
/// one that failed or was killed, keeps its row Running, for the person to
/// see. The feed still says Running: a worker picking it up moves it back.
fn benched(row: &PaneRow<'_, SubagentRow>, now_ms: u64) -> bool {
    let Some(f) = row.feed.filter(|_| row.stage == Stage::Running) else {
        return false;
    };
    let set_aside = f.held.is_some() || f.paused_since_ms.is_some();
    match unworked(row, now_ms) {
        Some(Unworked::NoWorker { .. }) => true,
        Some(Unworked::Finished {
            how: "finished", ..
        }) => set_aside,
        // Just started or handed on, no worker on it yet: set aside, none
        // is to come.
        None => set_aside && f.live.is_none() && row.live.is_none_or(|l| !l.running()),
        _ => false,
    }
}

/// Move the [`benched`] rows to Next up, keeping every other row where it
/// was, and ditto again ([`PaneRow::ditto`]) where rows moved. Where in Next
/// up a benched row is drawn is its own wait's call ([`start_order`]).
fn bench(rows: &mut [PaneRow<'_, SubagentRow>], now_ms: u64) {
    let mut moved = false;
    for row in rows.iter_mut() {
        if benched(row, now_ms) {
            row.stage = Stage::Planned;
            moved = true;
        }
    }
    if !moved {
        return;
    }
    // Stable: every other row keeps its place.
    rows.sort_by_key(|r| r.stage);
    reditto(rows);
}

/// Ditto again ([`PaneRow::ditto`]) where rows moved or went: a row is a
/// ditto of the one now directly above it, the same worker's same batch.
fn reditto(rows: &mut [PaneRow<'_, SubagentRow>]) {
    for i in 0..rows.len() {
        let same = i > 0
            && match (rows[i - 1].agent_id(), rows[i].agent_id()) {
                (Some(a), Some(b)) => a == b && rows[i - 1].batch == rows[i].batch,
                _ => false,
            };
        rows[i].ditto = same;
    }
}

/// The chores of [`chores`] that are due at `now_ms` and that the feed's
/// rows were not told of yet: what the manager is to hear.
pub fn due_chores(
    drawn: Drawn,
    feed: &Feed,
    live: &[SubagentRow],
    logs: &Logs,
    shells: &Shells,
    now_ms: u64,
) -> Vec<Chore> {
    chores(drawn, Some(feed), live, logs, shells, now_ms)
        .into_iter()
        .filter(|c| c.is_due(now_ms))
        .filter(|c| !feed.rows.iter().any(|r| r.key == c.key && c.told(r)))
        .collect()
}

/// What the rows ask of their manager rather than of the person
/// (giverny#310): a Running row whose worker has finished, unlanded
/// ([`Unworked::Finished`]), drawn under Next up when its manager paused it
/// (giverny#379); a Next up row waiting on nothing recorded
/// ([`Wait::idle_since`]); and a Running row with no worker at all, drawn
/// under Next up ([`Unworked::NoWorker`], giverny#334) unless its manager
/// paused or held it — the very rows the pane once drew in amber, from the
/// same logic. Every one, due or not, told or not:
/// [`manage_chores::Chore::is_due`] and [`manage_chores::Chore::told`] say
/// which to keep.
///
/// Only of the rows the pane draws (`drawn`, giverny#357): nothing drawn,
/// nothing to nag about. A session that is no manager ([`Drawn::Agents`])
/// never draws a row with no worker, so it never hears "no worker"; a
/// manager still does, as its Next up draws such a row: one that ran
/// `start` and forgot to spawn is the one it catches.
pub fn chores(
    drawn: Drawn,
    feed: Option<&Feed>,
    live: &[SubagentRow],
    logs: &Logs,
    shells: &Shells,
    now_ms: u64,
) -> Vec<Chore> {
    with_rows(feed, live, logs, now_ms, |_, rows| {
        let mut out = Vec::new();
        for row in rows.iter().filter(|r| drawn.keeps(r)) {
            let Some(key) = row.feed.map(|f| f.key.clone()) else {
                continue;
            };
            let chore = match row.stage {
                // Asleep on its own shell: Claude Code wakes it, nothing to
                // land (giverny#476).
                Stage::Running if asleep_on(row, shells, now_ms).is_some() => None,
                Stage::Running => match unworked(row, now_ms) {
                    Some(Unworked::Finished { how, ended_ms }) => ended_ms.map(|since| Chore {
                        key,
                        kind: manage_chores::Kind::Land,
                        since,
                        how: Some(how.to_string()),
                    }),
                    _ => None,
                },
                // A Running row benched for want of a worker: spawn or hand
                // it one, or set it aside. One its manager paused or held is
                // set aside already. A paused one whose worker finished,
                // before the pause or during it, is set aside with its work
                // done: land it or resume it (giverny#379).
                Stage::Planned if row.feed.is_some_and(|f| f.stage() == Stage::Running) => {
                    let paused = row.feed.is_some_and(|f| f.paused_since_ms.is_some());
                    let set_aside = paused || row.feed.is_some_and(|f| f.held.is_some());
                    match unworked(row, now_ms) {
                        Some(Unworked::NoWorker { since }) if !set_aside => Some(Chore {
                            key,
                            kind: manage_chores::Kind::Worker,
                            since,
                            how: None,
                        }),
                        Some(Unworked::Finished { how, ended_ms }) if paused => {
                            ended_ms.map(|since| Chore {
                                key,
                                kind: manage_chores::Kind::LandPaused,
                                since,
                                how: Some(how.to_string()),
                            })
                        }
                        _ => None,
                    }
                }
                Stage::Planned => {
                    planned_wait(row, &rows)
                        .and_then(|w| w.idle_since)
                        .map(|since| Chore {
                            key,
                            kind: manage_chores::Kind::Start,
                            since,
                            how: None,
                        })
                }
                Stage::Done => None,
            };
            out.extend(chore);
        }
        out
    })
}

/// A cell is `"` when its row is the same worker as the row above
/// ([`PaneRow::ditto`]) and its text is that row's text — compared before
/// either was replaced, so a run of three dittos all the way down.
fn dittos(rows: &[PaneRow<'_, SubagentRow>], lines: &mut [Line]) {
    let raw: Vec<[String; 4]> = lines
        .iter()
        .map(|l| {
            [
                l.elapsed.clone(),
                l.eta.clone(),
                l.now.clone(),
                l.tokens.clone(),
            ]
        })
        .collect();
    for i in 1..lines.len() {
        if !rows[i].ditto {
            continue;
        }
        let out = &mut lines[i];
        let cells = [
            &mut out.elapsed,
            &mut out.eta,
            &mut out.now,
            &mut out.tokens,
        ];
        for (k, cell) in cells.into_iter().enumerate() {
            if !raw[i][k].is_empty() && raw[i][k] == raw[i - 1][k] {
                *cell = "\"".into();
            }
        }
    }
}

/// Where a Running row's clock stands: `now`, or for a row paused in the
/// feed, the instant its `started` is true as of — the feed's write time
/// (a writer that pauses moves `started` on by the open pause up to then), never before the
/// pause began nor after `now`.
fn clock_stop(paused_since: Option<u64>, written: Option<u64>, now_ms: u64) -> u64 {
    match paused_since {
        Some(p) => written.map_or(p, |w| w.max(p)).min(now_ms),
        None => now_ms,
    }
}

fn format_row(
    row: &PaneRow<'_, SubagentRow>,
    now_ms: u64,
    written: Option<u64>,
    clock: &Clock,
    wait: Option<Wait>,
) -> Line {
    let f = row.feed;
    let l = row.live;
    let key = f.map(|f| f.key.clone()).unwrap_or_default();
    // A feed row's key and title; a live-only row's name and description.
    let (id, title) = match (f, l) {
        (Some(f), _) => (
            Some(shown_key(f))
                .filter(|k| !k.is_empty())
                .or_else(|| l.and_then(|l| l.model.as_deref()).and_then(short_model))
                .unwrap_or_default(),
            f.title
                .clone()
                .or_else(|| l.map(|l| l.display_name().to_string()))
                .unwrap_or_default(),
        ),
        (None, Some(l)) => match (&l.name, &l.description) {
            (Some(n), Some(d)) => (n.clone(), d.clone()),
            _ => (
                l.model.as_deref().and_then(short_model).unwrap_or_default(),
                l.display_name().to_string(),
            ),
        },
        (None, None) => (String::new(), String::new()),
    };
    // This row's own start: the feed's (a worker holding several tasks
    // started each at a different time, and a pausing writer moves it on by the row's
    // pauses), else the worker's.
    let row_start = row.started_ms();
    let paused = row.paused_since_ms();
    // A Running row drawn under Next up ([`benched`]) that its manager paused.
    let benched_pause = f
        .filter(|f| row.stage == Stage::Planned && f.stage() == Stage::Running)
        .and_then(|f| f.paused_since_ms);
    // A feed row that carries its pauses has its stops taken off `started`
    // already (a writer may also pause rows for a limit itself): the
    // pane's own holds would take them off twice.
    let writer_pauses = f.is_some_and(|f| f.paused_s.is_some() || f.paused_since_ms.is_some());
    let holds = if writer_pauses { &[][..] } else { clock.holds };
    let slept_ms = |s: u64, e: u64| sleeps::slept_ms(clock.sleeps, s, e);
    // A Running row's work so far, in seconds: wall time up to where its
    // clock stands, less every span the account was out of its limit or
    // the machine slept.
    // A worker stopped on an API error, or ended badly under a Running
    // row, stands still from then, and its error spans are off its clock.
    let stopped = l
        .filter(|_| row.stage == Stage::Running)
        .and_then(|l| l.stopped());
    let error_ms = |s: u64, e: u64| l.map_or(0, |l| l.stopped_ms(s, e));
    // Where a Running row's clock stands now.
    let stop = match (row.stage, row_start) {
        (Stage::Running, Some(s)) => {
            let mut stop = clock_stop(paused, written, now_ms);
            if let Some((since, _)) = &stopped {
                stop = stop.min((*since).max(s));
            }
            Some(stop)
        }
        _ => None,
    };
    // The work in `[from, stop]`, in seconds: its wall time less every span
    // the account was out of its limit, the machine slept or the worker
    // stood on an error.
    let work_since = |from: u64, stop: u64| {
        let from = from.min(stop);
        stop.saturating_sub(from)
            .saturating_sub(off_ms(holds, clock.sleeps, from, stop))
            .saturating_sub(error_ms(from, stop))
            / 1000
    };
    let work_s = row_start.zip(stop).map(|(s, stop)| work_since(s, stop));
    let elapsed = match row.stage {
        Stage::Planned => String::new(),
        Stage::Running => work_s.map(stopwatch).unwrap_or_default(),
        Stage::Done => {
            let end = row.ended_ms();
            match (row_start, end) {
                (Some(s), Some(e)) => stopwatch(
                    e.saturating_sub(s)
                        .saturating_sub(error_ms(s, e))
                        .saturating_sub(slept_ms(s, e))
                        / 1000,
                ),
                _ => String::new(),
            }
        }
    };
    // A worker no feed row carries: its agent ETA, counted from its start.
    let agent = l
        .filter(|_| f.is_none())
        .and_then(|l| clock.etas.get(&l.id));
    let agent_eta = agent.zip(row_start).map(|(e, s)| e.total_s(s));
    let eta_s = f.and_then(|f| f.eta_s).or(agent_eta);
    // The latest figure given mid-run, and when: a re-estimated feed row's,
    // else the agent ETA's, never before the row's clock starts.
    let figure = row
        .last_figure()
        .or_else(|| agent.map(|e| (row_start.map_or(e.at_ms, |s| e.at_ms.max(s)), e.left_s)));
    // A Done row's ± and the figure it was measured against (giverny#402):
    // the feed row's ([`feed::PaneRow::eta_delta_s`]), else an agent ETA's
    // first figure, by the work since it. Its later figures are the
    // worker's, but which came first from it is not kept.
    let delta = match row.stage {
        Stage::Done => row.eta_delta_s(&slept_ms).or_else(|| {
            let e = agent?;
            let at = row_start.map_or(e.first_at_ms, |s| e.first_at_ms.max(s));
            let end = row.ended_ms()?.max(at);
            let work_s = (end - at).saturating_sub(slept_ms(at, end)) / 1000;
            feed::done_delta(work_s, None, Some(e.first_left_s))
        }),
        _ => None,
    };
    // Re-estimated: the worker's latest figure less the work since it
    // was given (giverny#402). Off-clock spans before it are in neither,
    // those after still come off; `eta_s` less the work since the start
    // took the ones before it off twice, being wall time up to the figure.
    let left_s = match (row.stage, figure, stop, eta_s, work_s) {
        (Stage::Running, Some((at, left)), Some(stop), _, _) => {
            Some(left as i64 - work_since(at, stop) as i64)
        }
        (Stage::Running, _, _, Some(eta), Some(work)) => Some(eta as i64 - work as i64),
        _ => None,
    };
    let eta = match row.stage {
        Stage::Running => left_s.map(countdown).unwrap_or_default(),
        Stage::Planned => eta_s
            .map(|e| format!("~{}", feed::fmt_span(e as i64)))
            .unwrap_or_default(),
        Stage::Done => delta.map(|(d, _)| feed::fmt_delta(d)).unwrap_or_default(),
    };
    let no_eta = row.stage == Stage::Running && eta_s.is_none();
    // What the task holds in the machine ledger is in the overlay header; a
    // Next up row that waits for it says so in NOW.
    let held = f
        .and_then(|f| f.lease.as_ref())
        .filter(|_| row.stage != Stage::Done);
    // The cell is the row's use: a Running row's commands now,
    // a Done row's memory peak alone.
    let measured = f.and_then(|f| f.usage.as_ref());
    let oom = measured.is_some_and(|u| u.oom_open) && row.stage != Stage::Planned;
    // Running and Done rows always have a cell, zero when nothing is going
    // or was measured; Next up rows say what they wait for in NOW instead.
    let peak = measured.and_then(|u| u.peak_mb);
    let gpu = session_use::gpu::present();
    let live = (row.stage == Stage::Running).then(|| {
        f.and_then(|f| f.live)
            .or_else(|| row.agent_id().and_then(|id| clock.workers.get(id)).copied())
            .unwrap_or_default()
    });
    let usage = match row.stage {
        Stage::Running => usage_cell(live, None, oom, gpu),
        Stage::Done => usage_cell(None, peak, oom, gpu),
        Stage::Planned => String::new(),
    };
    let usage_load = live.map(|l| run_load(l, sessions_load::machine_ram_mb()));
    let limit = clock.limit.filter(|l| l.out_at(now_ms));
    // Only what the person has to know is flagged: a worker stopped on an
    // API error, a row nothing runs. A finished worker's row and an idle
    // Next up row are their manager's ([`chores`]), drawn plain.
    // The worker's open wait (`eta --why wait`) and what it said of it.
    let waiting = f
        .filter(|_| row.stage == Stage::Running)
        .and_then(|f| f.waiting_since_ms.map(|w| (w, f.note.as_deref())));
    let flag = match row.stage {
        Stage::Running => unworked(row, now_ms).and_then(Unworked::flag),
        Stage::Planned | Stage::Done => None,
    };
    // Its worker's turn ended with shells of its own still running: asleep
    // (giverny#476).
    let asleep = (row.stage == Stage::Running)
        .then(|| asleep_on(row, clock.shells, now_ms))
        .flatten();
    let now = match row.stage {
        // What is not running comes first: a Running row whose worker
        // stopped on an error, or that has none, says so before anything
        // else.
        Stage::Running if flag.is_some() => flag.clone().unwrap_or_default(),
        // The limit next: a row the writer paused for it says why.
        Stage::Running => match (limit, paused) {
            (Some(limit), _) => limit_note(limit, now_ms, &clock.tz),
            (None, Some(p)) => format!("paused since {}", clock_at(p, now_ms, &clock.tz)),
            (None, None) if stopped.is_some() => stopped.map(|(_, why)| why).unwrap_or_default(),
            // Its worker ended its turn: when, and the wait it marked, so
            // the row does not read as work going on (giverny#471).
            (None, None) => match unworked(row, now_ms) {
                // Asleep on its own work, not ended (giverny#476).
                Some(Unworked::Finished { .. }) if asleep.is_some() => {
                    asleep_now(asleep.unwrap_or_default(), waiting, now_ms)
                }
                Some(Unworked::Finished { ended_ms, .. }) => {
                    turn_ended(ended_ms, waiting, now_ms, &clock.tz)
                }
                _ => l
                    .filter(|l| l.running())
                    .and_then(|l| l.activity.clone())
                    .unwrap_or_default(),
            },
        },
        // What it waits for ([`planned_wait`]); nothing when it waits on
        // nothing recorded. A Running row benched while paused: since when,
        // or that its worker finished, when it has (giverny#379): `paused
        // since …` alone read as work not yet begun, when it was work done
        // and waiting to be landed.
        Stage::Planned => {
            match benched_pause.filter(|_| wait.as_ref().is_none_or(|w| w.held.is_none())) {
                Some(_) if matches!(unworked(row, now_ms), Some(Unworked::Finished { .. })) => {
                    PAUSED_FINISHED.to_string()
                }
                Some(p) => format!("paused since {}", clock_at(p, now_ms, &clock.tz)),
                None => wait.as_ref().map(|w| w.note.clone()).unwrap_or_default(),
            }
        }
        Stage::Done => f
            .and_then(|f| f.landing.clone())
            .or_else(|| row.next_key.as_ref().map(|k| format!("→ {k}")))
            .or_else(|| l.and_then(|l| l.outcome).map(outcome_word))
            .unwrap_or_default(),
    };
    let tokens = row.tokens().map(fmt_tokens).unwrap_or_default();
    let vs = delta.map_or_else(|| "estimate".into(), |(_, a)| a.label());
    let mut facts = row_facts(row.stage, &elapsed, &eta, &vs, &now, &tokens);
    if no_eta {
        facts.insert(
            1.min(facts.len()),
            no_eta_hint(&key, l.map(|l| l.agent_id())),
        );
    }
    if let Some(why) = wait.as_ref().and_then(|w| w.held.as_deref()) {
        facts.push(format!("held: {why}"));
    }
    if let Some(h) = held {
        facts.push(lease_fact(h));
    }
    if let Some(u) = measured.filter(|_| row.stage != Stage::Planned) {
        let fact = usage_fact(u);
        if !fact.is_empty() {
            facts.push(fact);
        }
    }
    // The hover's lines under the title: where the row is and why
    // (giverny#423), from the cells above before any ditto.
    let note = f.and_then(|f| f.note.as_deref());
    let why = match row.stage {
        Stage::Running => {
            let who = match l {
                Some(l) => {
                    let mut w = format!("worker {}", l.display_name());
                    if let Some(m) = l.model.as_deref().and_then(short_model) {
                        w.push_str(&format!(" ({m})"));
                    }
                    w
                }
                None if f.is_some_and(|f| f.live.is_some()) => {
                    "no worker; its own commands run".into()
                }
                None => "no worker on it".into(),
            };
            let since = f
                .and_then(|f| f.spawned_ms)
                .or(row_start)
                .map(|s| clock_at(s, now_ms, &clock.tz));
            let state =
                match unworked(row, now_ms) {
                    Some(Unworked::Stopped(r)) => {
                        Some(format!("stopped: {r} — its worker is not working"))
                    }
                    Some(Unworked::Finished { ended_ms, .. }) if asleep.is_some() => Some(
                        asleep_why(asleep.unwrap_or_default(), ended_ms, now_ms, &clock.tz),
                    ),
                    Some(Unworked::Finished { how, ended_ms }) => Some(match ended_ms {
                        Some(e) => format!(
                            "its worker {how} at {} — not landed yet; nothing works on it \
                         until its manager wakes the worker or lands it",
                            clock_at(e, now_ms, &clock.tz)
                        ),
                        None => format!("its worker {how} — not landed yet"),
                    }),
                    _ => None,
                }
                .or_else(|| {
                    limit.map(|lim| {
                        format!(
                            "{} — out of usage; its clock is held",
                            limit_note(lim, now_ms, &clock.tz)
                        )
                    })
                })
                .or_else(|| paused.map(|p| paused_why(&clock_at(p, now_ms, &clock.tz), note)));
            // The worker's open wait, in full: NOW cuts it (giverny#471).
            let waited = waiting
                .filter(|_| paused.is_none())
                .map(|(w, n)| waiting_why(w, n, now_ms, &clock.tz));
            // What the worker is doing right now, whole: NOW cuts it to fit.
            let doing = l
                .filter(|l| l.running())
                .and_then(|l| l.activity.clone())
                .filter(|a| !a.is_empty());
            doing
                .into_iter()
                .chain(running_why(&who, since.as_deref(), eta_s, left_s, state))
                .chain(waited)
                .collect()
        }
        Stage::Planned => {
            let line =
                match benched_pause.filter(|_| wait.as_ref().is_none_or(|w| w.held.is_none())) {
                    Some(_) if now == PAUSED_FINISHED => {
                        format!("{PAUSED_FINISHED} — its work is done, to land or resume")
                    }
                    Some(p) => paused_why(&clock_at(p, now_ms, &clock.tz), note),
                    None => wait.as_ref().map(|w| w.why.clone()).unwrap_or_default(),
                };
            let est = (!eta.is_empty()).then(|| format!("estimate {eta}"));
            [Some(line).filter(|l| !l.is_empty()), est]
                .into_iter()
                .flatten()
                .collect()
        }
        Stage::Done => done_why(&now, &elapsed, delta, f.and_then(|f| f.review.as_deref())),
    };
    Line {
        stage: row.stage,
        id,
        title,
        why,
        elapsed,
        eta,
        no_eta,
        now,
        tokens,
        usage,
        usage_load,
        oom,
        flag: flag.is_some(),
        landed_ms: row.ended_ms().filter(|_| row.stage == Stage::Done),
        depth: 0,
        fold: None,
        click: RowClick {
            stage: row.stage,
            key,
            agent_id: row.agent_id().map(str::to_string),
            finished: row.stage == Stage::Running && l.is_some_and(|l| !l.running()),
            asleep: asleep.is_some(),
            name: l
                .map(|l| l.display_name().to_string())
                .or_else(|| f.and_then(|f| f.title.clone()))
                .unwrap_or_default(),
            transcript: l.and_then(|l| l.transcript.clone()),
            open: f.and_then(|f| f.open.clone()),
            brief: f.and_then(|f| f.brief.clone()),
            note: f.and_then(|f| f.note.clone()),
            review: f.and_then(|f| f.review.clone()),
            facts,
        },
    }
}

/// A Running row nothing is running (giverny#258).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Unworked {
    /// Its worker ended on an API error it never wrote past: that error.
    Stopped(String),
    /// Its worker finished — handed back, failed, killed or stopped
    /// (`how`) — at `ended_ms`, and nobody has landed the row.
    Finished {
        how: &'static str,
        ended_ms: Option<u64>,
    },
    /// No worker at all, and no command of its own runs: since it was
    /// handed out (started, or handed to a worker that never picked it up).
    NoWorker { since: u64 },
}

impl Unworked {
    /// What the pane flags of it, in NOW: the person's to know. A finished
    /// worker is its manager's to land, a row with no worker its manager's
    /// to staff ([`chores`]): never flagged.
    fn flag(self) -> Option<String> {
        match self {
            Unworked::Stopped(reason) => Some(format!("stopped: {reason}")),
            Unworked::NoWorker { .. } | Unworked::Finished { .. } => None,
        }
    }
}

/// Why nothing is running a Running row: the feed says the task runs, but
/// its worker has finished — handed back, failed, killed or stopped (on an
/// API error, that error) — and nobody has landed it; or it has no worker
/// at all, [`feed::HANDOFF_WINDOW_MS`] after it started (a dispatcher runs
/// `start` a moment before the spawn), and no command of its own runs
/// either. A worker counts as finished only if it ended after the row was
/// handed to it ([`PaneRow::handed_at_ms`]); one that ended before is
/// still to pick the row up, and is [`Unworked::NoWorker`] only once
/// [`feed::HANDOFF_WINDOW_MS`] has passed without it doing so. `None` while
/// a worker works on it.
fn unworked(row: &PaneRow<'_, SubagentRow>, now_ms: u64) -> Option<Unworked> {
    if let Some(l) = row.live {
        if l.running() {
            return None;
        }
        let ended_ms = l.ended_ms.or(l.last_turn_ms);
        // An idle worker handed this task (`start --agent`, a message) and
        // not woken yet ended its previous task, not this one (giverny#332):
        // it is about to pick this up. Nothing to land, nothing to flag,
        // until it has had [`feed::HANDOFF_WINDOW_MS`] to; then nothing
        // works on the row.
        if let (Some(end), Some(handed)) = (ended_ms, row.handed_at_ms())
            && end < handed
        {
            let fresh = now_ms < handed.saturating_add(feed::HANDOFF_WINDOW_MS);
            return (!fresh).then_some(Unworked::NoWorker { since: handed });
        }
        // Ended on an API error it never wrote past: that error is why.
        if let Some(s) = l.stops.last().filter(|s| s.to_ms.is_none()) {
            return Some(Unworked::Stopped(s.reason.clone()));
        }
        let how = match l.outcome {
            Some(Outcome::Failed) => "failed",
            Some(Outcome::Killed) => "killed",
            Some(Outcome::Stopped) => "stopped",
            _ => "finished",
        };
        return Some(Unworked::Finished { how, ended_ms });
    }
    let f = row.feed?;
    let started = f.started_ms?;
    let fresh = now_ms < started.saturating_add(feed::HANDOFF_WINDOW_MS);
    (!fresh && f.live.is_none()).then_some(Unworked::NoWorker { since: started })
}

/// What a Next up row's NOW says ([`planned_wait`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wait {
    pub note: String,
    /// It waits on nothing recorded, since this instant (epoch ms): its
    /// manager's to start or hold once that is
    /// [`manage_chores::IDLE_PLANNED_MS`] past ([`chores`]). Drawn as any
    /// Next up row.
    pub idle_since: Option<u64>,
    /// The dispatcher's hold, in full, for the overlay header.
    pub held: Option<String>,
    /// The row's hover line (giverny#423): the note, and why it is there.
    pub why: String,
    /// Where it stands in line to start ([`start_order`]).
    pub start: Start,
    /// The task it waits for (`after <key>`), if any.
    pub after: Option<String>,
}

/// Where a Next up row stands in line to start (giverny#442), first to
/// last: the pane draws Next up in this order, the one that starts next on
/// top ([`start_order`]); rows of one place keep their plan order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Start {
    /// Its lease is granted, or it was started: it waits only for its
    /// worker.
    Ready,
    /// Queued in the machine ledger, at this place (1-based; one with no
    /// place known goes after the rest).
    Queued(u64),
    /// Nothing recorded to wait for: its manager's to start.
    Idle,
    /// After another task ([`Wait::after`]), and after that task's row when
    /// it is in Next up too.
    After,
    /// Held or paused: it starts only on a person's or its manager's call.
    SetAside,
}

/// What a Next up row waits for (giverny#282). A planned task may wait only
/// on a hold the dispatcher recorded (`held: <why>`), a lease queued in the
/// ledger (`queued for 3G behind demo#12`), or another task: the one it was
/// planned `--after` until that lands, or the task its worker is busy with
/// (`after <key>`). A row with none of these waits on nothing, since it last
/// could have started (planned, its hold ended, the task before it landed,
/// its lease granted): [`Wait::idle_since`], and an empty NOW. `None` for
/// any other row; a Next up row whose writer never said when it was planned
/// has no `idle_since`.
fn planned_wait(row: &PaneRow<'_, SubagentRow>, rows: &[PaneRow<'_, SubagentRow>]) -> Option<Wait> {
    if row.stage != Stage::Planned {
        return None;
    }
    let f = row.feed;
    let waits = |note: String, why: String, start: Start| {
        Some(Wait {
            note,
            idle_since: None,
            held: None,
            why,
            start,
            after: None,
        })
    };
    if let Some(why) = f.and_then(|f| f.held.clone()) {
        return Some(Wait {
            note: format!("held: {why}"),
            idle_since: None,
            why: format!("held — not started; waiting for: {why}"),
            held: Some(why),
            start: Start::SetAside,
            after: None,
        });
    }
    // A Running row benched for want of a worker ([`benched`]): a paused
    // one says since when ([`format_row`]), and is set aside.
    if let Some(f) = f.filter(|f| f.stage() == Stage::Running) {
        return waits(
            NO_WORKER_YET.to_string(),
            format!("{NO_WORKER_YET} — started, but no worker has picked it up"),
            if f.paused_since_ms.is_some() {
                Start::SetAside
            } else {
                Start::Ready
            },
        );
    }
    let lease = f.and_then(|f| f.lease.as_ref());
    if let Some(l) = lease.filter(|l| l.state == LeaseState::Queued) {
        let place = l.position.unwrap_or(u64::MAX);
        return waits(queued_note(l), queued_why(l), Start::Queued(place));
    }
    if lease.is_some_and(|l| l.uncollected) {
        return waits(
            GRANTED_WAITING.to_string(),
            format!("{GRANTED_WAITING} — its lease is granted; its manager is to start it"),
            Start::Ready,
        );
    }
    // The task it was planned after: waited for until it is drawn Done; one
    // gone from the feed is no wait.
    let before = f
        .and_then(|f| f.after.as_deref())
        .and_then(|a| rows.iter().find(|r| r.feed.is_some_and(|f| f.key == a)));
    let after = |k: &str, note: String, why: String| {
        Some(Wait {
            after: Some(k.to_string()),
            ..waits(note, why, Start::After)?
        })
    };
    if let Some(r) = before.filter(|r| r.stage != Stage::Done) {
        let k = r.feed.map_or("", |f| f.key.as_str());
        return after(
            k,
            format!("after {k}"),
            format!("after {k} — starts when that lands"),
        );
    }
    if let Some(k) = &row.after_key {
        return after(
            k,
            format!("after {k}"),
            format!("after {k} — starts when its worker is done with that"),
        );
    }
    let f = f?;
    let since = [
        f.planned_ms,
        f.unheld_ms,
        before.and_then(|r| r.ended_ms()),
        lease.and_then(|l| l.granted_ms),
    ]
    .into_iter()
    .flatten()
    .max();
    // A lease already granted: it waits only for its worker.
    let granted = lease.is_some_and(|l| l.state != LeaseState::Queued);
    Some(Wait {
        note: String::new(),
        idle_since: since,
        held: None,
        why: IDLE_WHY.to_string(),
        start: if granted { Start::Ready } else { Start::Idle },
        after: None,
    })
}

/// Each row's place in the pane's drawing order (giverny#442): its section,
/// then, in Next up, where it stands in line to start ([`Wait::start`]) and
/// how deep in a chain of `after`s it sits. A row after another Next up row
/// goes after that row: no sooner than its place, one deeper, so it is set
/// aside too behind a held one. Sorted on stably, rows of one place keep
/// their plan order; across managers the ledger's places decide.
fn start_order(
    rows: &[PaneRow<'_, SubagentRow>],
    waits: &[Option<Wait>],
) -> Vec<(Stage, Start, usize)> {
    fn place(
        i: usize,
        rows: &[PaneRow<'_, SubagentRow>],
        waits: &[Option<Wait>],
        seen: &mut Vec<usize>,
    ) -> (Start, usize) {
        let Some(w) = &waits[i] else {
            return (Start::Idle, 0);
        };
        let before = w.after.as_deref().and_then(|k| {
            rows.iter()
                .position(|r| r.stage == Stage::Planned && r.feed.is_some_and(|f| f.key == k))
        });
        match before {
            // A loop of `after`s is no chain: its rows keep their own place.
            Some(j) if !seen.contains(&j) && j != i => {
                seen.push(i);
                let (start, depth) = place(j, rows, waits, seen);
                (start.max(w.start), depth + 1)
            }
            _ => (w.start, usize::from(w.after.is_some())),
        }
    }
    (0..rows.len())
        .map(|i| match rows[i].stage {
            Stage::Planned => {
                let (start, depth) = place(i, rows, waits, &mut Vec::new());
                (Stage::Planned, start, depth)
            }
            s => (s, Start::Ready, 0),
        })
        .collect()
}

/// The hover of a Next up row that waits on nothing recorded
/// ([`Wait::idle_since`]).
pub const IDLE_WHY: &str = "not started — waits on nothing; its manager is to start it";

/// NOW of a Running row with no worker, drawn under Next up ([`benched`]).
pub const NO_WORKER_YET: &str = "no worker yet";

/// NOW of a paused Running row whose worker finished, drawn under Next up
/// ([`benched`], giverny#379): its work is done, its manager's to land or
/// resume ([`chores`]). About as wide as the `paused since 15:10` it takes
/// the place of.
pub const PAUSED_FINISHED: &str = "paused · worker finished";

/// The overlay header's facts, from the row's own cells before any ditto.
/// `vs` names what a Done row's ± was measured against (giverny#402):
/// `worker's 40m`, `estimate 30m`.
fn row_facts(
    stage: Stage,
    elapsed: &str,
    eta: &str,
    vs: &str,
    now: &str,
    tokens: &str,
) -> Vec<String> {
    let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
    let v = match stage {
        Stage::Done => [
            some(now),
            some(elapsed).map(|e| format!("took {e}")),
            some(eta).map(|d| format!("{d} vs {vs}")),
            some(tokens).map(|t| format!("{t} tokens")),
        ],
        Stage::Running => [
            some(elapsed).map(|e| format!("running {e}")),
            some(eta).map(|l| format!("{l} left")),
            some(now),
            some(tokens).map(|t| format!("{t} tokens")),
        ],
        Stage::Planned => [some(eta).map(|e| format!("est {e}")), None, None, None],
    };
    v.into_iter().flatten().collect()
}

/// A Running row's hover (giverny#423): who works on it and since when,
/// what holds it when something does (`state`), and its estimate against
/// the time left (`left_s`, below zero once over).
fn running_why(
    who: &str,
    since: Option<&str>,
    estimate_s: Option<u64>,
    left_s: Option<i64>,
    state: Option<String>,
) -> Vec<String> {
    let mut head = format!("Running: {who}");
    if let Some(s) = since {
        head.push_str(&format!(" since {s}"));
    }
    let est = match (estimate_s, left_s) {
        (Some(e), Some(left)) if left < 0 && feed::fmt_span(left) != "0m" => format!(
            "estimate {}: {} over it",
            feed::fmt_span(e as i64),
            feed::fmt_span(-left)
        ),
        (Some(e), Some(left)) => format!(
            "estimate {}: {} left, on track",
            feed::fmt_span(e as i64),
            countdown(left)
        ),
        (Some(e), None) => format!("estimate {}", feed::fmt_span(e as i64)),
        (None, _) => "no estimate yet".into(),
    };
    std::iter::once(head).chain(state).chain([est]).collect()
}

/// The background shells a Running row's worker sleeps on (giverny#476):
/// its turn ended as a plain finish — Claude Code lists it as completed,
/// "waiting on its own background work" — while shells it started still
/// run ([`bg_shells::Watch`]). Claude Code wakes it when one ends, so it is
/// asleep, not ended: nothing to land, nobody to wake. Oldest first.
fn asleep_on<'s>(
    row: &PaneRow<'_, SubagentRow>,
    shells: &'s Shells,
    now_ms: u64,
) -> Option<&'s [bg_shells::Shell]> {
    let l = row.live?;
    match unworked(row, now_ms) {
        Some(Unworked::Finished {
            how: "finished", ..
        }) => {}
        _ => return None,
    }
    shells
        .get(&l.id)
        .map(Vec::as_slice)
        .filter(|s| !s.is_empty())
}

/// NOW of a Running row whose worker is asleep on its own background
/// shells (giverny#476): `asleep · wrangler deploy (30m) · wakes when it
/// ends`, `+N` for more, and the wait it marked, if any, as [`turn_ended`]
/// has it. What it sleeps on comes first: the cell is cut, the hover has
/// it all.
fn asleep_now(
    shells: &[bg_shells::Shell],
    waiting: Option<(u64, Option<&str>)>,
    now_ms: u64,
) -> String {
    let when = if shells.len() == 1 {
        "it ends"
    } else {
        "one ends"
    };
    let base = format!(
        "asleep · {} · wakes when {when}",
        bg_shells::named(shells, now_ms)
    );
    match waiting {
        Some((_, Some(n))) if !n.trim().is_empty() => format!("{base} · waiting: {}", n.trim()),
        Some(_) => format!("{base} · waiting"),
        None => base,
    }
}

/// The hover's line for an asleep worker (giverny#476): since when, each
/// shell in full, and that Claude Code wakes it.
fn asleep_why(
    shells: &[bg_shells::Shell],
    ended_ms: Option<u64>,
    now_ms: u64,
    tz: &jiff::tz::TimeZone,
) -> String {
    let since = ended_ms.map_or(String::new(), |e| {
        format!(" since {}", clock_at(e, now_ms, tz))
    });
    let list: Vec<String> = shells
        .iter()
        .map(|s| {
            format!(
                "`{}` ({} in, shell {})",
                bg_shells::gist(&s.cmd),
                feed::fmt_span((now_ms.saturating_sub(s.started_ms) / 1000) as i64),
                s.id
            )
        })
        .collect();
    let (what, when) = if shells.len() == 1 {
        ("its own background shell runs", "that ends")
    } else {
        ("its own background shells run", "one ends")
    };
    format!(
        "its worker is asleep{since}: its turn ended while {what}: {}. Claude Code wakes it \
         when {when}; nothing to land yet",
        list.join(", ")
    )
}

/// A paused row's hover: `paused since 15:10: <note>`, else why its
/// clock stands.
/// NOW of a Running row whose worker ended its turn (giverny#471): when,
/// and the wait it marked, if any — `turn ended 11:41 · waiting: machine
/// time`.
fn turn_ended(
    ended_ms: Option<u64>,
    waiting: Option<(u64, Option<&str>)>,
    now_ms: u64,
    tz: &jiff::tz::TimeZone,
) -> String {
    let ended = match ended_ms {
        Some(e) => format!("turn ended {}", clock_at(e, now_ms, tz)),
        None => "turn ended".into(),
    };
    match waiting {
        Some((_, Some(n))) if !n.trim().is_empty() => format!("{ended} · waiting: {}", n.trim()),
        Some(_) => format!("{ended} · waiting"),
        None => ended,
    }
}

/// The hover's line for a worker's open wait (giverny#471).
fn waiting_why(since_ms: u64, note: Option<&str>, now_ms: u64, tz: &jiff::tz::TimeZone) -> String {
    let since = clock_at(since_ms, now_ms, tz);
    match note.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => format!("waiting since {since}: {n}"),
        None => format!("waiting since {since}, on something that is not the work"),
    }
}

fn paused_why(since: &str, note: Option<&str>) -> String {
    match note.filter(|n| !n.is_empty()) {
        Some(n) => format!("paused since {since}: {n}"),
        None => format!("paused since {since} — its clock is stopped"),
    }
}

/// A Done row's hover (giverny#423): how it landed (`now`, the row's NOW)
/// and how long it took against its estimate, then who is to review it and
/// where, when someone is.
fn done_why(
    now: &str,
    elapsed: &str,
    delta: Option<(i64, feed::Against)>,
    review: Option<&str>,
) -> Vec<String> {
    let mut head = "Done".to_string();
    // A landing that says only Done is not said twice.
    if !now.is_empty() && !now.eq_ignore_ascii_case("done") {
        head.push_str(&format!(": {now}"));
    }
    if !elapsed.is_empty() {
        head.push_str(&format!("; took {elapsed}"));
        head.push_str(&match delta {
            Some((d, a)) => {
                let what = format!("its {}", a.label());
                match feed::fmt_span(d.abs()) {
                    s if s == "0m" => format!(", on {what}"),
                    s if d > 0 => format!(", {s} over {what}"),
                    s => format!(", {s} under {what}"),
                }
            }
            None => ", no estimate".into(),
        });
    }
    std::iter::once(head)
        .chain(review.map(|r| format!("awaiting review: {r}")))
        .collect()
}

/// How a Running row with no estimate gets one, as the overlay header says
/// it: a manager session's task is re-estimated on its row; any other
/// worker is given an agent ETA by its id.
fn no_eta_hint(key: &str, agent_id: Option<&str>) -> String {
    if !key.is_empty() {
        return format!("no ETA — add one: giverny-manage eta {key} <min> --why scope");
    }
    let agent = agent_id.unwrap_or("<agent-id>");
    format!("no ETA — add one: giverny-eta {agent} <min>")
}

/// A feed row's key as the TASK column shows it: none for the
/// `agent-<id>` a worker with no task once started its own row under
/// (giverny#158's first round), which names nothing a person knows.
/// A model id in Claude Code's short names (`haiku`, `sonnet`, `opus`,
/// `fable`), without version or date; None for a name of no known family.
fn short_model(m: &str) -> Option<String> {
    let m = m.to_ascii_lowercase();
    ["haiku", "sonnet", "opus", "fable"]
        .into_iter()
        .find(|f| m.contains(f))
        .map(str::to_string)
}

fn shown_key(f: &feed::FeedRow) -> String {
    let made_up = f.follows_worker
        && f.key
            .strip_prefix("agent-")
            .is_some_and(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric()));
    if made_up {
        String::new()
    } else {
        f.key.clone()
    }
}

fn outcome_word(o: Outcome) -> String {
    match o {
        Outcome::Completed | Outcome::Unknown => "done",
        Outcome::Failed => "failed",
        Outcome::Killed => "killed",
        Outcome::Stopped => "stopped",
    }
    .into()
}

// --------------------------------------------------------- formatting ----

/// ELAPSED as a stopwatch writes it, only the digits it needs: `0:42`,
/// `14:05`, `1:03:07`.
pub fn stopwatch(secs: u64) -> String {
    let (h, rest) = (secs / 3600, secs % 3600);
    let (m, s) = (rest / 60, rest % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A Running row's time left: `~20m`, and `-~12m` once past its estimate.
pub fn countdown(left_s: i64) -> String {
    if left_s < 0 && feed::fmt_span(left_s) != "0m" {
        format!("-~{}", feed::fmt_span(-left_s))
    } else {
        format!("~{}", feed::fmt_span(left_s.max(0)))
    }
}

/// Claude Code's compact count — `842`, `13.5k`, `124.8k`, `1.2M` — with a
/// capital `M` so a total never reads as minutes.
pub fn fmt_tokens(n: u64) -> String {
    for (floor, div, suffix) in [
        (999_950_000u64, 1_000_000_000f64, "B"),
        (999_950, 1_000_000.0, "M"),
        (1000, 1000.0, "k"),
    ] {
        if n >= floor {
            let v = format!("{:.1}", n as f64 / div);
            let v = v.strip_suffix(".0").unwrap_or(&v);
            return format!("{v}{suffix}");
        }
    }
    n.to_string()
}

fn stage_word(s: Stage) -> &'static str {
    match s {
        Stage::Running => "Running",
        Stage::Planned => "NextUp",
        Stage::Done => "Done",
    }
}

fn stage_color(s: Stage) -> Color32 {
    match s {
        Stage::Running => RUNNING,
        Stage::Planned => PLANNED,
        Stage::Done => DONE,
    }
}

/// Cut to `max` characters, the last one an ellipsis.
/// Start column that makes `s` end at column `end` (right alignment on the
/// cell grid).
fn right_at(end: usize, s: &str) -> usize {
    end.saturating_sub(s.chars().count())
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

// ------------------------------------------------------------ drawing ----

/// Draw `tab`'s pane from `tracker`, if there are rows. Call inside the
/// central panel, before the terminal takes the rest. Returns the row
/// clicked this frame.
///
/// `limit` is the usage limit the tab's account is out on ([`limit_for`]):
/// while it is, the Running rows' clocks are held. `viewed` is the worker
/// the tab shows, in Claude Code's view or on Giverny's page: its row is
/// lit as the selection. `header` names the worker the terminal's
/// header is about, whose row comes back with the click, as
/// the pane drew it.
#[allow(clippy::too_many_arguments)]
pub fn show(
    views: &mut Views,
    tab: TabId,
    reading: Option<&giverny_claude::use_reading::Reading>,
    tracker: Option<&Tracker>,
    viewed: Option<Shown<'_>>,
    header: Option<Shown<'_>>,
    limit: Option<Limit>,
    panel: &ManagementPanelConfig,
    chrome: &Chrome,
    shared: &mut RenderShared,
    ui: &mut Ui,
) -> (Option<RowClick>, Option<Line>) {
    // A look asks for one flash, at the drawing right after it: a pane with
    // nothing to show now does not flash later (giverny#394).
    let flash_due = views
        .tabs
        .get_mut(&tab)
        .is_some_and(|v| std::mem::take(&mut v.flash_due));
    let Some(tracker) = tracker else {
        return (None, None);
    };
    let view = views.tabs.entry(tab).or_default();
    view.poll_feed(
        tracker.session_id.as_deref(),
        &tracker.aliases,
        tracker.config_dir.as_deref(),
    );
    // Only when subagents are involved, or for a manager (giverny#357): a
    // feed row alone opens nothing.
    if !feed::opens(!tracker.is_empty(), view.manager) {
        return (None, None);
    }
    let drawn = Drawn::of(view.manager);
    let now = now_ms();
    track(&mut view.holds, limit.as_ref(), now);
    // What the rows' processes use: this frame's reading, the one the
    // sidebar shows the sums of (`sessions_load::latest`).
    let runs = reading
        .map(crate::sessions_load::task_lives)
        .unwrap_or_default();
    let workers = reading
        .map(crate::sessions_load::workers)
        .unwrap_or_default();
    let clock = Clock {
        holds: &view.holds,
        sleeps: views.sleeps.poll(&feed::feed_dir()),
        limit: limit.as_ref(),
        tz: jiff::tz::TimeZone::system(),
        workers: &workers,
        etas: &view.etas_now,
        shells: view.shells.live(),
    };
    // Done rows cleared by hand go from the feed too, whoever wrote it.
    let cleared = tracker
        .done_cleared_ms
        .and_then(|c| view.feed_now.as_ref().map(|f| f.without_done_by(c)));
    let feed = cleared.as_ref().or(view.feed_now.as_ref());
    // The ledger is the truth about leases; a row's copy is as of its
    // session's last claim.
    let ledger = views.ledger.get(ui.ctx());
    let leased = match (feed, ledger.as_deref()) {
        (Some(f), Some(l)) => {
            let mut sessions: Vec<&str> = f.session.iter().map(String::as_str).collect();
            sessions.extend(f.aliases.iter().map(String::as_str));
            sessions.extend(tracker.session_id.as_deref());
            sessions.extend(tracker.aliases.iter().map(String::as_str));
            Some(with_workers(
                with_live(with_ledger(f, &sessions, &l.ledger), &sessions, &runs),
                &sessions,
                &runs,
                &workers,
            ))
        }
        _ => None,
    };
    let feed = leased.as_ref().or(feed);
    if std::mem::take(&mut view.logs_due) {
        poll_logs(&mut view.logs, tracker.rows());
    }
    let mut table = build_with(
        feed,
        tracker.rows(),
        now,
        &clock,
        &view.logs,
        panel.done(),
        drawn,
        &view.folded,
    );
    table.footer = with_hookless(table.footer.take(), view.hookless);
    if table.is_empty() {
        return (None, None);
    }
    // What landed since the last look breathes; whatever is drawn now has
    // been seen (giverny#394).
    if flash_due {
        let rows = newly_done(&table.lines, view.seen_done.as_ref(), views.born_ms);
        view.flash = (!rows.is_empty()).then(|| Flash {
            rows,
            from: crate::rail::anim_time(),
        });
    }
    note_seen(&mut view.seen_done, &table.lines);
    let animate = crate::rail::animating(ui);
    let glow = view.flash.as_ref().and_then(|f| {
        let t = crate::rail::anim_time() - f.from;
        let glow = flash_glow(t, animate)?;
        if !animate {
            // Still: one frame, where the steady highlight ends.
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f64((FLASH_S - t).max(0.0)));
        }
        Some(glow)
    });
    if glow.is_none() {
        view.flash = None;
    }
    let flash = view.flash.as_ref().zip(glow).map(|(f, g)| (&f.rows, g));
    // The first of a worker's rows: the one with no ditto in it.
    let header_line = header.and_then(|h| table.lines.iter().find(|l| h.is(&l.click)).cloned());
    if table.any_running() {
        // ELAPSED is a stopwatch.
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }

    // The session's own cell: the pane is drawn in the terminal's glyphs
    // (face, size, hinting, grid), so it follows zoom and the font setting.
    let cell = shared.cell_size(ui.ctx().pixels_per_point());
    // A little air around each row; a table, not a wall of grid.
    let row_h = (cell.y * 1.2).round().max(cell.y);
    let rows = table.lines.len() + usize::from(table.footer.is_some());
    let frame = pane_frame(shared.theme.bg);
    let want = pane_height(
        rows,
        row_h,
        ui.spacing().item_spacing.y,
        frame.total_margin().sum().y,
    );
    let min_h = row_h * 1.5;
    let max_h = (ui.available_height() * 0.5).max(row_h * 3.0);
    let fit = want.clamp(min_h, max_h);
    let id = egui::Id::new(("management_panel", tab));
    follow_rows(ui.ctx(), id, &mut view.fit, fit);

    let mut clicked = None;
    egui::Panel::bottom(id)
        .resizable(true)
        .default_size(fit)
        .size_range(min_h..=max_h)
        .frame(frame)
        .show(ui, |ui| {
            // Measured outside the scroll area: inside it, the width shrinks
            // by the bar's lane only while the rows overflow, and the right
            // columns would jump as the pane is resized across that point.
            let full = ui.available_rect_before_wrap();
            let ppp = ui.ctx().pixels_per_point();
            let cols = table_cols(full.width(), cell.x, ppp, bar_lane(ui));
            // The table on the terminal's grid: INSET whole cells in, as
            // Claude Code's status line is.
            let grid = full.with_min_x(full.min.x + INSET as f32 * cell.x);
            let outer = ui;
            let mut inner = outer.new_child(egui::UiBuilder::new().max_rect(grid));
            let ui = &mut inner;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let drew = draw_table(
                        ui,
                        id.with("rows"),
                        &table,
                        &panel.columns,
                        viewed,
                        flash,
                        animate,
                        chrome,
                        shared,
                        cell,
                        row_h,
                        cols,
                        &mut view.sel,
                    );
                    clicked = drew.clicked;
                    view.hover = drew.hover;
                    if let Some(ident) = drew.fold {
                        if !view.folded.remove(&ident) {
                            view.folded.insert(ident);
                        }
                        ui.ctx().request_repaint();
                    }
                    #[cfg(debug_assertions)]
                    {
                        view.clicks = table.lines.iter().map(|l| l.click.clone()).collect();
                    }
                    view.rows_at = drew.rects;
                    view.cell_w = cell.x.max(1.0);
                });
            outer.advance_cursor_after_rect(inner.min_rect());
        });
    (clicked, header_line)
}

/// The pane's frame: the session's own background, not the rail's lifted
/// panel colour, so the pane reads as part of the terminal above it. egui's
/// separator line (on by default) keeps the boundary between the two.
fn pane_frame(bg: Color32) -> egui::Frame {
    // No margin at the sides: the table is placed on the terminal's grid
    // ([`INSET`]), and the scroll bar sits at the right edge.
    egui::Frame::NONE
        .fill(bg)
        .inner_margin(egui::Margin::symmetric(0, 5))
}

/// The pane's outer height for `rows` rows of `row_h`: the rows, egui's
/// `gap` between each two of them, and the frame's `margin`. Leave out the
/// gaps and the rows overflow by one gap a row — about one row in seven —
/// and the scroll area's fade dims the last row shown.
fn pane_height(rows: usize, row_h: f32, gap: f32, margin: f32) -> f32 {
    let n = rows as f32;
    n * row_h + (n - 1.0).max(0.0) * gap + margin
}

/// Keep the pane sized to its rows as they come and go, until the user
/// drags it: egui remembers a panel's height and only reads `default_size`
/// the first time, so a pane opened with three rows would stay three rows
/// tall. `fit` is the height the pane last sized itself to; a stored
/// height that differs from it is the user's, and from then on theirs.
fn follow_rows(ctx: &egui::Context, id: egui::Id, fit: &mut Option<f32>, want: f32) {
    let stored = egui::containers::panel::PanelState::load(ctx, id).map(|s| s.size().y);
    match (stored, *fit) {
        // First sight: default_size applies.
        (None, _) => *fit = Some(want),
        // Still ours; resize to the rows if they changed.
        (Some(h), Some(f)) if (h - f).abs() <= 1.0 => {
            if (want - f).abs() > 0.5 {
                ctx.data_mut(|d| d.remove::<egui::containers::panel::PanelState>(id));
                *fit = Some(want);
            }
        }
        // Dragged: the user's height stands.
        _ => *fit = None,
    }
}

/// A row's background tint, from the pointer alone: a deeper one while it is
/// pressed, a light one while hovered, none otherwise. There is no third
/// input — a click leaves nothing behind to highlight.
/// How strongly the row of the worker on view is lit: past a hover's, so
/// it reads as the selection even under the pointer.
const VIEWED_TINT: f32 = 0.26;

/// How wide a newly done row's breathing frame is, in points (giverny#394).
const FLASH_WIDTH: f32 = 1.5;
/// How far out past the row's left edge the frame sits, in points: clear of
/// the row's first glyph, and within the scroll area's clip margin, so it is
/// not cut. The right edge is the row's own, which ends clear of its last
/// glyph and at the clip.
const FLASH_OUT: f32 = 3.0;

fn row_tint(hovered: bool, pressed: bool) -> Option<f32> {
    if pressed {
        Some(0.22)
    } else if hovered {
        Some(0.10)
    } else {
        None
    }
}

/// The scrollbar's full lane, in points, whether or not it is showing.
fn bar_lane(ui: &Ui) -> f32 {
    let bar = &ui.spacing().scroll;
    bar.bar_inner_margin + bar.bar_width + bar.bar_outer_margin
}

/// How many cells in from the pane's left edge the table starts: as many
/// as Claude Code leaves at the left of its status line, which it draws
/// two columns into the terminal right above the pane.
const INSET: usize = session_use::CLAUDE_MARGIN / 2;

/// How many character columns the table lays out in, from the pane's width
/// *outside* the scroll area, which is the terminal's: the pane spans the
/// terminal's area, and is drawn on its grid ([`INSET`] whole cells in).
/// The table is as wide as the status line Claude Code draws over it
/// (the terminal's columns less [`session_use::CLAUDE_MARGIN`]), so its
/// last column, the use column, ends where the status line does. The bar's
/// lane is always left free, so it never paints over a cell and the
/// columns never move when it appears; on the usual sizes it fits in the
/// two cells right of the table anyway.
fn table_cols(width: f32, cell_w: f32, ppp: f32, bar_lane: f32) -> usize {
    let cw = cell_w.max(0.1);
    // The terminal counts its columns in whole device pixels
    // (`TermView::show`); counted the same way, the two never disagree.
    let cell_px = (cw * ppp).round().max(1.0);
    let terminal = (width * ppp / cell_px).floor() as usize;
    let under_line = terminal.saturating_sub(session_use::CLAUDE_MARGIN);
    let room = ((width - INSET as f32 * cw - bar_lane) / cw)
        .floor()
        .max(0.0) as usize;
    under_line.min(room).max(40)
}

/// Where each column of a row starts or ends, in characters, for a table
/// laid out in `cols` columns. A column switched off
/// (`[management_panel.columns]`) takes no width: the rest close up, each a
/// [`GAP`] from the one before, and TASK takes what is left.
struct Cols {
    on: PaneColumns,
    idw: usize,
    taskw: usize,
    x_task: usize,
    x_el_end: usize,
    x_eta_end: usize,
    x_now: usize,
    x_tok_end: usize,
    /// The use column, the last: a [`usage_cell`] wide while any row is Running
    /// or Done, not there at all while every row is Next up. It ends on
    /// the table's last column, so it ends where the status line above
    /// does ([`table_cols`]).
    usew: usize,
    x_use: usize,
    /// The table nests rows ([`Table::nested`]).
    nested: bool,
}

impl Cols {
    /// Every column on.
    #[cfg(test)]
    fn new(table: &Table, cols: usize) -> Self {
        Self::shown(table, cols, &PaneColumns::default())
    }

    fn shown(table: &Table, cols: usize, on: &PaneColumns) -> Self {
        let idw = if on.id {
            table
                .lines
                .iter()
                .map(|l| l.id.chars().count())
                .max()
                .unwrap_or(0)
        } else {
            0
        };
        // Every Running and Done row has a cell, all of one width
        // ([`usage_cell`]), a Next up row none.
        let usew = if on.usage {
            table
                .lines
                .iter()
                .map(|l| l.usage.chars().count())
                .max()
                .unwrap_or(0)
        } else {
            0
        };
        // The fixed columns right of TASK, each with the gap before it.
        let fixed_right: usize = [
            (on.elapsed, EL_W),
            (on.eta, ETA_W),
            (on.now, NOW_W),
            (on.tokens, TOK_W),
            (usew > 0, usew),
        ]
        .iter()
        .filter(|(shown, _)| *shown)
        .map(|(_, w)| w + GAP)
        .sum();
        let stage_cols = if on.stage { STAGE_W + GAP } else { 0 };
        // TASK is the id and the title; with the title off, just the id.
        // However narrow the pane, it holds the id and, on every row, its
        // nesting, a few cells of title and a folded row's short note
        // (`+3`), so nothing spills into ELAPSED (giverny#464): the table
        // runs off the right instead.
        let taskw = if on.title {
            let id_room = if idw > 0 { idw + 1 } else { 0 };
            let least = table
                .lines
                .iter()
                .map(|l| {
                    let note = l.fold.as_ref().map(Fold::short_note).unwrap_or_default();
                    let note = if note.is_empty() {
                        0
                    } else {
                        note.chars().count() + 2
                    };
                    nest_head(l, table.nested).chars().count() + note + TITLE_KEEP
                })
                .max()
                .unwrap_or(0);
            cols.saturating_sub(stage_cols + fixed_right)
                .max(id_room + MIN_TITLE.max(least))
        } else {
            idw
        };
        // Lay the shown columns out left to right; `place` gives a
        // column's start and moves past it.
        let mut x = 0;
        let mut any = false;
        let mut place = |shown: bool, w: usize| -> usize {
            if !shown {
                return x;
            }
            let at = if any { x + GAP } else { 0 };
            any = true;
            x = at + w;
            at
        };
        place(on.stage, STAGE_W);
        let x_task = place(taskw > 0, taskw);
        let x_el_end = place(on.elapsed, EL_W) + EL_W;
        let x_eta_end = place(on.eta, ETA_W) + ETA_W;
        let x_now = place(on.now, NOW_W);
        let x_tok_end = place(on.tokens, TOK_W) + TOK_W;
        // The use column ends on the table's last column even when TASK
        // does not stretch to fill (the title off), never closer than a
        // gap to what is before it.
        let x_use = place(usew > 0, usew).max(cols.saturating_sub(usew));
        Cols {
            on: *on,
            idw,
            taskw,
            x_task,
            x_el_end,
            x_eta_end,
            x_now,
            x_tok_end,
            usew,
            x_use,
            nested: table.nested,
        }
    }

    /// The cells TASK leaves the title, past the id; `None` with the
    /// title off.
    fn title_w(&self) -> Option<usize> {
        if !self.on.title {
            None
        } else if self.idw > 0 {
            Some(self.taskw.saturating_sub(self.idw + 1))
        } else {
            Some(self.taskw)
        }
    }

    /// Where a row's title starts, in columns from the row's left: past
    /// the id. `None` with the title off.
    fn title_x(&self) -> Option<usize> {
        self.title_w()?;
        Some(self.x_task + if self.idw > 0 { self.idw + 1 } else { 0 })
    }

    /// The column of a row's fold toggle ([`Fold::glyph`]), when it has
    /// one and the title is shown.
    fn fold_x(&self, line: &Line) -> Option<usize> {
        line.fold.as_ref()?;
        Some(self.title_x()? + NEST_W * line.depth)
    }

    /// A click in `line`'s cell `col` is on its fold toggle — the glyph or
    /// the space after it: the fold it toggles ([`Fold::ident`]).
    fn fold_hit(&self, line: &Line, col: usize) -> Option<String> {
        let x = self.fold_x(line)?;
        (col == x || col == x + 1).then(|| line.fold.as_ref().map(|f| f.ident.clone()))?
    }

    /// A row's cells as drawn: each one's text and the column it starts at.
    /// Always the same seven, in this order ([`ETA_SEG`], [`LEASE_SEG`]); a
    /// column switched off is an empty cell.
    fn segments(&self, line: &Line) -> Vec<(usize, String)> {
        let on = &self.on;
        let idw = self.idw;
        // The id is never cut; the title fits in what is left
        // ([`fit_title`]).
        let title = |w: usize| fit_title(line, self.nested, w);
        let task = match self.title_w() {
            None => line.id.clone(),
            Some(w) if idw > 0 => format!("{:<idw$} {}", line.id, title(w)),
            Some(w) => title(w),
        };
        let cell = |shown: bool, at: usize, text: String| {
            if shown {
                (at, text)
            } else {
                (0, String::new())
            }
        };
        // A row with no tokens (Next up) lends NOW the TOKENS column:
        // `queued for 3G behind demo#12` is longer than NOW.
        let now_w = if line.tokens.is_empty() && on.tokens {
            NOW_W + GAP + TOK_W
        } else {
            NOW_W
        };
        vec![
            cell(on.stage, 0, stage_word(line.stage).to_string()),
            cell(self.taskw > 0, self.x_task, task),
            // Right-aligned: the text ends at the column's last cell.
            cell(
                on.elapsed,
                right_at(self.x_el_end, &line.elapsed),
                line.elapsed.clone(),
            ),
            cell(
                on.eta,
                right_at(self.x_eta_end, eta_cell(line)),
                eta_cell(line).into(),
            ),
            cell(on.now, self.x_now, cut(&line.now, now_w)),
            cell(
                on.tokens,
                right_at(self.x_tok_end, &line.tokens),
                line.tokens.clone(),
            ),
            cell(self.usew > 0, self.x_use, cut(&line.usage, self.usew)),
        ]
    }
}

/// How far each level of nesting moves a title in (giverny#464).
const NEST_W: usize = 3;

/// The least of a title kept beside a folded row's note before the note
/// shortens: `Work …` (giverny#464).
const TITLE_KEEP: usize = 6;

/// A row's title cell, `w` cells wide (giverny#464): its nesting, the
/// title, and a folded row's note, never wider than `w`. The nesting is
/// never cut ([`Cols::shown`] leaves it room). As the cell narrows the
/// title gives way first, down to [`TITLE_KEEP`]; then the note shortens
/// (`+3, 2 running` to `+3`); then it drops and the title takes it all.
fn fit_title(line: &Line, nested: bool, w: usize) -> String {
    let head = nest_head(line, nested);
    let hw = head.chars().count();
    let keep = line.title.chars().count().min(TITLE_KEEP);
    let notes = line
        .fold
        .as_ref()
        .filter(|f| f.folded)
        .map(|f| [f.note(), f.short_note()])
        .unwrap_or_default();
    for note in notes.iter().filter(|n| !n.is_empty()) {
        let tail = format!("  {note}");
        let used = hw + tail.chars().count();
        if used + keep <= w {
            return format!("{head}{}{tail}", cut(&line.title, w - used));
        }
    }
    cut(
        &format!("{head}{}", cut(&line.title, w.saturating_sub(hw))),
        w,
    )
}

/// What a title is drawn behind (giverny#464): its level's indent, then its
/// fold toggle, or `└` for a nested row with nothing under it. A row of its
/// own with nothing under it has none — but leaves the toggle's room while
/// another row has one (`nested`), so the titles stay aligned.
fn nest_head(line: &Line, nested: bool) -> String {
    let indent = " ".repeat(NEST_W * line.depth);
    match (&line.fold, line.depth) {
        (Some(f), _) => format!("{indent}{} ", f.glyph()),
        // The toggle's room: the glyph and the space after it.
        (None, 0) if nested => "  ".into(),
        (None, 0) => String::new(),
        (None, _) => format!("{indent}└ "),
    }
}

/// The ETA cell's text: the row's own, or [`NO_ETA`] for a Running row
/// nobody estimated.
fn eta_cell(line: &Line) -> &str {
    if line.no_eta { NO_ETA } else { &line.eta }
}

/// A row's text as it reads on screen, one character a cell: what a
/// selection over it copies.
fn compose(segments: &[(usize, String)]) -> String {
    let mut buf: Vec<char> = Vec::new();
    for (at, s) in segments {
        for (i, ch) in s.chars().enumerate() {
            let k = at + i;
            if buf.len() <= k {
                buf.resize(k + 1, ' ');
            }
            buf[k] = ch;
        }
    }
    let s: String = buf.into_iter().collect();
    s.trim_end().to_string()
}

// ---------------------------------------------------------- selection ----

/// A drag over the pane's text, in `(row, column)` cells —
/// rows count the table's lines, then its footer. The terminal's own model:
/// a stream from where the drag began to where the pointer is, copied when
/// the drag ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (usize, usize),
    pub head: (usize, usize),
}

impl Selection {
    fn ordered(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// The columns selected on `row`, a row `len` characters long; `None`
    /// when none are.
    fn span(&self, row: usize, len: usize) -> Option<(usize, usize)> {
        let ((r0, c0), (r1, c1)) = self.ordered();
        if row < r0 || row > r1 {
            return None;
        }
        let a = if row == r0 { c0.min(len) } else { 0 };
        let b = if row == r1 { c1.min(len) } else { len };
        (a < b).then_some((a, b))
    }
}

/// The text `sel` covers in `rows`, line by line, each line's trailing
/// blanks dropped: plain text, as it pastes.
pub fn selected_text(rows: &[String], sel: &Selection) -> String {
    let ((r0, _), (r1, _)) = sel.ordered();
    let mut out: Vec<String> = Vec::new();
    for (r, row) in rows.iter().enumerate().take(r1 + 1).skip(r0) {
        let len = row.chars().count();
        let piece: String = match sel.span(r, len) {
            Some((a, b)) => row.chars().skip(a).take(b - a).collect(),
            None => String::new(),
        };
        out.push(piece.trim_end().to_string());
    }
    out.join("\n")
}

/// What the pointer did to the rows this frame.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct RowsInput {
    /// A plain click: the row it landed on.
    clicked: Option<usize>,
    /// The cell a plain click landed in, counted from the rows' left.
    click_col: Option<usize>,
    /// The row under the pointer, while it is not dragging.
    hovered: Option<usize>,
    /// The pointer is held on a row, not (yet) dragging.
    pressed: bool,
    dragging: bool,
    /// Text put on the clipboard this frame.
    copied: Option<String>,
}

/// The cell under `p`, for rows at `rects` in cells `cw` wide: a point
/// above the first row or below the last is on that row, and a column
/// is the cell boundary nearest to it.
fn cell_at(rects: &[egui::Rect], cw: f32, p: egui::Pos2) -> Option<(usize, usize)> {
    let first = rects.first()?;
    let row = rects
        .iter()
        .position(|r| p.y < r.max.y)
        .unwrap_or(rects.len() - 1);
    let col = ((p.x - first.left()) / cw.max(1.0)).round().max(0.0) as usize;
    Some((row, col))
}

/// The pane's pointer handling over rows at `rects` reading `texts`: a
/// click (press and release in place) opens a row, a drag selects text and
/// copies it as it ends, the terminal's copy-on-select; `Ctrl+Shift+C` copies
/// the selection again. Only a drag selects, and a drag is never a click.
fn rows_input(
    ui: &mut Ui,
    id: egui::Id,
    rects: &[egui::Rect],
    texts: &[String],
    cw: f32,
    sel: &mut Option<Selection>,
) -> Option<(RowsInput, egui::Response)> {
    let mut out = RowsInput::default();
    let area = rects.iter().copied().reduce(|a, b| a.union(b))?;
    // Any new press ends the last selection: it becomes a click, or a new
    // drag, or it is somewhere else entirely — as in the terminal.
    if ui.input(|i| i.pointer.primary_pressed()) {
        *sel = None;
    }
    let resp = ui.interact(area, id, Sense::click_and_drag());
    let at = |p: Option<egui::Pos2>| p.and_then(|p| cell_at(rects, cw, p));
    if resp.drag_started_by(egui::PointerButton::Primary) {
        let origin = ui.input(|i| i.pointer.press_origin());
        if let (Some(a), Some(h)) = (at(origin), at(resp.interact_pointer_pos())) {
            *sel = Some(Selection { anchor: a, head: h });
        }
    } else if resp.dragged_by(egui::PointerButton::Primary)
        && let (Some(s), Some(h)) = (sel.as_mut(), at(resp.interact_pointer_pos()))
    {
        s.head = h;
    }
    out.dragging = resp.dragged();
    let mut copy = false;
    if resp.drag_stopped_by(egui::PointerButton::Primary) {
        copy = true;
    }
    // Ctrl+Shift+C (Cmd+C on macOS) with a selection here is the pane's,
    // taken before the terminal can read it; plain Ctrl+C stays an
    // interrupt, as it is in the terminal.
    if sel.is_some() {
        let chord = ui.input_mut(|i| {
            let shift = i.modifiers.shift;
            let mut hit = false;
            i.events.retain(|e| {
                let ours = matches!(e, egui::Event::Copy | egui::Event::Cut)
                    && giverny_term::input::clipboard_chord(
                        matches!(e, egui::Event::Cut),
                        shift,
                        cfg!(target_os = "macos"),
                    ) == giverny_term::input::ClipboardChord::CopySelection;
                hit |= ours;
                !ours
            });
            hit
        });
        copy |= chord;
    }
    if copy && let Some(s) = sel.as_ref() {
        let text = selected_text(texts, s);
        if !text.is_empty() {
            ui.ctx().copy_text(text.clone());
            out.copied = Some(text);
        }
    }
    if resp.clicked() {
        *sel = None;
        let p = resp.interact_pointer_pos();
        out.clicked = at(p).map(|(r, _)| r);
        out.click_col = p
            .zip(rects.first())
            .map(|(p, first)| ((p.x - first.left()) / cw.max(1.0)).floor().max(0.0) as usize);
    }
    if !out.dragging {
        out.hovered = resp
            .hover_pos()
            .filter(|p| area.contains(*p))
            .and_then(|p| at(Some(p)))
            .map(|(r, _)| r);
        out.pressed = resp.is_pointer_button_down_on();
    }
    if out.dragging {
        ui.ctx().set_cursor_icon(CursorIcon::Text);
    }
    Some((out, resp))
}

#[allow(clippy::too_many_arguments)]
fn draw_table(
    ui: &mut Ui,
    id: egui::Id,
    table: &Table,
    shown: &PaneColumns,
    viewed: Option<Shown<'_>>,
    flash: Option<(&HashSet<String>, f32)>,
    animate: bool,
    chrome: &Chrome,
    shared: &mut RenderShared,
    cell: egui::Vec2,
    row_h: f32,
    cols: usize,
    sel: &mut Option<Selection>,
) -> Drew {
    let cw = cell.x.max(1.0);
    let layout = Cols::shown(table, cols, shown);
    let mut segs: Vec<Vec<(usize, String)>> =
        table.lines.iter().map(|l| layout.segments(l)).collect();
    if let Some(footer) = &table.footer {
        segs.push(vec![(0, cut(footer, cols))]);
    }
    let texts: Vec<String> = segs.iter().map(|s| compose(s)).collect();
    let rects: Vec<egui::Rect> = segs
        .iter()
        .map(|_| {
            ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), Sense::hover())
                .0
        })
        .collect();
    let Some((input, resp)) = rows_input(ui, id, &rects, &texts, cw, sel) else {
        return Drew {
            rects,
            ..Drew::default()
        };
    };
    let n = table.lines.len();
    let hovered_line = input.hovered.filter(|&r| r < n);
    if hovered_line.is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    // Every row's hover reads its whole title and why it is where it is
    // (giverny#350, giverny#423). Every row shares the one response, so the
    // tooltip is anchored to the hovered row's rect: it sits under that
    // row, and moving to another row keeps it open (same id, the pointer
    // still inside) with the new text. egui keeps it closed while a drag
    // selects or just after a click.
    if let Some(i) = hovered_line {
        let mut row = resp.clone();
        row.rect = rects[i];
        row.interact_rect = rects[i];
        row.on_hover_text(table.lines[i].tip());
    }
    let sel_fill = ui.visuals().selection.bg_fill;
    let p = ui.painter().clone();

    for (i, rect) in rects.iter().copied().enumerate() {
        let line = table.lines.get(i);
        let color = line.map_or(chrome.dim, |l| stage_color(l.stage));
        if let Some(line) = line {
            // The worker whose view the terminal above shows:
            // held lit, with a bar at its left edge, as a selection.
            let on_view = viewed.is_some_and(|v| v.is(&line.click));
            if on_view {
                p.rect_filled(rect, 2.0, color.gamma_multiply(VIEWED_TINT));
                // In the pane's left margin, clear of the text.
                let bar = egui::Rect::from_min_size(
                    rect.min - egui::vec2(6.0, 0.0),
                    egui::vec2(3.0, rect.height()),
                );
                p.rect_filled(bar, 1.0, color);
            }
            let here = hovered_line == Some(i);
            if let Some(alpha) = row_tint(here, here && input.pressed) {
                p.rect_filled(rect, 2.0, color.gamma_multiply(alpha));
            }
            // Landed since the last look: its frame breathes with the
            // others' (giverny#394). Painted only, over the row and a hair
            // out to its left so it clears the first glyph: nothing moves.
            // Kept moving only while the row is on screen.
            if let Some((rows, glow)) = flash
                && line.stage == Stage::Done
                && rows.contains(&done_ident(&line.click))
            {
                p.rect_stroke(
                    rect.with_min_x(rect.min.x - FLASH_OUT),
                    2.0,
                    egui::Stroke::new(FLASH_WIDTH, color.gamma_multiply(glow)),
                    egui::StrokeKind::Inside,
                );
                if animate {
                    crate::rail::keep_animating(ui, rect);
                }
            }
        }
        // The selected text, behind the glyphs.
        if let Some(s) = sel.as_ref()
            && let Some((a, b)) = s.span(i, texts[i].chars().count())
        {
            let r = egui::Rect::from_min_max(
                egui::pos2(rect.left() + a as f32 * cw, rect.top()),
                egui::pos2(rect.left() + b as f32 * cw, rect.bottom()),
            );
            p.rect_filled(r, 0.0, sel_fill);
        }
        let top = rect.center().y - cell.y / 2.0;
        for (k, (at, s)) in segs[i].iter().enumerate() {
            let at = egui::pos2(rect.left() + *at as f32 * cw, top);
            let missing = k == ETA_SEG && line.is_some_and(|l| l.no_eta);
            // The use cell takes its row's colour, but a run killed by the
            // memory cap is flagged in amber, as a missing ETA is in dim.
            let oom = k == LEASE_SEG && line.is_some_and(|l| l.oom);
            // So is a Running row that nothing runs.
            let flag = k == NOW_SEG && line.is_some_and(|l| l.flag);
            let ink = if missing {
                chrome.dim
            } else if oom || flag {
                chrome.amber
            } else {
                color
            };
            if k == LEASE_SEG
                && let Some(line) = line
            {
                // A Running row's figures each by their weight on the
                // machine (giverny#305).
                for (col, run, ink) in usage_inks(s, line.usage_load, ink, chrome) {
                    shared.paint_text(&p, at + egui::vec2(col as f32 * cw, 0.0), &run, ink);
                }
                continue;
            }
            shared.paint_text(&p, at, s, ink);
        }
    }
    // A click on a row's fold toggle, or the space after it, folds or
    // unfolds what hangs under it (giverny#464) and opens nothing.
    let clicked_line = input.clicked.and_then(|i| table.lines.get(i));
    let fold = clicked_line.and_then(|l| layout.fold_hit(l, input.click_col?));
    let clicked = clicked_line
        .filter(|_| fold.is_none())
        .map(|l| l.click.clone());
    let hover = input
        .hovered
        .map(|i| table.lines.get(i).map(|l| l.click.clone()));
    Drew {
        clicked,
        rects,
        hover,
        fold,
    }
}

/// What [`draw_table`] saw this frame.
#[derive(Default)]
struct Drew {
    /// The row a click opens.
    clicked: Option<RowClick>,
    /// Where each row was drawn.
    rects: Vec<egui::Rect>,
    /// The pointer over the rows: `Some(None)` on the pane but on no row.
    hover: Option<Option<RowClick>>,
    /// A click on a row's fold toggle: that row's [`Fold::ident`].
    fold: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use giverny_claude::subagents::LiveSnapshot;

    const T0: u64 = 1_790_000_000_000; // an epoch-ms "now"

    fn feed(json: &str) -> Feed {
        feed::parse(json.as_bytes()).expect("test feed parses")
    }

    fn live(json: &str) -> Vec<SubagentRow> {
        let mut t = Tracker::new(None);
        t.apply_live(&LiveSnapshot::parse(json), T0);
        t.rows().to_vec()
    }

    fn row(key: &str, agent: Option<&str>, stage: Stage) -> RowClick {
        RowClick {
            stage,
            key: key.into(),
            agent_id: agent.map(Into::into),
            finished: false,
            asleep: false,
            name: "w".into(),
            transcript: None,
            open: None,
            brief: None,
            note: None,
            review: None,
            facts: vec![],
        }
    }

    #[test]
    fn clicking_the_viewed_workers_row_goes_home() {
        let r = row("demo#1", Some("a1"), Stage::Running);
        assert_eq!(toggle(&r, Some("a1"), None), Toggle::Home);
        // The worker finished while it was viewed: still its row.
        let done = row("demo#1", Some("a1"), Stage::Done);
        assert_eq!(toggle(&done, Some("a1"), None), Toggle::Home);
    }

    #[test]
    fn clicking_another_row_while_viewing_opens_it() {
        let r = row("demo#2", Some("b2"), Stage::Running);
        assert_eq!(toggle(&r, Some("a1"), None), Toggle::Open);
        let planned = row("demo#3", None, Stage::Planned);
        assert_eq!(toggle(&planned, Some("a1"), None), Toggle::Open);
    }

    #[test]
    fn clicking_with_nothing_viewed_opens() {
        let r = row("demo#1", Some("a1"), Stage::Running);
        assert_eq!(toggle(&r, None, None), Toggle::Open);
        let keyless = row("", None, Stage::Done);
        assert_eq!(toggle(&keyless, None, None), Toggle::Open);
    }

    #[test]
    fn clicking_the_row_whose_overlay_is_up_closes_it() {
        let done = row("demo#1", Some("a1"), Stage::Done);
        assert_eq!(toggle(&done, None, Some(&done)), Toggle::Close);
        // The overlay was opened while the row was Running.
        let was = row("demo#1", Some("a1"), Stage::Running);
        assert_eq!(toggle(&done, None, Some(&was)), Toggle::Close);
        let other = row("demo#2", Some("b2"), Stage::Done);
        assert_eq!(toggle(&other, None, Some(&done)), Toggle::Open);
    }

    #[test]
    fn rows_without_keys_are_told_apart_by_id_then_name() {
        let a = row("", Some("a1"), Stage::Done);
        let b = row("", Some("b2"), Stage::Done);
        assert!(a.same_row(&a.clone()));
        assert!(!a.same_row(&b));
        let mut n1 = row("", None, Stage::Done);
        let mut n2 = n1.clone();
        n1.name = "one".into();
        n2.name = "two".into();
        assert!(!n1.same_row(&n2));
        assert!(!row("k", None, Stage::Done).same_row(&row("", None, Stage::Done)));
    }

    #[test]
    fn stopwatch_writes_only_the_digits_it_needs() {
        assert_eq!(stopwatch(42), "0:42");
        assert_eq!(stopwatch(14 * 60 + 5), "14:05");
        assert_eq!(stopwatch(3600 + 3 * 60 + 7), "1:03:07");
        assert_eq!(stopwatch(27 * 3600), "27:00:00");
    }

    #[test]
    fn countdown_goes_through_zero() {
        assert_eq!(countdown(20 * 60), "~20m");
        assert_eq!(countdown(63 * 60), "~1h3m");
        assert_eq!(countdown(-12 * 60), "-~12m");
        assert_eq!(countdown(-10), "~0m");
    }

    #[test]
    fn tokens_read_like_claude_codes_own() {
        assert_eq!(fmt_tokens(842), "842");
        assert_eq!(fmt_tokens(13_500), "13.5k");
        assert_eq!(fmt_tokens(124_800), "124.8k");
        assert_eq!(fmt_tokens(999_950), "1M");
        assert_eq!(fmt_tokens(1_234_567), "1.2M");
        assert_eq!(fmt_tokens(2_000), "2k");
    }

    #[test]
    fn a_live_worker_with_no_feed_is_a_running_row() {
        let rows = live(
            r#"{"session_id":"s","tasks":[{"id":"a1","status":"running",
                "description":"Fix the board","startTime":1789999958000,
                "tokenCount":64100,"label":"Editing src/lib.rs"}]}"#,
        );
        let t = build(None, &rows, T0);
        assert_eq!(t.lines.len(), 1);
        let l = &t.lines[0];
        assert_eq!(l.stage, Stage::Running);
        assert_eq!(l.title, "Fix the board");
        assert_eq!(l.elapsed, "0:42");
        assert_eq!(l.now, "Editing src/lib.rs");
        assert_eq!(l.tokens, "64.1k");
        assert_eq!(l.eta, "");
        assert_eq!(l.click.agent_id.as_deref(), Some("a1"));
    }

    /// Live rows from `(id, status, start offset s, parent)`: each named
    /// `Work <id>`, its parent as its meta would give it (giverny#464).
    fn family(rows: &[(&str, &str, u64, Option<&str>)]) -> Vec<SubagentRow> {
        let tasks: Vec<String> = rows
            .iter()
            .map(|(id, status, at, _)| {
                format!(
                    r#"{{"id":"{id}","status":"{status}","description":"Work {id}",
                       "startTime":{},"tokenCount":1000}}"#,
                    T0 - 600_000 + at * 1000
                )
            })
            .collect();
        let mut out = live(&format!(
            r#"{{"session_id":"s","tasks":[{}]}}"#,
            tasks.join(",")
        ));
        for r in &mut out {
            let (.., parent) = rows.iter().find(|(id, ..)| *id == r.id).unwrap();
            r.parent_id = parent.map(str::to_string);
        }
        out
    }

    fn titles(t: &Table) -> Vec<(String, usize, Stage)> {
        t.lines
            .iter()
            .map(|l| (l.title.clone(), l.depth, l.stage))
            .collect()
    }

    fn drawn_titles(t: &Table) -> Vec<String> {
        let cols = Cols::new(t, 160);
        t.lines
            .iter()
            .map(|l| cols.segments(l)[1].1.trim_end().to_string())
            .collect()
    }

    #[test]
    fn an_agent_a_worker_spawned_hangs_under_its_row() {
        let rows = family(&[
            ("w1", "running", 0, None),
            ("w2", "running", 10, None),
            ("c1", "running", 20, Some("w1")),
            ("c2", "completed", 30, Some("w1")),
        ]);
        let t = build(None, &rows, T0);
        assert_eq!(
            titles(&t),
            [
                ("Work w1".into(), 0, Stage::Running),
                ("Work c1".into(), 1, Stage::Running),
                ("Work c2".into(), 1, Stage::Done),
                ("Work w2".into(), 0, Stage::Running),
            ]
        );
        let fold = t.lines[0].fold.as_ref().expect("a toggle on the worker");
        assert_eq!((fold.under, fold.running, fold.folded), (2, 1, false));
        assert!(t.nested);
        // Each with its own state, time and tokens; the worker's count is
        // its own, not its agents' too.
        let c2 = &t.lines[2];
        assert_eq!(c2.stage, Stage::Done);
        assert_eq!(c2.tokens, "1k");
        assert_eq!(t.lines[0].tokens, "1k");
        assert_eq!(t.lines[1].elapsed, "9:40");
        // Indented under the toggle; a row of its own keeps the toggle's
        // room.
        assert_eq!(
            drawn_titles(&t),
            ["▾ Work w1", "   └ Work c1", "   └ Work c2", "  Work w2"]
        );
        assert!(t.lines[1].tip().contains("spawned by Work w1"));
        assert!(
            t.lines[0]
                .tip()
                .contains("2 agents of its own under it, 1 running")
        );
        // A click on it is any worker's: its own id and transcript.
        assert_eq!(t.lines[1].click.agent_id.as_deref(), Some("c1"));
        assert_eq!(t.lines[2].click.stage, Stage::Done);
    }

    #[test]
    fn nesting_goes_as_deep_as_the_spawns() {
        let rows = family(&[
            ("w1", "running", 0, None),
            ("c1", "running", 20, Some("w1")),
            ("g1", "running", 30, Some("c1")),
            ("g2", "completed", 40, Some("c1")),
        ]);
        let t = build(None, &rows, T0);
        assert_eq!(
            titles(&t),
            [
                ("Work w1".into(), 0, Stage::Running),
                ("Work c1".into(), 1, Stage::Running),
                ("Work g1".into(), 2, Stage::Running),
                ("Work g2".into(), 2, Stage::Done),
            ]
        );
        assert_eq!(
            drawn_titles(&t),
            [
                "▾ Work w1",
                "   ▾ Work c1",
                "      └ Work g1",
                "      └ Work g2"
            ]
        );
        let w1 = t.lines[0].fold.as_ref().unwrap();
        assert_eq!((w1.under, w1.running), (3, 2), "every level down");
        assert!(t.lines[2].tip().contains("spawned by Work c1"));
    }

    #[test]
    fn an_agent_whose_spawner_has_no_row_is_a_row_of_its_own() {
        let rows = family(&[
            ("w1", "running", 0, None),
            ("c9", "running", 20, Some("gone")),
        ]);
        let t = build(None, &rows, T0);
        assert_eq!(
            titles(&t),
            [
                ("Work w1".into(), 0, Stage::Running),
                ("Work c9".into(), 0, Stage::Running),
            ]
        );
        assert!(!t.nested && t.lines.iter().all(|l| l.fold.is_none()));
        assert_eq!(drawn_titles(&t), ["Work w1", "Work c9"], "drawn as today");
        // A loop of parents (never seen) is no nesting either.
        let rows = family(&[
            ("a", "running", 0, Some("b")),
            ("b", "running", 10, Some("a")),
        ]);
        let t = build(None, &rows, T0);
        assert_eq!(t.lines.len(), 2);
    }

    #[test]
    fn a_done_workers_running_agent_stays_under_it() {
        let rows = family(&[
            ("w1", "completed", 0, None),
            ("w2", "running", 10, None),
            ("c1", "running", 20, Some("w1")),
        ]);
        let t = build(None, &rows, T0);
        assert_eq!(
            titles(&t),
            [
                ("Work w2".into(), 0, Stage::Running),
                ("Work w1".into(), 0, Stage::Done),
                ("Work c1".into(), 1, Stage::Running),
            ]
        );
        let fold = t.lines[1].fold.as_ref().unwrap();
        assert_eq!((fold.under, fold.running), (1, 1));
        // Its own stage word and ticking clock, under a Done row.
        assert_eq!(t.lines[2].elapsed, "9:40");
    }

    #[test]
    fn a_folded_worker_says_what_it_hides() {
        let rows = family(&[
            ("w1", "running", 0, None),
            ("c1", "running", 20, Some("w1")),
            ("c2", "completed", 30, Some("w1")),
            ("g1", "running", 40, Some("c1")),
            ("w2", "completed", 50, None),
            ("d1", "completed", 60, Some("w2")),
        ]);
        let folded: HashSet<String> = ["a:w1".to_string(), "a:w2".to_string()].into();
        let (clock, logs) = (Clock::plain(), Logs::new());
        let t = build_with(
            None,
            &rows,
            T0,
            &clock,
            &logs,
            DoneRows::All,
            Drawn::Every,
            &folded,
        );
        assert_eq!(
            titles(&t),
            [
                ("Work w1".into(), 0, Stage::Running),
                ("Work w2".into(), 0, Stage::Done),
            ]
        );
        let w1 = t.lines[0].fold.as_ref().unwrap();
        assert_eq!((w1.under, w1.running, w1.folded), (3, 2, true));
        assert_eq!(
            drawn_titles(&t),
            ["▸ Work w1  +3, 2 running", "▸ Work w2  +1"]
        );
        assert!(
            t.lines[0]
                .tip()
                .contains("3 agents of its own folded under it, 2 running")
        );
        // The toggle is the glyph, or the space after it; the rest of the
        // row opens it.
        let cols = Cols::new(&t, 160);
        let x = cols.title_x().unwrap();
        assert_eq!(cols.fold_hit(&t.lines[0], x).as_deref(), Some("a:w1"));
        assert_eq!(cols.fold_hit(&t.lines[0], x + 1).as_deref(), Some("a:w1"));
        assert_eq!(cols.fold_hit(&t.lines[0], x + 2), None);
        assert_eq!(cols.fold_hit(&t.lines[0], x - 1), None);
        // The title gives way to the note, then the note shortens
        // ([`folded_rows_never_overlap_at_any_width`]).
        let narrow = Cols::new(&t, 40);
        let cell = narrow.segments(&t.lines[0])[1].1.trim_end().to_string();
        assert_eq!(cell, "▸ Work …  +3");
        // Folding one leaves the other's open.
        let folded: HashSet<String> = ["a:c1".to_string()].into();
        let t = build_with(
            None,
            &rows,
            T0,
            &clock,
            &logs,
            DoneRows::All,
            Drawn::Every,
            &folded,
        );
        assert_eq!(t.lines.len(), 5, "only g1 is folded away");
        assert_eq!(
            t.lines[1].fold.as_ref().map(Fold::note).as_deref(),
            Some("+1, 1 running")
        );
    }

    /// Each drawn piece of a row — every cell, and in TASK the id, the
    /// nesting, the title and the note — as `(start, end, text)` columns.
    fn pieces(cols: &Cols, line: &Line) -> Vec<(usize, usize, String)> {
        let mut out = Vec::new();
        for (k, (at, text)) in cols.segments(line).into_iter().enumerate() {
            let text = text.trim_end().to_string();
            if text.is_empty() {
                continue;
            }
            if k != 1 {
                out.push((at, at + text.chars().count(), text));
                continue;
            }
            // TASK: the id, then the nesting's glyph, then the title and
            // the note, drawn two blanks apart.
            let chars: Vec<char> = text.chars().collect();
            let piece = |from: usize, to: usize, out: &mut Vec<_>| {
                let to = to.min(chars.len());
                let p: String = chars.get(from..to).unwrap_or(&[]).iter().collect();
                let lead = p.chars().take_while(|c| *c == ' ').count();
                let p = p.trim().to_string();
                if !p.is_empty() {
                    out.push((at + from + lead, at + from + lead + p.chars().count(), p));
                }
            };
            let mut i = 0;
            if cols.idw > 0 {
                piece(0, cols.idw, &mut out);
                i = cols.idw + 1;
            }
            if cols.title_w().is_some() {
                let hw = nest_head(line, cols.nested).chars().count();
                piece(i, i + hw, &mut out);
                i += hw;
                let body: String = chars.get(i..).unwrap_or(&[]).iter().collect();
                for part in body.split("  ") {
                    piece(i, i + part.chars().count(), &mut out);
                    i += part.chars().count() + 2;
                }
            }
        }
        out
    }

    #[test]
    fn folded_rows_never_overlap_at_any_width() {
        // A folded worker of its own, and an open one whose agent is
        // folded under it at depth 1 with one of its own at depth 2.
        let rows = family(&[
            ("w1", "running", 0, None),
            ("c1", "running", 20, Some("w1")),
            ("c2", "completed", 30, Some("w1")),
            ("g1", "running", 40, Some("c1")),
            ("w2", "running", 50, None),
            ("d1", "running", 60, Some("w2")),
            ("e1", "running", 70, Some("d1")),
            ("f1", "running", 80, Some("e1")),
        ]);
        let folded: HashSet<String> = ["a:w1".to_string(), "a:e1".to_string()].into();
        let (clock, logs) = (Clock::plain(), Logs::new());
        let mut t = build_with(
            None,
            &rows,
            T0,
            &clock,
            &logs,
            DoneRows::All,
            Drawn::Every,
            &folded,
        );
        assert_eq!(
            titles(&t).iter().map(|(_, d, _)| *d).collect::<Vec<_>>(),
            [0, 0, 1, 2],
            "w1 folded; w2, d1, e1 (folded) open"
        );
        assert!(t.lines[0].fold.as_ref().unwrap().folded);
        assert!(t.lines[3].fold.as_ref().unwrap().folded);
        for l in &mut t.lines {
            l.title = format!("FEATURE: panel: {} sits under its worker's row", l.title);
        }
        let no_id = t.clone();
        for l in &mut t.lines {
            l.id = "giverny#464".into();
        }
        let title_off = PaneColumns {
            title: false,
            ..PaneColumns::default()
        };
        let mut seen = HashSet::new();
        for (table, on) in [
            (&t, PaneColumns::default()),
            (&no_id, PaneColumns::default()),
            (&t, title_off),
        ] {
            // From wide down to the pane's least ([`table_cols`]).
            for w in (40..=200).rev() {
                let cols = Cols::shown(table, w, &on);
                for line in &table.lines {
                    let ps = pieces(&cols, line);
                    let mut spans = ps.clone();
                    spans.sort();
                    for pair in spans.windows(2) {
                        assert!(
                            pair[0].1 < pair[1].0,
                            "at {w} cols, {:?} runs into {:?}: {:?}",
                            pair[0].2,
                            pair[1].2,
                            compose(&cols.segments(line)),
                        );
                    }
                    let task = &cols.segments(line)[1].1;
                    assert!(task.chars().count() <= cols.taskw, "{task:?} at {w}");
                    if on.title {
                        // The nesting is whole and some title is left.
                        let head = nest_head(line, cols.nested);
                        assert!(task.contains(&head), "{task:?} at {w}");
                        assert!(task.contains("FEATU"), "{task:?} at {w}");
                    }
                    if let Some(f) = line.fold.as_ref().filter(|f| f.folded) {
                        let note = ps.iter().find(|p| p.2.starts_with('+'));
                        let state = match note.map(|p| p.2.as_str()) {
                            Some(n) if n == f.note() && n != f.short_note() => "whole",
                            Some(n) if n == f.short_note() => "short",
                            Some(n) => panic!("a cut note {n:?} at {w}"),
                            None => "dropped",
                        };
                        // TASK always keeps room for the short one.
                        assert!(!on.title || state != "dropped", "at {w}");
                        seen.insert((on.title, line.depth, state));
                    }
                }
            }
        }
        // Each folded row went through the steps, wide to narrow.
        for depth in [0, 2] {
            for state in ["whole", "short"] {
                assert!(
                    seen.contains(&(true, depth, state)),
                    "{depth} {state}: {seen:?}"
                );
            }
        }
    }

    #[test]
    fn the_done_setting_counts_rows_of_their_own() {
        let mut rows = family(&[
            ("old", "completed", 0, None),
            ("new", "completed", 10, None),
            ("c-old", "running", 20, Some("old")),
            ("d-old", "completed", 25, Some("old")),
            ("c-new", "completed", 30, Some("new")),
        ]);
        for r in &mut rows {
            r.ended_ms = Some(match r.id.as_str() {
                "old" => T0 - 100_000,
                "new" => T0 - 10_000,
                _ => T0 - 5_000,
            });
        }
        let (clock, logs) = (Clock::plain(), Logs::new());
        let at = |done| {
            titles(&build_with(
                None,
                &rows,
                T0,
                &clock,
                &logs,
                done,
                Drawn::Every,
                &HashSet::new(),
            ))
        };
        // The newest Done row of its own, with what hangs under it; the
        // older one's running agent stands on its own, its Done one goes.
        assert_eq!(
            at(DoneRows::Last(1)),
            [
                ("Work c-old".into(), 0, Stage::Running),
                ("Work new".into(), 0, Stage::Done),
                ("Work c-new".into(), 1, Stage::Done),
            ]
        );
        assert_eq!(
            at(DoneRows::Hide),
            [("Work c-old".into(), 0, Stage::Running)]
        );
        assert_eq!(at(DoneRows::All).len(), 5);
    }

    /// An agent a worker spawned that names its worker's task is still its
    /// own row under the worker, never taken for the task's worker.
    #[test]
    fn an_agent_naming_its_workers_task_is_not_its_worker() {
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"g463a","stage":"running","agent_id":"w1","started":{s},"eta_s":3600}},
              {{"key":"g470","stage":"running","started":{s},"eta_s":3600}}]}}"#,
            s = T0 - 600_000
        ));
        let mut rows = family(&[
            ("w1", "running", 0, None),
            ("c1", "running", 20, Some("w1")),
        ]);
        rows[1].description = Some("g463a triage group A, for g470".into());
        let t = build(Some(&f), &rows, T0);
        let ids: Vec<(&str, Option<&str>, usize)> = t
            .lines
            .iter()
            .map(|l| (l.click.key.as_str(), l.click.agent_id.as_deref(), l.depth))
            .collect();
        assert_eq!(
            ids,
            [
                ("g463a", Some("w1"), 0),
                ("", Some("c1"), 1),
                ("g470", None, 0),
            ]
        );
        assert_eq!(t.lines[0].fold.as_ref().unwrap().ident, "k:g463a");
    }

    #[test]
    fn a_running_row_with_no_estimate_says_so() {
        // A worker spawned outside a manager session: a row, but no ETA.
        let rows = live(
            r#"{"session_id":"s","tasks":[{"id":"a1","status":"running",
                "description":"acme#613 market SD graph","startTime":1789999958000}]}"#,
        );
        let t = build(None, &rows, T0);
        let l = &t.lines[0];
        assert_eq!(l.eta, "", "nothing to count down");
        assert!(l.no_eta);
        assert_eq!(eta_cell(l), NO_ETA);
        let drawn = compose(&Cols::new(&t, 100).segments(l));
        assert!(drawn.contains(NO_ETA), "{drawn}");
        assert_eq!(Cols::new(&t, 100).segments(l)[ETA_SEG].1, NO_ETA);
        let hint = &l.click.facts[1];
        assert!(hint.starts_with("no ETA — add one:"), "{hint}");
        assert_eq!(hint, "no ETA — add one: giverny-eta a1 <min>");

        // A feed row with no estimate is re-estimated in place.
        let f = feed(
            r#"{"rows":[{"key":"g#3","stage":"running","agent_id":"a1",
               "started":1789999400000}]}"#,
        );
        let t = build(Some(&f), &rows, T0);
        assert!(t.lines[0].no_eta);
        assert!(
            t.lines[0]
                .click
                .facts
                .contains(&"no ETA — add one: giverny-manage eta g#3 <min> --why scope".into())
        );
    }

    #[test]
    fn only_a_running_row_misses_an_estimate() {
        let f = feed(
            r#"{"rows":[
              {"key":"g#1","stage":"running","started":1789999400000,"eta_s":1800},
              {"key":"g#2","stage":"planned"},
              {"key":"g#3","stage":"done","started":1789990000000,"ended":1789993900000}]}"#,
        );
        let t = build(Some(&f), &[], T0);
        assert!(t.lines.iter().all(|l| !l.no_eta));
        assert!(t.lines[0].click.facts.iter().all(|x| !x.contains(NO_ETA)));
    }

    /// Four Done rows, newest landed first, under a Running and a Next up.
    // g#1 started two minutes before T0: a spawn may still come, so it is
    // drawn Running with no worker (giverny#334).
    const DONE_FOUR: &str = r#"{"rows":[
      {"key":"g#1","stage":"running","title":"one","started":1789999880000,"eta_s":1800},
      {"key":"g#2","stage":"planned","title":"two","eta_s":600},
      {"key":"g#3","stage":"done","title":"three","started":1789990000000,"ended":1789993900000},
      {"key":"g#4","stage":"done","title":"four","started":1789990000000,"ended":1789994900000},
      {"key":"g#5","stage":"done","title":"five","started":1789990000000,"ended":1789995900000},
      {"key":"g#6","stage":"done","title":"six","started":1789990000000,"ended":1789996900000}]}"#;

    /// The keys of a set of [`done_ident`]s, sorted, for asserting on.
    fn flashed(rows: &HashSet<String>) -> Vec<String> {
        let mut keys: Vec<String> = rows
            .iter()
            .map(|r| r.trim_start_matches("k:").to_string())
            .collect();
        keys.sort();
        keys
    }

    /// Focusing a ticked manager flashes the Done rows that landed since
    /// the last look, and only those (giverny#394).
    #[test]
    fn only_rows_landed_since_the_last_look_flash() {
        // The last look saw g#3 and g#4 done.
        let before = feed(
            r#"{"rows":[
          {"key":"g#3","stage":"done","title":"three","started":1789990000000,"ended":1789993900000},
          {"key":"g#4","stage":"done","title":"four","started":1789990000000,"ended":1789994900000},
          {"key":"g#5","stage":"running","title":"five","started":1789990000000,"eta_s":1800}]}"#,
        );
        let mut seen = None;
        note_seen(&mut seen, &build(Some(&before), &[], T0).lines);
        // Away, g#5 and g#6 landed: those two flash, Running and Next up
        // never do.
        let now = build(Some(&feed(DONE_FOUR)), &[], T0).lines;
        let rows = newly_done(&now, seen.as_ref(), 0);
        assert_eq!(flashed(&rows), ["g#5", "g#6"]);
        // Once drawn they are seen: nothing landed since, nothing flashes.
        note_seen(&mut seen, &now);
        assert!(newly_done(&now, seen.as_ref(), 0).is_empty());
    }

    /// A pane not drawn since Giverny started knows nothing of what was
    /// seen before: only rows that landed since the start flash.
    #[test]
    fn a_pane_never_drawn_flashes_what_landed_since_the_start() {
        let lines = build(Some(&feed(DONE_FOUR)), &[], T0).lines;
        // Started between g#4's and g#5's landings.
        let rows = newly_done(&lines, None, 1_789_995_000_000);
        assert_eq!(flashed(&rows), ["g#5", "g#6"]);
        assert!(newly_done(&lines, None, T0).is_empty());
    }

    /// The flash breathes from nothing out and back, three times, then
    /// ends unframed; with `rail.animate` off it holds steady as long.
    #[test]
    fn the_flash_breathes_a_few_times_then_ends() {
        let half = crate::rail::CALM_PERIOD / 2.0;
        assert_eq!(flash_glow(0.0, true), Some(0.0));
        assert!(flash_glow(half, true).is_some_and(|g| g > 0.99));
        assert!(flash_glow(crate::rail::CALM_PERIOD, true).is_some_and(|g| g < 0.01));
        assert!(flash_glow(FLASH_S - half, true).is_some_and(|g| g > 0.99));
        assert_eq!(flash_glow(FLASH_S, true), None, "over after three breaths");
        assert_eq!(flash_glow(FLASH_S + 60.0, true), None);
        assert_eq!(flash_glow(-1.0, true), None);
        // Still: one steady frame, the same span.
        assert_eq!(flash_glow(0.0, false), Some(1.0));
        assert_eq!(flash_glow(FLASH_S - 0.25, false), Some(1.0));
        assert_eq!(flash_glow(FLASH_S, false), None);
    }

    #[test]
    fn done_rows_are_all_none_or_the_newest_few() {
        let f = feed(DONE_FOUR);
        let keys = |done: DoneRows| -> Vec<String> {
            build_with(
                Some(&f),
                &[],
                T0,
                &Clock::plain(),
                &Logs::new(),
                done,
                Drawn::Every,
                &HashSet::new(),
            )
            .lines
            .into_iter()
            .map(|l| l.id)
            .collect()
        };
        assert_eq!(
            keys(DoneRows::All),
            ["g#1", "g#2", "g#6", "g#5", "g#4", "g#3"]
        );
        assert_eq!(keys(DoneRows::Hide), ["g#1", "g#2"]);
        assert_eq!(keys(DoneRows::Last(2)), ["g#1", "g#2", "g#6", "g#5"]);
        assert_eq!(keys(DoneRows::Last(9)), keys(DoneRows::All));
        // The config's words come to the same.
        let mut panel = ManagementPanelConfig::default();
        assert_eq!(panel.done(), DoneRows::All);
        panel.done_rows = "last".into();
        panel.done_last = 1;
        assert_eq!(keys(panel.done()), ["g#1", "g#2", "g#6"]);
        panel.done_rows = "hide".into();
        assert_eq!(keys(panel.done()), ["g#1", "g#2"]);
    }

    #[test]
    fn the_settings_list_the_columns_in_the_panes_order() {
        // Settings → Management panel shows a switch per column in `SETTINGS`'
        // order; that order must be the one the pane draws them in.
        let t = build(Some(&feed(DONE_FOUR)), &[], T0);
        let c = Cols::new(&t, 100);
        let at: Vec<(&str, usize)> = giverny_core::settings::SETTINGS
            .iter()
            .filter_map(|d| d.key.strip_prefix("management_panel.columns."))
            .map(|col| {
                let x = match col {
                    "stage" => 0,
                    "id" => c.x_task,
                    "title" => c.x_task + c.idw + 1,
                    "elapsed" => c.x_el_end,
                    "eta" => c.x_eta_end,
                    "now" => c.x_now,
                    "tokens" => c.x_tok_end,
                    "usage" => c.x_use,
                    other => panic!("a column the pane does not draw: {other}"),
                };
                (col, x)
            })
            .collect();
        assert_eq!(at.len(), 8, "{at:?}");
        assert!(at.windows(2).all(|w| w[0].1 < w[1].1), "{at:?}");
    }

    #[test]
    fn a_hidden_column_takes_no_width_and_the_rest_stay_aligned() {
        let t = build(Some(&feed(DONE_FOUR)), &[], T0);
        let all = Cols::new(&t, 100);
        let drawn: Vec<String> = t.lines.iter().map(|l| compose(&all.segments(l))).collect();
        assert!(drawn[0].starts_with("Running  g#1 one"), "{drawn:?}");

        // STAGE and ETA off: the task starts the row, and ETA's cells are
        // gone without moving anything else out of line.
        let on = PaneColumns {
            stage: false,
            eta: false,
            ..PaneColumns::default()
        };
        let cols = Cols::shown(&t, 100, &on);
        assert_eq!(cols.x_task, 0);
        assert!(cols.taskw > all.taskw, "the title takes the room");
        let drawn: Vec<String> = t.lines.iter().map(|l| compose(&cols.segments(l))).collect();
        assert!(drawn[0].starts_with("g#1 one"), "{drawn:?}");
        for (l, d) in t.lines.iter().zip(&drawn) {
            assert!(!d.contains("Running") && !d.contains("Done"), "{d}");
            assert_eq!(cols.segments(l)[ETA_SEG].1, "", "no ETA cell");
            // Every row's TOKENS ends on the same column.
            if !l.tokens.is_empty() {
                assert!(d.ends_with(&l.tokens), "{d}");
                assert_eq!(d.chars().count(), cols.x_tok_end, "{d}");
            }
        }
        // ELAPSED ends at the same column on every row that has one.
        let el_end: Vec<usize> = t
            .lines
            .iter()
            .filter(|l| !l.elapsed.is_empty())
            .map(|l| {
                let seg = &cols.segments(l)[2];
                seg.0 + seg.1.chars().count()
            })
            .collect();
        assert!(el_end.iter().all(|e| *e == cols.x_el_end), "{el_end:?}");

        // The title off: TASK is the id alone, and the columns after it
        // close up to it.
        let on = PaneColumns {
            title: false,
            ..PaneColumns::default()
        };
        let cols = Cols::shown(&t, 100, &on);
        assert_eq!(cols.taskw, 3, "g#1");
        let d = compose(&cols.segments(&t.lines[0]));
        assert!(!d.contains("one"), "{d}");
        assert!(d.starts_with("Running  g#1  "), "{d}");

        // Everything off: nothing drawn.
        let none = PaneColumns {
            stage: false,
            id: false,
            title: false,
            usage: false,
            elapsed: false,
            eta: false,
            now: false,
            tokens: false,
        };
        let cols = Cols::shown(&t, 100, &none);
        assert!(
            t.lines
                .iter()
                .all(|l| compose(&cols.segments(l)).is_empty())
        );
    }

    #[test]
    fn every_rows_hover_reads_its_whole_title_and_why_it_is_there() {
        // giverny#423 (after giverny#350): the tooltip is the whole title,
        // cut or not, then what the row's state is and why.
        let t = build(Some(&feed(DONE_FOUR)), &[], T0);
        for l in &t.lines {
            let tip = l.tip();
            assert!(tip.starts_with(&l.title), "{tip}");
            assert!(tip.lines().count() >= 2, "a line of why: {tip}");
        }
        // A row with no title is named by its id.
        let mut l = t.lines[0].clone();
        l.title.clear();
        assert!(l.tip().starts_with(&l.id), "{}", l.tip());
    }

    #[test]
    fn a_running_rows_hover_says_who_since_when_and_how_it_goes() {
        let who = "worker Wren (opus)";
        assert_eq!(
            running_why(who, Some("14:02"), Some(1800), Some(18 * 60), None),
            [
                "Running: worker Wren (opus) since 14:02",
                "estimate 30m: ~18m left, on track"
            ]
        );
        assert_eq!(
            running_why(who, Some("14:02"), Some(1800), Some(-5 * 60), None),
            [
                "Running: worker Wren (opus) since 14:02",
                "estimate 30m: 5m over it"
            ]
        );
        // Seconds past is not over yet.
        assert_eq!(
            running_why(who, None, Some(1800), Some(-10), None)[1],
            "estimate 30m: ~0m left, on track"
        );
        let paused = paused_why("15:10", None);
        assert_eq!(paused, "paused since 15:10 — its clock is stopped");
        assert_eq!(
            running_why("no worker on it", None, None, None, Some(paused)),
            [
                "Running: no worker on it",
                "paused since 15:10 — its clock is stopped",
                "no estimate yet"
            ]
        );
        assert_eq!(
            paused_why("15:10", Some("waits for the slot")),
            "paused since 15:10: waits for the slot"
        );
    }

    #[test]
    fn a_running_row_with_a_live_worker_names_it_in_its_hover() {
        let rows = live(&format!(
            r#"{{"session_id":"s","tasks":[{{"id":"w","status":"running",
                "description":"Work acme#9","startTime":{}}}]}}"#,
            T0 - 10 * 60_000
        ));
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"acme#9","stage":"running","agent_id":"w","started":{s},
                "eta_s":1800,"title":"a task"}}]}}"#,
            s = T0 - 10 * 60_000,
        ));
        let t = build(Some(&f), &rows, T0);
        let why = &t.lines[0].why;
        assert!(
            why[0].starts_with("Running: worker Work acme#9 since "),
            "{why:?}"
        );
        assert_eq!(why[1], "estimate 30m: ~20m left, on track");
        assert_eq!(why.len(), 2, "{why:?}");
    }

    #[test]
    fn a_next_up_rows_hover_says_why_it_waits() {
        let at = |m: u64| ts(T0 + m * 60_000);
        let json = format!(
            r#"{{"version":1,"rows":[
              {{"key":"280-reuse","stage":"running","started":"{p}","eta_s":1800}},
              {{"key":"265-x","stage":"planned","planned":"{p}","held":"your answer on the layout",
                "eta_s":1200}},
              {{"key":"282-after","stage":"planned","planned":"{p}","after":"280-reuse"}},
              {{"key":"283-y","stage":"planned","planned":"{p}",
                "lease":{{"state":"queued","position":2,"behind":"280-reuse","short":["ram"],
                          "cpu":3,"ram_mb":3072}}}},
              {{"key":"284-idle","stage":"planned","planned":"{p}"}}]}}"#,
            p = at(0),
        );
        let t = build(Some(&feed(&json)), &[], T0 + 30 * 60_000);
        let why = |k: &str| t.lines.iter().find(|l| l.id == k).unwrap().why.clone();
        assert_eq!(
            why("280-reuse"),
            [
                "no worker yet — started, but no worker has picked it up",
                "estimate ~30m"
            ]
        );
        assert_eq!(
            why("265-x"),
            [
                "held — not started; waiting for: your answer on the layout",
                "estimate ~20m"
            ]
        );
        assert_eq!(
            why("282-after"),
            ["after 280-reuse — starts when that lands"]
        );
        assert_eq!(
            why("283-y"),
            ["queued — waiting for 3G behind 280-reuse in the machine ledger"]
        );
        assert_eq!(why("284-idle"), [IDLE_WHY]);
    }

    /// The Next up keys, top to bottom.
    fn next_up(t: &Table) -> Vec<&str> {
        t.lines
            .iter()
            .filter(|l| l.stage == Stage::Planned)
            .map(|l| l.id.as_str())
            .collect()
    }

    /// giverny#442: Next up reads in the order its rows will start, the
    /// next on top, whatever order they were planned in.
    #[test]
    fn next_up_reads_in_the_order_its_rows_start() {
        let at = |m: u64| ts(T0 + m * 60_000);
        let json = format!(
            r#"{{"version":1,"rows":[
              {{"key":"501-held","stage":"planned","planned":"{p}","held":"your answer"}},
              {{"key":"502-after2","stage":"planned","planned":"{p}","after":"503-after1"}},
              {{"key":"503-after1","stage":"planned","planned":"{p}","after":"100-run"}},
              {{"key":"504-idle-a","stage":"planned","planned":"{p}"}},
              {{"key":"505-q2","stage":"planned","planned":"{p}",
                "lease":{{"state":"queued","position":2,"ram_mb":3072}}}},
              {{"key":"506-paused","stage":"running","started":"{p}","paused_since":"{paused}"}},
              {{"key":"507-q1","stage":"planned","planned":"{p}",
                "lease":{{"state":"queued","position":1,"ram_mb":3072}}}},
              {{"key":"508-granted","stage":"planned","planned":"{p}",
                "lease":{{"state":"granted","ram_mb":3072,"uncollected":true}}}},
              {{"key":"509-no-worker","stage":"running","started":"{p}"}},
              {{"key":"510-idle-b","stage":"planned","planned":"{p}"}},
              {{"key":"511-after-held","stage":"planned","planned":"{p}","after":"501-held"}},
              {{"key":"512-leased","stage":"planned","planned":"{p}",
                "lease":{{"state":"granted","ram_mb":3072,"granted_at":"{p}"}}}},
              {{"key":"513-after-done","stage":"planned","planned":"{p}","after":"099-done"}},
              {{"key":"100-run","stage":"running","started":"{fresh}"}},
              {{"key":"099-done","stage":"done","started":"{p}","ended":"{p}"}}]}}"#,
            p = at(0),
            paused = at(5),
            fresh = at(29),
        );
        let t = build(Some(&feed(&json)), &[], T0 + 30 * 60_000);
        assert_eq!(
            next_up(&t),
            [
                // Started, or granted: each waits only for its worker, in
                // plan order (a benched row, started, by its own wait).
                "509-no-worker",
                "508-granted",
                "512-leased",
                // The machine ledger's queue, #1 first.
                "507-q1",
                "505-q2",
                // Nothing to wait for, in plan order (a task whose
                // prerequisite landed waits on nothing).
                "504-idle-a",
                "510-idle-b",
                "513-after-done",
                // After another task, chained.
                "503-after1",
                "502-after2",
                // Held and paused last, and what waits on a held row after it.
                "506-paused",
                "501-held",
                "511-after-held",
            ]
        );
        let ids: Vec<&str> = t.lines.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids.first(), Some(&"100-run"), "Running stays on top");
        assert_eq!(ids.last(), Some(&"099-done"), "Done stays below");
    }

    /// A row `--after` another Next up row goes after it, even when it was
    /// planned first; a loop of `after`s keeps its rows' own places.
    #[test]
    fn a_row_after_another_next_up_row_goes_below_it() {
        let at = |m: u64| ts(T0 + m * 60_000);
        let json = format!(
            r#"{{"version":1,"rows":[
              {{"key":"c","stage":"planned","planned":"{p}","after":"b"}},
              {{"key":"b","stage":"planned","planned":"{p}","after":"a"}},
              {{"key":"a","stage":"planned","planned":"{p}",
                "lease":{{"state":"queued","position":1,"ram_mb":1024}}}},
              {{"key":"x","stage":"planned","planned":"{p}","after":"y"}},
              {{"key":"y","stage":"planned","planned":"{p}","after":"x"}}]}}"#,
            p = at(0),
        );
        let t = build(Some(&feed(&json)), &[], T0 + 30 * 60_000);
        assert_eq!(next_up(&t), ["a", "b", "c", "x", "y"]);
    }

    /// Dittos follow the rows to their new places: a worker's two Next up
    /// rows brought together by the order read as one worker's.
    #[test]
    fn next_up_rows_are_dittoed_where_they_land() {
        let f = feed(
            r#"{"rows":[
              {"key":"g#1","stage":"planned","agent_id":"a1","eta_s":600},
              {"key":"g#2","stage":"planned","agent_id":"a1","eta_s":600,
                "lease":{"state":"queued","position":1,"ram_mb":1024}},
              {"key":"g#3","stage":"planned","eta_s":600,
                "lease":{"state":"queued","position":2,"ram_mb":1024}},
              {"key":"g#4","stage":"planned","agent_id":"a1","eta_s":600}
            ]}"#,
        );
        let t = build(Some(&f), &[], T0);
        assert_eq!(next_up(&t), ["g#2", "g#3", "g#1", "g#4"]);
        let ditto: Vec<bool> = t.lines.iter().map(|l| l.eta == "\"").collect();
        assert_eq!(ditto, [false, false, false, true]);
    }

    #[test]
    fn a_done_rows_hover_says_how_it_went_against_its_estimate() {
        use feed::Against;
        assert_eq!(
            done_why("Done", "25:00", Some((300, Against::Estimate(1200))), None),
            ["Done; took 25:00, 5m over its estimate 20m"]
        );
        assert_eq!(
            done_why("Blocked", "", None, None),
            ["Done: Blocked"],
            "no clock, no estimate said"
        );
        assert_eq!(
            done_why("", "15:00", Some((-300, Against::Worker(1200))), None),
            ["Done; took 15:00, 5m under its worker's 20m"]
        );
        assert_eq!(
            done_why("done", "20:00", Some((10, Against::Given)), None),
            ["Done; took 20:00, on its estimate"]
        );
        assert_eq!(
            done_why(
                "Review — ita",
                "20:00",
                None,
                Some("ita — the hover — window PID 1")
            ),
            [
                "Done: Review — ita; took 20:00, no estimate",
                "awaiting review: ita — the hover — window PID 1"
            ]
        );
    }

    #[test]
    fn a_worker_with_an_agent_eta_counts_down_under_no_id() {
        // A plain session's two workers, five minutes in: one estimated.
        let rows = live(
            r#"{"session_id":"s","tasks":[
                {"id":"a1","status":"running","description":"Classify chunk 0",
                 "startTime":1789999700000},
                {"id":"a2","status":"running","description":"Classify chunk 1",
                 "startTime":1789999700000}]}"#,
        );
        // Given a minute after the spawn, 8 minutes left: 9 in all, 4 left.
        let at = 1_789_999_760_000;
        let etas: Etas = [(
            "a1".to_string(),
            agent_eta::Eta {
                left_s: 480,
                at_ms: at,
                first_left_s: 480,
                first_at_ms: at,
            },
        )]
        .into();
        let clock = Clock {
            etas: &etas,
            ..Clock::plain()
        };
        let t = build_at(None, &rows, T0, &clock, &Logs::new());
        let (one, two) = (&t.lines[0], &t.lines[1]);
        assert_eq!(
            (one.id.as_str(), one.title.as_str()),
            ("", "Classify chunk 0")
        );
        assert_eq!(one.eta, countdown(240));
        assert!(!one.no_eta);
        assert_eq!((two.id.as_str(), two.eta.as_str()), ("", ""));
        assert!(two.no_eta, "its own, never its sibling's");

        // Done: how it landed against that estimate.
        let done = live(
            r#"{"session_id":"s","tasks":[{"id":"a1","status":"completed",
                "description":"Classify chunk 0","startTime":1789999700000,
                "endTime":1790000000000}]}"#,
        );
        let t = build_at(None, &done, T0, &clock, &Logs::new());
        assert_eq!(t.lines[0].eta, feed::fmt_delta(300 - 540));

        // Asleep from ten seconds in until the figure (giverny#402): the
        // figure is less the work since it, the sleep in neither.
        let nap = [sleeps::Span {
            since: 1_789_999_710_000,
            until: at,
        }];
        let napped = Clock {
            sleeps: &nap,
            etas: &etas,
            ..Clock::plain()
        };
        let t = build_at(None, &rows, T0, &napped, &Logs::new());
        assert_eq!(t.lines[0].eta, countdown(240), "not ~5m");
        let t = build_at(None, &done, T0, &napped, &Logs::new());
        assert_eq!(t.lines[0].eta, feed::fmt_delta(240 - 480), "not (-5m)");

        // A feed row's estimate wins, and so does its key.
        let f = feed(r#"{"rows":[{"key":"g#3","stage":"running","agent_id":"a1","eta_s":1800}]}"#);
        let t = build_at(Some(&f), &rows[..1], T0, &clock, &Logs::new());
        assert_eq!(t.lines[0].id, "g#3");
        assert_eq!(t.lines[0].eta, countdown(1800 - 300));
    }

    /// giverny#471, poly's row `170`: paused 987 s under its first worker,
    /// resumed, and taken over by a fresh worker that marked a wait and
    /// ended its turn. Its clock counted from the new worker's start moved
    /// on by pauses that came before that worker: in the future, so 0:00
    /// and frozen; NOW was empty. Now the clock is the task's, and NOW says
    /// the turn ended, and the wait.
    #[test]
    fn a_resumed_row_taken_over_by_a_fresh_worker_reads_clearly() {
        const M: u64 = 60_000;
        let spawned = T0 - 112 * M;
        let started = spawned + 987_000;
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"170","stage":"running","agent_id":"a1","spawned":{spawned},
                "started":{started},"paused_s":987,"eta_s":33433,"left_s":28500,
                "left_at":{la},"waiting_since":{la},"wait_s":1490,
                "note":"machine time: re-sims and scorer running"}}]}}"#,
            la = T0 - 13 * M,
        ));
        let worker = |status: &str| {
            live(&format!(
                r#"{{"session_id":"s","tasks":[{{"id":"a1","status":"{status}",
                    "description":"170: finish per-minute scores",
                    "startTime":{st},"endTime":{en},"label":"Running the sims"}}]}}"#,
                st = T0 - 15 * M,
                en = T0 - 12 * M,
            ))
        };
        let tz = Clock::plain().tz;
        let ended = worker("completed");
        let t = build(Some(&f), &ended, T0);
        let l = &t.lines[0];
        assert_eq!(l.stage, Stage::Running);
        assert_eq!(
            l.elapsed,
            stopwatch((112 * 60) - 987),
            "the task's clock, moving"
        );
        // Ended when the live list first saw it so (here T0).
        assert_eq!(
            l.now,
            format!(
                "turn ended {} · waiting: machine time: re-sims and scorer running",
                clock_at(T0, T0, &tz)
            )
        );
        assert!(
            l.why
                .iter()
                .any(|w| w.contains("until its manager wakes the worker")),
            "{:?}",
            l.why
        );
        assert!(
            l.why.iter().any(|w| w.starts_with("waiting since")
                && w.ends_with(": machine time: re-sims and scorer running")),
            "the wait in full: {:?}",
            l.why
        );
        let later = build(Some(&f), &ended, T0 + 2 * M);
        assert_eq!(later.lines[0].elapsed, stopwatch((114 * 60) - 987));
        // Working again: its activity, and the hover keeps the wait.
        let working = worker("running");
        let t = build(Some(&f), &working, T0);
        assert_eq!(t.lines[0].now, "Running the sims");
        assert_eq!(t.lines[0].elapsed, stopwatch((112 * 60) - 987));
        assert!(
            t.lines[0]
                .why
                .iter()
                .any(|w| w.starts_with("waiting since")),
            "{:?}",
            t.lines[0].why
        );
    }

    /// giverny#476, poly's 170: its worker ended its turn while its own
    /// `wrangler deploy` ran on in the background. The row and its click
    /// read asleep, with the deploy and how long it has run, the wait kept,
    /// and no land chore; with two shells the first is named and `+1`;
    /// once no shell of its own is left it reads `turn ended` as before;
    /// woken, its activity.
    #[test]
    fn a_worker_asleep_on_its_own_shell_reads_asleep_not_ended() {
        const M: u64 = 60_000;
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"170","stage":"running","agent_id":"a1","started":{st},
                "eta_s":33433,"waiting_since":{w},"note":"deploy upload slow"}}]}}"#,
            st = T0 - 300 * M,
            w = T0 - 26 * M,
        ));
        let worker = |status: &str| {
            live(&format!(
                r#"{{"session_id":"s","tasks":[{{"id":"a1","status":"{status}",
                    "description":"170: finish per-minute runs",
                    "startTime":{st},"endTime":{en},"label":"Retry wrangler deploy"}}]}}"#,
                st = T0 - 300 * M,
                en = T0 - 26 * M,
            ))
        };
        let deploy = bg_shells::Shell {
            id: "bohhzzia9".into(),
            agent: "a1".into(),
            task: "170".into(),
            cmd: "cd /home/ita/poly; T=/tmp/t170; npx wrangler deploy > $T/deploy4.log 2>&1; \
                  echo \"exit $?\""
                .into(),
            started_ms: T0 - 30 * M,
        };
        let mut shells = Shells::new();
        shells.insert("a1".into(), vec![deploy.clone()]);
        let clock = Clock {
            shells: &shells,
            ..Clock::plain()
        };
        let tz = clock.tz.clone();
        let ended = worker("completed");
        let t = build_at(Some(&f), &ended, T0, &clock, &Logs::new());
        let l = &t.lines[0];
        assert_eq!(
            l.now,
            "asleep · wrangler deploy (30m) · wakes when it ends · waiting: deploy upload slow"
        );
        assert!(
            l.why.iter().any(|w| w
                == &format!(
                    "its worker is asleep since {}: its turn ended while its own background \
                     shell runs: `wrangler deploy` (30m in, shell bohhzzia9). Claude Code wakes \
                     it when that ends; nothing to land yet",
                    clock_at(T0, T0, &tz)
                )),
            "{:?}",
            l.why
        );
        assert!(!l.why.iter().any(|w| w.contains("until its manager wakes")));
        // Its click follows the transcript and says asleep at the top.
        assert!(l.click.finished && l.click.asleep);
        assert!(
            l.click.facts.iter().any(|x| x.starts_with("asleep · ")),
            "{:?}",
            l.click.facts
        );
        // No land chore for it; one once it really ended.
        let logs = Logs::new();
        assert!(
            chores(
                Drawn::Manager,
                Some(&f),
                &ended,
                &logs,
                &shells,
                T0 + 30 * M
            )
            .is_empty()
        );
        assert_eq!(
            chores(
                Drawn::Manager,
                Some(&f),
                &ended,
                &logs,
                &NO_SHELLS,
                T0 + 30 * M
            )
            .iter()
            .map(|c| c.kind)
            .collect::<Vec<_>>(),
            [manage_chores::Kind::Land]
        );
        // Two shells: the first named, `+1`.
        let mut two = Shells::new();
        let mut sleep = deploy.clone();
        sleep.id = "bsleep".into();
        sleep.cmd = "sleep 100000".into();
        sleep.started_ms = T0 - 5 * M;
        two.insert("a1".into(), vec![deploy.clone(), sleep]);
        let c2 = Clock {
            shells: &two,
            ..Clock::plain()
        };
        let t = build_at(Some(&f), &ended, T0, &c2, &Logs::new());
        assert!(
            t.lines[0]
                .now
                .starts_with("asleep · wrangler deploy (30m) +1 · wakes when one ends"),
            "{}",
            t.lines[0].now
        );
        // No shell of its own left: turn ended, as before (giverny#471).
        let t = build_at(Some(&f), &ended, T0, &Clock::plain(), &Logs::new());
        assert_eq!(
            t.lines[0].now,
            format!(
                "turn ended {} · waiting: deploy upload slow",
                clock_at(T0, T0, &tz)
            )
        );
        assert!(!t.lines[0].click.asleep);
        // Another worker's shell does not make it asleep.
        let mut others = Shells::new();
        others.insert("zz9".into(), vec![deploy.clone()]);
        let c3 = Clock {
            shells: &others,
            ..Clock::plain()
        };
        let t = build_at(Some(&f), &ended, T0, &c3, &Logs::new());
        assert!(
            t.lines[0].now.starts_with("turn ended "),
            "{}",
            t.lines[0].now
        );
        // Woken: its activity, as any working worker.
        let woken = worker("running");
        let t = build_at(Some(&f), &woken, T0, &clock, &Logs::new());
        assert_eq!(t.lines[0].now, "Retry wrangler deploy");
        assert!(!t.lines[0].click.asleep);
    }

    #[test]
    fn a_made_up_agent_key_is_not_drawn() {
        // A row a worker with no task started under giverny#158's first round.
        let rows = live(
            r#"{"session_id":"s","tasks":[{"id":"adfb891dbd4470353","status":"running",
                "description":"Classify chunk 0","startTime":1789999700000}]}"#,
        );
        let f = feed(
            r#"{"rows":[{"key":"agent-adfb891d","stage":"running","follows_worker":true,
               "agent_id":"adfb891dbd4470353","title":"Classify chunk 0","eta_s":480}]}"#,
        );
        let t = build(Some(&f), &rows, T0);
        assert_eq!(t.lines[0].id, "");
        assert_eq!(t.lines[0].title, "Classify chunk 0");
        // A dispatcher's own key of that shape is drawn.
        let f = feed(
            r#"{"rows":[{"key":"agent-x1","stage":"running","agent_id":"adfb891dbd4470353"}]}"#,
        );
        assert_eq!(build(Some(&f), &rows, T0).lines[0].id, "agent-x1");
    }

    #[test]
    fn short_model_names() {
        assert_eq!(
            short_model("claude-haiku-4-5-20251001").as_deref(),
            Some("haiku")
        );
        assert_eq!(short_model("claude-opus-5[1m]").as_deref(), Some("opus"));
        assert_eq!(short_model("Sonnet").as_deref(), Some("sonnet"));
        assert_eq!(short_model("claude-fable-1").as_deref(), Some("fable"));
        assert_eq!(short_model("m"), None);
    }

    #[test]
    fn an_untasked_row_shows_its_model_and_a_tasked_one_its_key() {
        let rows = live(
            r#"{"session_id":"s","tasks":[{"id":"adfb891dbd4470353","status":"running",
                "description":"Classify chunk 0","startTime":1789999700000,
                "model":"claude-haiku-4-5-20251001"}]}"#,
        );
        // No feed: the model stands in for the missing name.
        assert_eq!(build(None, &rows, T0).lines[0].id, "haiku");
        // A made-up feed key: the same.
        let f = feed(
            r#"{"rows":[{"key":"agent-adfb891d","stage":"running","follows_worker":true,
               "agent_id":"adfb891dbd4470353"}]}"#,
        );
        assert_eq!(build(Some(&f), &rows, T0).lines[0].id, "haiku");
        // A tasked row keeps its key.
        let f =
            feed(r#"{"rows":[{"key":"g#3","stage":"running","agent_id":"adfb891dbd4470353"}]}"#);
        assert_eq!(build(Some(&f), &rows, T0).lines[0].id, "g#3");
    }

    #[test]
    fn the_feed_gives_ids_titles_etas_and_landings() {
        let rows = live(
            r#"{"session_id":"s","tasks":[{"id":"a1","status":"running",
                "startTime":1789999400000,"tokenCount":1000}]}"#,
        );
        let f = feed(
            r#"{"version":1,"session":"s","rows":[
              {"key":"acme#1","stage":"running","title":"FEATURE: one","agent_id":"a1",
               "started":1789999400000,"eta_s":1800},
              {"key":"acme#2","stage":"planned","title":"FEATURE: two","eta_s":3780},
              {"key":"acme#3","stage":"done","title":"BUG: three","started":1789990000000,
               "ended":1789993900000,"eta_s":3600,"landing":"Review — ita","tokens":5000}
            ]}"#,
        );
        let t = build(Some(&f), &rows, T0);
        let ids: Vec<&str> = t.lines.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, ["acme#1", "acme#2", "acme#3"]);
        // Running: 10m in, 30m estimate → 20m left.
        assert_eq!(t.lines[0].elapsed, "10:00");
        assert_eq!(t.lines[0].eta, "~20m");
        // Planned: its estimate, nothing else.
        assert_eq!(t.lines[1].eta, "~1h3m");
        assert_eq!(t.lines[1].elapsed, "");
        assert_eq!(t.lines[1].tokens, "");
        // Done: took 65m against 60m → five minutes late.
        assert_eq!(t.lines[2].elapsed, "1:05:00");
        assert_eq!(t.lines[2].eta, "(+5m)");
        assert_eq!(t.lines[2].now, "Review — ita");
    }

    #[test]
    fn a_worker_holding_two_rows_is_dittoed() {
        let rows = live(
            r#"{"session_id":"s","tasks":[{"id":"a1","status":"running",
                "startTime":1789999400000,"tokenCount":64100,"label":"Reading"}]}"#,
        );
        let f = feed(
            r#"{"rows":[
              {"key":"g#3","stage":"running","agent_id":"a1","eta_s":1800},
              {"key":"g#7","stage":"running","agent_id":"a1","eta_s":1800}
            ]}"#,
        );
        let t = build(Some(&f), &rows, T0);
        assert_eq!(t.lines[0].tokens, "64.1k");
        assert_eq!(t.lines[1].elapsed, "\"");
        assert_eq!(t.lines[1].eta, "\"");
        assert_eq!(t.lines[1].now, "\"");
        assert_eq!(t.lines[1].tokens, "\"");
    }

    #[test]
    fn a_ditto_is_only_for_a_cell_that_repeats() {
        let rows = live(
            r#"{"session_id":"s","tasks":[{"id":"a1","status":"running",
                "startTime":1789999400000,"tokenCount":10}]}"#,
        );
        let f = feed(
            r#"{"rows":[
              {"key":"g#3","stage":"running","agent_id":"a1","eta_s":1800},
              {"key":"g#7","stage":"running","agent_id":"a1","eta_s":3600},
              {"key":"g#8","stage":"running","agent_id":"a1","eta_s":3600}
            ]}"#,
        );
        let t = build(Some(&f), &rows, T0);
        assert_eq!(t.lines[1].eta, "~50m", "different estimate: written out");
        assert_eq!(t.lines[1].tokens, "\"");
        assert_eq!(t.lines[2].eta, "\"", "same as the row above's own text");
    }

    #[test]
    fn a_done_row_without_a_feed_says_how_it_ended() {
        let mut tr = Tracker::new(None);
        tr.apply_live(
            &LiveSnapshot::parse(
                r#"{"session_id":"s","tasks":[{"id":"a1","status":"running",
                    "startTime":1789999000000,"description":"x"}]}"#,
            ),
            T0 - 60_000,
        );
        tr.apply_live(
            &LiveSnapshot::parse(r#"{"session_id":"s","tasks":[{"id":"a1","status":"failed"}]}"#),
            T0,
        );
        let t = build(None, tr.rows(), T0 + 5_000);
        assert_eq!(t.lines[0].stage, Stage::Done);
        assert_eq!(t.lines[0].now, "failed");
        assert_eq!(t.lines[0].elapsed, "16:40");
        assert_eq!(t.lines[0].eta, "");
    }

    /// A feed written at `start` (no `agent_id`, no tokens) and
    /// never refreshed, and the worker Claude Code runs for it. One row,
    /// with the feed's id and ETA and the worker's tokens and activity,
    /// which move with its transcript at every refresh.
    #[test]
    fn a_stale_feed_row_and_its_worker_are_one_row_that_ticks() {
        let config = std::env::temp_dir().join(format!("giverny-pane-83-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&config);
        let subs = config.join("projects/-w/s/subagents");
        std::fs::create_dir_all(&subs).unwrap();
        std::fs::write(config.join("projects/-w/s.jsonl"), "{}\n").unwrap();
        let transcript = subs.join("agent-w82.jsonl");
        let turn = |ctx: u64, file: &str| {
            serde_json::json!({"type": "assistant", "timestamp": "2026-09-26T12:00:00Z",
                "message": {"model": "m", "usage": {"input_tokens": 1, "cache_read_input_tokens": ctx - 1},
                    "content": [{"type": "tool_use", "name": "Edit", "input": {"file_path": file}}]}})
            .to_string()
                + "\n"
        };
        let add = |text: String| {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&transcript)
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        };
        add(turn(50_000, "/a.rs"));

        let mut tr = Tracker::new(Some(config.clone()));
        tr.apply_live(
            &LiveSnapshot::parse(
                r#"{"session_id":"s","tasks":[{"id":"w82","status":"running",
                    "description":"Work demo#82 open direct","startTime":1789999400000,
                    "tokenCount":49000}]}"#,
            ),
            T0,
        );
        let f = feed(
            r#"{"session":"s","rows":[
              {"key":"demo#82","stage":"running","title":"FEATURE: open direct",
               "started":1789999400000,"eta_s":3600},
              {"key":"demo#84","stage":"planned","title":"FEATURE: selectable","eta_s":1800}
            ]}"#,
        );
        let mut seen = Vec::new();
        for (i, (ctx, file)) in [(50_000, "/a.rs"), (61_200, "/b.rs"), (74_900, "/c.rs")]
            .into_iter()
            .enumerate()
        {
            if i > 0 {
                add(turn(ctx, file));
            }
            tr.refresh();
            let t = build(Some(&f), tr.rows(), T0 + i as u64 * 1000);
            assert_eq!(
                t.lines.len(),
                2,
                "one row for the task and its worker: {t:?}"
            );
            let l = &t.lines[0];
            assert_eq!(l.id, "demo#82");
            assert_eq!(l.title, "FEATURE: open direct");
            assert_eq!(l.eta, "~50m");
            assert_eq!(l.click.agent_id.as_deref(), Some("w82"));
            assert_eq!(l.now, format!("Edit: {file}"));
            // The hover says it whole, right under the title.
            assert_eq!(l.why[0], format!("Edit: {file}"), "{}", l.tip());
            seen.push(l.tokens.clone());
        }
        assert_eq!(seen, ["50k", "61.2k", "74.9k"]);
        let _ = std::fs::remove_dir_all(&config);
    }

    // ------------------------------------------- one worker, tasks in turn ----

    fn ts(ms: u64) -> String {
        jiff::Timestamp::from_millisecond(ms as i64)
            .unwrap()
            .to_string()
    }

    /// A billed reply at `at`: `fresh` read new, `read` carried, `out`
    /// written. Written in Claude Code's own key order, `type` first.
    fn reply(id: &str, at: u64, fresh: u64, read: u64, out: u64) -> String {
        let usage = serde_json::json!({"input_tokens": 1,
            "cache_creation_input_tokens": fresh - 1, "cache_read_input_tokens": read,
            "output_tokens": out});
        format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"id":"{id}","model":"m","usage":{usage}}}}}"#,
            ts(at)
        )
    }

    /// The dispatcher's `SendMessage`, as the worker's transcript has it.
    fn sent(at: u64, body: &str) -> String {
        serde_json::json!({"type": "user", "timestamp": ts(at), "isMeta": true,
            "origin": {"kind": "coordinator"},
            "message": {"role": "user", "content":
                format!("The coordinator sent a message while you were working:\n{body}")}})
        .to_string()
    }

    /// A worker `w` spawned at `T0 - 60m` for acme#613, and its transcript.
    fn reused_worker(
        name: &str,
        lines: &[String],
        running: bool,
    ) -> (PathBuf, Vec<SubagentRow>, Logs) {
        spawned_worker(name, "acme#613 market SD graph", lines, running)
    }

    /// A worker `w` spawned at `T0 - 60m` as `description`, and its
    /// transcript.
    fn spawned_worker(
        name: &str,
        description: &str,
        lines: &[String],
        running: bool,
    ) -> (PathBuf, Vec<SubagentRow>, Logs) {
        let dir =
            std::env::temp_dir().join(format!("giverny-pane-141-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-w.jsonl");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let status = if running { "running" } else { "completed" };
        let mut rows = live(&format!(
            r#"{{"session_id":"s","tasks":[{{"id":"w","status":"{status}",
                "description":"{description}","startTime":{},
                "tokenCount":156313}}]}}"#,
            T0 - 60 * MIN
        ));
        rows[0].transcript = Some(path.clone());
        if !running {
            rows[0].ended_ms = Some(T0 - 5 * MIN);
        }
        let mut logs = Logs::new();
        poll_logs(&mut logs, &rows);
        (dir, rows, logs)
    }

    /// A worker's run as a feed writer recorded it: acme#613
    /// landed late with no span, acme#614 handed on by `start --agent`, and
    /// both rows carried the worker's whole count. The dispatcher's
    /// messages, a hand-off in words among them, move nothing (giverny#485).
    #[test]
    fn a_worker_handed_its_next_task_has_a_row_clock_and_count_per_task() {
        let lines = [
            reply("m1", T0 - 59 * MIN, 20_000, 0, 1_000),
            reply("m2", T0 - 40 * MIN, 5_000, 20_000, 2_000),
            sent(
                T0 - 50 * MIN,
                "Dispatcher note: the build is warm (acme#211).",
            ),
            sent(
                T0 - 30 * MIN,
                "New task for you, Wren Tilbury: acme#614, the follow-up to #613.",
            ),
            reply("m3", T0 - 20 * MIN, 8_000, 27_000, 3_000),
        ];
        let (dir, rows, logs) = reused_worker("feed", &lines, true);
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"acme#614","stage":"running","agent_id":"w","started":{s614},
                "eta_s":3600,"tokens":156313}},
              {{"key":"acme#616","stage":"planned","agent_id":"w","eta_s":900}},
              {{"key":"acme#613","stage":"done","agent_id":"w","started":{late},"ended":{late},
                "eta_s":1200,"tokens":156313,"landing":"Review — ita"}}
            ]}}"#,
            s614 = T0 - 22 * MIN,
            late = T0 - 22 * MIN - 7_000,
        ));
        let t = build_at(Some(&f), &rows, T0, &Clock::plain(), &logs);
        let ids: Vec<&str> = t.lines.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, ["acme#614", "acme#616", "acme#613"]);
        let (run, next, done) = (&t.lines[0], &t.lines[1], &t.lines[2]);
        // Running: timed from its `start`, never from a message, and its
        // count is the worker's whole live context, what it works with now
        // (giverny#319).
        assert_eq!(run.stage, Stage::Running);
        assert_eq!(run.elapsed, "22:00");
        assert_eq!(run.eta, "~38m");
        assert_eq!(run.tokens, "156.3k", "the worker's context, not 11k");
        // Queued behind it on the same worker.
        assert_eq!(next.stage, Stage::Planned);
        assert_eq!(next.now, "after acme#614");
        assert_eq!(next.tokens, "", "nothing spent on a task before it starts");
        // Done: from the spawn to the hand-off, and only what it added.
        assert_eq!(done.stage, Stage::Done);
        assert_eq!(done.elapsed, "37:53");
        assert_eq!(done.eta, "(+18m)");
        assert_eq!(done.now, "Review — ita");
        assert_eq!(
            done.tokens,
            fmt_tokens(21_000 + 7_000),
            "not the worker's 156.3k again"
        );
        for l in &t.lines {
            assert_eq!(
                l.click.agent_id.as_deref(),
                Some("w"),
                "every row opens the worker"
            );
        }

        // The next task is sent and nobody runs `start`: nothing moves.
        // The queued row waits, and acme#614 runs on.
        let more = [sent(
            T0 - 10 * MIN,
            "New task for you: acme#616, the legend.",
        )];
        let (dir2, rows, logs) =
            reused_worker("feed-next", &[&lines[..], &more[..]].concat(), true);
        let t = build_at(Some(&f), &rows, T0, &Clock::plain(), &logs);
        let got: Vec<(Stage, &str, &str, &str)> = t
            .lines
            .iter()
            .map(|l| {
                (
                    l.stage,
                    l.id.as_str(),
                    l.elapsed.as_str(),
                    l.tokens.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (Stage::Running, "acme#614", "22:00", "156.3k"),
                (Stage::Planned, "acme#616", "", ""),
                (Stage::Done, "acme#613", "37:53", "28k"),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// giverny#278, this repo's manager session: `266-anim` landed, then
    /// `start 267-use --agent w`, then `New task for you: 267-use
    /// (giverny#267)`. The message's first `#` key is not the feed's key,
    /// yet it is the task `start` gave the worker: one Running row with the
    /// feed's ETA and its own clock, never a second keyed `giverny#267`
    /// whose ELAPSED is dittoed one second and not the next.
    #[test]
    fn a_worker_reused_by_start_agent_holds_one_row_for_its_new_task() {
        let lines = [
            reply("m1", T0 - 59 * MIN, 20_000, 0, 1_000),
            sent(
                T0 - 10 * MIN,
                "New task for you: 267-use (giverny#267). Your #266 work is merged \
                 into agents-pane; thanks. Read and follow the brief.",
            ),
            reply("m2", T0 - 5 * MIN, 8_000, 21_000, 3_000),
        ];
        let (dir, rows, logs) = spawned_worker(
            "start-agent",
            "266-anim: rail animation switch",
            &lines,
            true,
        );
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"266-anim","stage":"done","agent_id":"w","started":{s266},
                "ended":{e266},"eta_s":1500,"landing":"Review — ita"}},
              {{"key":"267-use","stage":"running","agent_id":"w","started":{s267},
                "eta_s":3600}}
            ]}}"#,
            s266 = T0 - 60 * MIN,
            e266 = T0 - 11 * MIN,
            s267 = T0 - 10 * MIN - 20_000,
        ));
        for tick in 0..6 {
            let now = T0 + tick * 1_000;
            let t = build_at(Some(&f), &rows, now, &Clock::plain(), &logs);
            let ids: Vec<(Stage, &str)> =
                t.lines.iter().map(|l| (l.stage, l.id.as_str())).collect();
            assert_eq!(
                ids,
                [(Stage::Running, "267-use"), (Stage::Done, "266-anim")],
                "{t:?}"
            );
            let run = &t.lines[0];
            assert!(run.eta.starts_with('~'), "the feed's ETA: {:?}", run.eta);
            assert_ne!(run.elapsed, "\"", "its own clock");
            assert!(!t.lines[1].elapsed.contains('"'), "{:?}", t.lines[1]);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// giverny#485: no message hands a row to a worker, in any words. A
    /// plain `start` left acme#614 with no worker; the dispatcher's
    /// message giving it to the idle worker of acme#613, and one naming a
    /// task the feed lacks as new, move nothing and add no row. Forty
    /// minutes on, acme#614 still has no worker: Next up, `no worker yet`.
    #[test]
    fn a_message_hands_no_row_to_a_worker() {
        let lines = [
            reply("m1", T0 - 59 * MIN, 20_000, 0, 1_000),
            sent(
                T0 - 40 * MIN + 13_000,
                "#613 verified and landed in Review — thanks. Next you hold acme#614 and nothing else.",
            ),
            sent(T0 - 30 * MIN, "New task for you: acme#615, the legend."),
            reply("m2", T0 - 25 * MIN, 5_000, 20_000, 2_000),
        ];
        let (dir, rows, logs) = reused_worker("no-message-path", &lines, true);
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"acme#614","stage":"running","started":{s614}}},
              {{"key":"acme#613","stage":"done","agent_id":"w","started":{s613},"ended":{s614}}}
            ]}}"#,
            s613 = T0 - 60 * MIN,
            s614 = T0 - 40 * MIN,
        ));
        let t = build_with(
            Some(&f),
            &rows,
            T0,
            &Clock::plain(),
            &logs,
            DoneRows::All,
            Drawn::Manager,
            &HashSet::new(),
        );
        let got: Vec<(Stage, &str, &str, Option<&str>)> = t
            .lines
            .iter()
            .map(|l| {
                (
                    l.stage,
                    l.id.as_str(),
                    l.now.as_str(),
                    l.click.agent_id.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (Stage::Planned, "acme#614", NO_WORKER_YET, None),
                (Stage::Done, "acme#613", "", Some("w")),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// giverny#258, giverny#310: what the pane draws for a Running row that
    /// nothing runs. A row linked to a worker that has finished — here
    /// acme#615, which a hand-off message to `w` only mentioned — stays
    /// Running, as its writer says, drawn as any Running row: landing it is
    /// its manager's chore, told to the manager, not drawn for the person.
    /// A Running row with no worker at all is its manager's too once a spawn
    /// would have come (giverny#334): drawn plain under Next up, `no worker
    /// yet`. One just started, or whose worker works, says nothing.
    #[test]
    fn a_running_row_that_nothing_runs_says_so() {
        let lines = [
            reply("m1", T0 - 59 * MIN, 20_000, 0, 1_000),
            sent(
                T0 - 30 * MIN,
                "New task for you: acme#614, the bridge. Another worker now holds acme#615 \
                 and reads the same file.",
            ),
            reply("m2", T0 - 20 * MIN, 5_000, 20_000, 2_000),
        ];
        let (dir, rows, logs) = reused_worker("finished", &lines, false);
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"acme#615","stage":"running","agent_id":"w","started":{s},"eta_s":3600}},
              {{"key":"acme#617","stage":"running","started":{old},"eta_s":3600}},
              {{"key":"acme#618","stage":"running","started":{new},"eta_s":3600}},
              {{"key":"acme#614","stage":"done","agent_id":"w","started":{s},"ended":{e}}}
            ]}}"#,
            s = T0 - 30 * MIN,
            e = T0 - 6 * MIN,
            old = T0 - 10 * MIN,
            new = T0 - MIN,
        ));
        let t = build_at(Some(&f), &rows, T0, &Clock::plain(), &logs);
        let line = |id: &str| t.lines.iter().find(|l| l.id == id).unwrap();
        let got = line("acme#615");
        assert_eq!(got.stage, Stage::Running, "the feed's stage, never Done");
        // When its turn ended, a fact (giverny#471), and no chore text.
        assert!(got.now.starts_with("turn ended "), "{}", got.now);
        assert!(!got.now.contains("land"), "{}", got.now);
        assert!(!got.flag, "drawn in its own colour");
        assert!(
            !got.click.facts.iter().any(|f| f.contains("landed")),
            "{:?}",
            got.click.facts
        );
        assert!(got.click.finished, "a click opens it as Done (giverny#274)");
        // The manager hears of it instead, from the same rows: once its
        // grace is past (the worker ended five minutes ago). And of the row
        // with no worker, ten minutes after it started.
        let land = Chore {
            key: "acme#615".into(),
            kind: manage_chores::Kind::Land,
            since: T0 - 5 * MIN,
            how: Some("finished".into()),
        };
        let staff = Chore {
            key: "acme#617".into(),
            kind: manage_chores::Kind::Worker,
            since: T0 - 10 * MIN,
            how: None,
        };
        let all = chores(Drawn::Manager, Some(&f), &rows, &logs, &NO_SHELLS, T0);
        assert_eq!(all, [land, staff.clone()]);
        assert_eq!(
            due_chores(Drawn::Manager, &f, &rows, &logs, &NO_SHELLS, T0),
            all
        );
        assert_eq!(
            due_chores(Drawn::Manager, &f, &rows, &logs, &NO_SHELLS, T0 - 3 * MIN),
            [staff],
            "the land chore in its grace"
        );
        let none = line("acme#617");
        assert_eq!(
            (none.stage, none.now.as_str()),
            (Stage::Planned, NO_WORKER_YET)
        );
        assert!(!none.flag, "not the person's");
        assert_eq!(none.click.agent_id, None, "only mentioned: not w's");
        let fresh = line("acme#618");
        assert_eq!((fresh.now.as_str(), fresh.flag), ("", false));
        assert!(!line("acme#614").flag, "a Done row is not flagged");

        // A worker that failed is drawn plain too, its end in NOW, and its
        // manager is told how it ended; one that works is nobody's chore.
        let (dir2, mut rows, logs) = reused_worker("failed", &lines, false);
        rows[0].outcome = Some(Outcome::Failed);
        let t = build_at(Some(&f), &rows, T0, &Clock::plain(), &logs);
        let got = t.lines.iter().find(|l| l.id == "acme#615").unwrap();
        assert_eq!((got.now.as_str(), got.flag), ("stopped: failed", false));
        let told = chores(Drawn::Manager, Some(&f), &rows, &logs, &NO_SHELLS, T0);
        assert_eq!(told[0].how.as_deref(), Some("failed"));
        // Told once: a row stamped for that end is no chore any more.
        let mut stamped = f.clone();
        stamped.rows[0].land_nudged_ms = Some(T0 - 5 * MIN);
        assert!(
            due_chores(Drawn::Manager, &stamped, &rows, &logs, &NO_SHELLS, T0)
                .iter()
                .all(|c| c.key != "acme#615")
        );
        let (dir3, rows, logs) = reused_worker("working", &lines, true);
        let t = build_at(Some(&f), &rows, T0, &Clock::plain(), &logs);
        let got = t.lines.iter().find(|l| l.id == "acme#615").unwrap();
        assert!(!got.flag, "{got:?}");
        assert!(!got.click.finished, "a working worker is walked into");
        assert!(
            chores(Drawn::Manager, Some(&f), &rows, &logs, &NO_SHELLS, T0)
                .iter()
                .all(|c| c.key != "acme#615")
        );
        for d in [dir, dir2, dir3] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// giverny#332: a task handed to a worker that finished its last one
    /// five minutes ago (`start acme#614 --agent w`) is not finished the
    /// moment it is handed on: no land chore, drawn plain, while the worker
    /// is still to pick it up. Never picked up, it has no worker running
    /// once the hand-off window is past. Picked up and finished again, its
    /// manager hears of it three minutes after that second end.
    #[test]
    fn a_task_handed_to_an_idle_worker_is_not_finished() {
        let lines = [
            reply("m1", T0 - 59 * MIN, 20_000, 0, 1_000),
            reply("m2", T0 - 5 * MIN, 5_000, 20_000, 2_000),
        ];
        // The worker ended acme#613 at T0 - 5m; acme#614 is handed it at T0.
        let (dir, rows, logs) = reused_worker("332-idle", &lines, false);
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"acme#614","stage":"running","agent_id":"w","started":{s614},"eta_s":900}},
              {{"key":"acme#613","stage":"done","agent_id":"w","started":{s613},"ended":{e613}}}
            ]}}"#,
            s613 = T0 - 60 * MIN,
            e613 = T0 - 5 * MIN,
            s614 = T0,
        ));
        let row = |t: &Table| {
            t.lines
                .iter()
                .find(|l| l.id == "acme#614")
                .cloned()
                .unwrap()
        };
        for at in [T0, T0 + MIN, T0 + 2 * MIN] {
            assert_eq!(
                chores(Drawn::Manager, Some(&f), &rows, &logs, &NO_SHELLS, at),
                [],
                "at T0+{}m",
                (at - T0) / MIN
            );
            assert_eq!(
                due_chores(Drawn::Manager, &f, &rows, &logs, &NO_SHELLS, at),
                []
            );
            let got = row(&build_at(Some(&f), &rows, at, &Clock::plain(), &logs));
            assert_eq!(got.stage, Stage::Running);
            assert!(!got.flag, "plain while it is picked up: {got:?}");
            assert_ne!(got.now, NO_WORKER_YET);
        }
        // Never picked up: nothing works on it. It waits under Next up, and
        // its manager is told to staff it (giverny#334).
        let late = T0 + 3 * MIN;
        assert_eq!(
            chores(Drawn::Manager, Some(&f), &rows, &logs, &NO_SHELLS, late),
            [Chore {
                key: "acme#614".into(),
                kind: manage_chores::Kind::Worker,
                since: T0,
                how: None,
            }]
        );
        let got = row(&build_at(Some(&f), &rows, late, &Clock::plain(), &logs));
        assert_eq!(
            (got.stage, got.now.as_str(), got.flag),
            (Stage::Planned, NO_WORKER_YET, false)
        );
        let _ = std::fs::remove_dir_all(&dir);

        // Picked up at T0 + 1m (the message) and finished at T0 + 10m.
        let mut lines = lines.to_vec();
        lines.push(sent(T0 + MIN, "New task for you: acme#614."));
        lines.push(reply("m3", T0 + 10 * MIN, 3_000, 25_000, 900));
        let (dir, mut rows, logs) = reused_worker("332-again", &lines, false);
        rows[0].ended_ms = Some(T0 + 10 * MIN);
        let all = chores(
            Drawn::Manager,
            Some(&f),
            &rows,
            &logs,
            &NO_SHELLS,
            T0 + 10 * MIN,
        );
        assert_eq!(
            all,
            [Chore {
                key: "acme#614".into(),
                kind: manage_chores::Kind::Land,
                since: T0 + 10 * MIN,
                how: Some("finished".into()),
            }]
        );
        assert_eq!(
            due_chores(Drawn::Manager, &f, &rows, &logs, &NO_SHELLS, T0 + 12 * MIN),
            [],
            "in its grace"
        );
        assert_eq!(
            due_chores(Drawn::Manager, &f, &rows, &logs, &NO_SHELLS, T0 + 13 * MIN),
            all
        );
        let got = row(&build_at(
            Some(&f),
            &rows,
            T0 + 13 * MIN,
            &Clock::plain(),
            &logs,
        ));
        assert!(
            !got.flag,
            "a finished worker's row is its manager's, drawn plain"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // A message before `start` was run hands nothing (giverny#485):
        // `start --agent` at T0 - 2m is the hand-off, so the worker's end at
        // T0 - 5m ended its task before, and it is still to pick this up.
        let mut lines = vec![
            reply("m1", T0 - 59 * MIN, 20_000, 0, 1_000),
            reply("m2", T0 - 30 * MIN, 5_000, 20_000, 2_000),
            sent(T0 - 20 * MIN, "New task for you: acme#614."),
        ];
        lines.push(reply("m3", T0 - 5 * MIN, 3_000, 25_000, 900));
        let (dir, rows, logs) = reused_worker("332-msg", &lines, false);
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"acme#614","stage":"running","agent_id":"w","started":{s614}}},
              {{"key":"acme#613","stage":"done","agent_id":"w","started":{s613},"ended":{e613}}}
            ]}}"#,
            s613 = T0 - 60 * MIN,
            e613 = T0 - 30 * MIN,
            s614 = T0 - 2 * MIN,
        ));
        let got = chores(Drawn::Manager, Some(&f), &rows, &logs, &NO_SHELLS, T0);
        assert_eq!(got, [], "not finished by the message's turns");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// giverny#334: a task nobody works on is Next up, not Running. A
    /// Running row with no worker that its manager paused or held, or never
    /// gave a worker, is drawn under Next up — in its place in line to
    /// start (giverny#442): one set aside below the rest — with why, never
    /// in amber; only the one not set aside is a
    /// chore. A worker that handed its row back and a row paused rather
    /// than landed waits there too. A worker that failed, or stopped on an
    /// API error, keeps its row Running for the person to see.
    #[test]
    fn a_running_row_nobody_works_on_waits_under_next_up() {
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"acme#700","stage":"running","started":{s},"eta_s":3600,
                "paused_since":{p}}},
              {{"key":"acme#701","stage":"running","started":{s},"eta_s":3600,
                "held":"Ita to pick the design"}},
              {{"key":"acme#702","stage":"running","started":{s},"eta_s":3600}},
              {{"key":"acme#703","stage":"planned","planned":{s},"eta_s":600}}
            ]}}"#,
            s = T0 - 30 * MIN,
            p = T0 - 20 * MIN,
        ));
        let t = build(Some(&f), &[], T0);
        let order: Vec<_> = t.lines.iter().map(|l| (l.id.as_str(), l.stage)).collect();
        assert_eq!(
            order,
            [
                ("acme#702", Stage::Planned),
                ("acme#703", Stage::Planned),
                ("acme#700", Stage::Planned),
                ("acme#701", Stage::Planned),
            ]
        );
        assert!(!t.any_running());
        let line = |id: &str| t.lines.iter().find(|l| l.id == id).unwrap();
        assert!(t.lines.iter().all(|l| !l.flag), "nothing amber");
        assert!(
            line("acme#700").now.starts_with("paused since "),
            "{:?}",
            line("acme#700").now
        );
        assert_eq!(line("acme#701").now, "held: Ita to pick the design");
        assert!(
            line("acme#701")
                .click
                .facts
                .contains(&"held: Ita to pick the design".to_string())
        );
        assert_eq!(line("acme#702").now, NO_WORKER_YET);
        assert_eq!(line("acme#702").eta, "~1h", "a Next up row's estimate");
        // Only the row nobody set aside is its manager's to staff, and the
        // idle Next up row is no chore for being below it.
        assert_eq!(
            chores(Drawn::Manager, Some(&f), &[], &Logs::new(), &NO_SHELLS, T0),
            [
                Chore {
                    key: "acme#702".into(),
                    kind: manage_chores::Kind::Worker,
                    since: T0 - 30 * MIN,
                    how: None,
                },
                Chore {
                    key: "acme#703".into(),
                    kind: manage_chores::Kind::Start,
                    since: T0 - 30 * MIN,
                    how: None,
                },
            ]
        );
        // Told once: stamped for that instant, it is no chore any more.
        let mut stamped = f.clone();
        stamped.rows[2].worker_nudged_ms = Some(T0 - 30 * MIN);
        assert!(
            due_chores(Drawn::Manager, &stamped, &[], &Logs::new(), &NO_SHELLS, T0)
                .iter()
                .all(|c| c.key != "acme#702")
        );
        // Just started: a spawn is still to come, Running and no chore;
        // paused or held, none is: Next up at once.
        let t = build(Some(&f), &[], T0 - 29 * MIN);
        let at = |id: &str| t.lines.iter().find(|l| l.id == id).unwrap().stage;
        assert_eq!(at("acme#702"), Stage::Running);
        assert_eq!(at("acme#700"), Stage::Planned);
        assert_eq!(at("acme#701"), Stage::Planned);
        assert!(
            chores(
                Drawn::Manager,
                Some(&f),
                &[],
                &Logs::new(),
                &NO_SHELLS,
                T0 - 29 * MIN
            )
            .iter()
            .all(|c| c.kind != manage_chores::Kind::Worker)
        );

        // A worker that handed its row back: the row paused rather than
        // landed waits under Next up, saying its worker finished, and its
        // manager is told to land or resume it (giverny#379, below); failed,
        // or stopped on an API error, it stays Running and says so.
        let lines = [
            reply("m1", T0 - 59 * MIN, 20_000, 0, 1_000),
            reply("m2", T0 - 20 * MIN, 5_000, 20_000, 2_000),
        ];
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"acme#613","stage":"running","agent_id":"w","started":{s},"eta_s":3600,
                "paused_since":{p}}}
            ]}}"#,
            s = T0 - 60 * MIN,
            p = T0 - 10 * MIN,
        ));
        let (dir, rows, logs) = reused_worker("334-paused", &lines, false);
        let t = build_at(Some(&f), &rows, T0, &Clock::plain(), &logs);
        assert_eq!(t.lines[0].stage, Stage::Planned, "{:?}", t.lines[0]);
        assert_eq!(t.lines[0].now, PAUSED_FINISHED);
        assert_eq!(
            chores(Drawn::Manager, Some(&f), &rows, &logs, &NO_SHELLS, T0),
            [Chore {
                key: "acme#613".into(),
                kind: manage_chores::Kind::LandPaused,
                since: T0 - 5 * MIN,
                how: Some("finished".into()),
            }]
        );
        let (dir2, mut rows, logs) = reused_worker("334-failed", &lines, false);
        rows[0].outcome = Some(Outcome::Failed);
        let mut f = f.clone();
        f.rows[0].paused_since_ms = None;
        let t = build_at(Some(&f), &rows, T0, &Clock::plain(), &logs);
        assert_eq!(
            (t.lines[0].stage, t.lines[0].now.as_str()),
            (Stage::Running, "stopped: failed")
        );
        for d in [dir, dir2] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// giverny#379: a paused row whose worker finished — during the pause,
    /// or before it — reads `paused · worker finished` under Next up, and
    /// its manager is told once, past the land grace, to land or resume it.
    /// One told of that finish while Running is not told again; a paused
    /// row whose worker still works is no chore and reads `paused since …`.
    #[test]
    fn a_paused_row_whose_worker_finished_is_its_managers_to_land() {
        let lines = [
            reply("m1", T0 - 59 * MIN, 20_000, 0, 1_000),
            reply("m2", T0 - 20 * MIN, 5_000, 20_000, 2_000),
        ];
        // The worker ended at T0 - 5m ([`spawned_worker`]): paused at
        // T0 - 10m it finished during the pause, at T0 - 2m before it.
        let paused_at = |p: u64| {
            feed(&format!(
                r#"{{"session":"s","rows":[
                  {{"key":"acme#613","stage":"running","agent_id":"w","started":{s},
                    "eta_s":3600,"paused_since":{p}}}
                ]}}"#,
                s = T0 - 60 * MIN,
            ))
        };
        let (dir, rows, logs) = reused_worker("379-finished", &lines, false);
        let land = Chore {
            key: "acme#613".into(),
            kind: manage_chores::Kind::LandPaused,
            since: T0 - 5 * MIN,
            how: Some("finished".into()),
        };
        for p in [T0 - 10 * MIN, T0 - 2 * MIN] {
            let f = paused_at(p);
            let t = build_at(Some(&f), &rows, T0, &Clock::plain(), &logs);
            assert_eq!(
                (t.lines[0].stage, t.lines[0].now.as_str(), t.lines[0].flag),
                (Stage::Planned, PAUSED_FINISHED, false)
            );
            assert_eq!(
                chores(Drawn::Manager, Some(&f), &rows, &logs, &NO_SHELLS, T0),
                std::slice::from_ref(&land)
            );
            assert_eq!(
                due_chores(Drawn::Manager, &f, &rows, &logs, &NO_SHELLS, T0 - 3 * MIN),
                [],
                "in its grace"
            );
            assert_eq!(
                due_chores(Drawn::Manager, &f, &rows, &logs, &NO_SHELLS, T0),
                std::slice::from_ref(&land)
            );
            // Told once: told of this finish — paused, or Running before
            // the pause — it is no chore any more.
            let mut stamped = f.clone();
            stamped.rows[0].land_nudged_ms = Some(T0 - 5 * MIN);
            assert_eq!(
                due_chores(Drawn::Manager, &stamped, &rows, &logs, &NO_SHELLS, T0),
                []
            );
        }
        // Unpaused, the same finish is the Running row's land chore, as
        // before.
        let mut running = paused_at(T0 - 10 * MIN);
        running.rows[0].paused_since_ms = None;
        let got = chores(Drawn::Manager, Some(&running), &rows, &logs, &NO_SHELLS, T0);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].kind, manage_chores::Kind::Land);
        assert_eq!(got[0].since, T0 - 5 * MIN);
        let t = build_at(Some(&running), &rows, T0, &Clock::plain(), &logs);
        assert_eq!(t.lines[0].stage, Stage::Running);
        assert!(
            t.lines[0].now.starts_with("turn ended "),
            "{}",
            t.lines[0].now
        );
        // A worker still at work under the paused row: no chore, and the
        // row says only since when it is paused.
        let (dir2, rows, logs) = reused_worker("379-working", &lines, true);
        let f = paused_at(T0 - 10 * MIN);
        assert_eq!(
            chores(Drawn::Manager, Some(&f), &rows, &logs, &NO_SHELLS, T0),
            []
        );
        let t = build_at(Some(&f), &rows, T0, &Clock::plain(), &logs);
        assert!(
            t.lines[0].now.starts_with("paused since "),
            "{:?}",
            t.lines[0]
        );
        for d in [dir, dir2] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// Claude Code lists a worker woken by a message afresh, its
    /// `startTime` moved to that message. A worker on its third task, so
    /// listed, still has a row per task: each Done row counts its own span
    /// and stays put while the worker goes on, the Running row shows the
    /// worker's live context (giverny#319), and a count the feed froze at
    /// landing is the one shown.
    #[test]
    fn a_relisted_workers_done_rows_keep_their_own_counts() {
        let mut lines = vec![
            reply("m1", T0 - 60 * MIN, 10_000, 0, 1_000),
            sent(T0 - 40 * MIN, "New task for you: acme#614."),
            reply("m2", T0 - 30 * MIN, 4_000, 10_000, 600),
            sent(T0 - 20 * MIN, "New task for you: acme#616, after #614."),
            reply("m3", T0 - 15 * MIN, 3_000, 14_000, 700),
        ];
        let dir = std::env::temp_dir().join(format!("giverny-pane-relist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-w.jsonl");
        // acme#613 was started before the worker had an id: joined by the
        // spawn description, its id put on it when it was handed on.
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"acme#616","stage":"running","agent_id":"w","started":{s616}}},
              {{"key":"acme#614","stage":"done","agent_id":"w","started":{s614},"ended":{s616}}},
              {{"key":"acme#613","stage":"done","agent_id":"w","started":{s613},"ended":{s614}}}
            ]}}"#,
            s613 = T0 - 60 * MIN,
            s614 = T0 - 40 * MIN,
            s616 = T0 - 20 * MIN,
        ));
        let table = |lines: &[String], f: &Feed, context: u64| {
            std::fs::write(&path, lines.join("\n") + "\n").unwrap();
            let mut rows = live(&format!(
                r#"{{"session_id":"s","tasks":[{{"id":"w","status":"running",
                    "description":"acme#613 market SD graph","startTime":{},
                    "tokenCount":{context}}}]}}"#,
                T0 - 20 * MIN
            ));
            rows[0].transcript = Some(path.clone());
            let mut logs = Logs::new();
            poll_logs(&mut logs, &rows);
            build_at(Some(f), &rows, T0, &Clock::plain(), &logs)
                .lines
                .iter()
                .map(|l| (l.id.clone(), l.elapsed.clone(), l.tokens.clone()))
                .collect::<Vec<_>>()
        };
        let row = |id: &str, el: &str, tok: u64| (id.to_string(), el.to_string(), fmt_tokens(tok));
        let want = [
            row("acme#616", "20:00", 17_000),
            row("acme#614", "20:00", 4_600),
            row("acme#613", "20:00", 11_000),
        ];
        assert_eq!(table(&lines, &f, 17_000), want);
        // The worker goes on: only its Running row moves.
        lines.push(reply("m4", T0 - 5 * MIN, 50_000, 17_000, 900));
        let mut later = want.clone();
        later[0].2 = fmt_tokens(67_000);
        assert_eq!(table(&lines, &f, 67_000), later);
        // A count frozen when the row landed is the one drawn.
        let mut f = f;
        f.rows[1].task_tokens = Some(4_321);
        assert_eq!(table(&lines, &f, 67_000)[1].2, fmt_tokens(4_321));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// giverny#319: a reused worker's Running row shows the whole context
    /// it works with now — the figure `giverny manage start` says an idle
    /// worker carries — not the 4k spent since the hand-off. Its first
    /// task's Done row still shows only what that task took, and a fresh
    /// worker's Running row is its live count, as ever.
    #[test]
    fn a_reused_workers_running_row_shows_its_whole_context() {
        let lines = [
            reply("m1", T0 - 59 * MIN, 60_000, 0, 2_000),
            reply("m2", T0 - 35 * MIN, 28_000, 62_000, 2_000),
            sent(T0 - 30 * MIN, "New task for you: acme#614, the follow-up."),
            reply("m3", T0 - 10 * MIN, 3_100, 90_000, 900),
        ];
        let dir = std::env::temp_dir().join(format!("giverny-pane-319-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-w.jsonl");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        // What `start` prints for this worker were it idle.
        let context = giverny_claude::tokens::tokens_of(&path).unwrap();
        assert_eq!(context, 93_100);
        let mut rows = live(&format!(
            r#"{{"session_id":"s","tasks":[
              {{"id":"w","status":"running","description":"acme#613 market SD graph",
                "startTime":{w},"tokenCount":{context}}},
              {{"id":"f","status":"running","description":"acme#700 fresh one",
                "startTime":{f},"tokenCount":41200}}]}}"#,
            w = T0 - 60 * MIN,
            f = T0 - 15 * MIN,
        ));
        rows[0].transcript = Some(path.clone());
        let mut logs = Logs::new();
        poll_logs(&mut logs, &rows);
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"acme#614","stage":"running","agent_id":"w","started":{s614}}},
              {{"key":"acme#700","stage":"running","agent_id":"f","started":{s700}}},
              {{"key":"acme#613","stage":"done","agent_id":"w","started":{s613},"ended":{s614}}}
            ]}}"#,
            s613 = T0 - 60 * MIN,
            s614 = T0 - 30 * MIN,
            s700 = T0 - 15 * MIN,
        ));
        let t = build_at(Some(&f), &rows, T0, &Clock::plain(), &logs);
        let got: Vec<(Stage, &str, &str)> = t
            .lines
            .iter()
            .map(|l| (l.stage, l.id.as_str(), l.tokens.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (Stage::Running, "acme#614", "93.1k"),
                (Stage::Running, "acme#700", "41.2k"),
                (Stage::Done, "acme#613", "92k"),
            ]
        );
        assert_eq!(t.lines[0].tokens, fmt_tokens(context), "agrees with start");
        assert_eq!(logs["w"].added(T0 - 30 * MIN, None), 4_000, "not this");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The top line's `subagents` and `total` are per agent, each transcript
    /// read once: splitting a reused worker into a row per task changes
    /// neither, and they are never the rows added up.
    #[test]
    fn splitting_a_reused_workers_rows_leaves_the_total_alone() {
        use giverny_claude::tokens;
        let lines = [
            reply("m1", T0 - 55 * MIN, 10_000, 0, 1_000),
            sent(T0 - 40 * MIN, "New task for you: acme#614."),
            reply("m2", T0 - 30 * MIN, 4_000, 10_000, 600),
        ];
        let (dir, rows, logs) = reused_worker("total", &lines, true);
        let other = dir.join("agent-o.jsonl");
        std::fs::write(&other, reply("o1", T0 - 5 * MIN, 30_000, 0, 10) + "\n").unwrap();
        let subs = [dir.join("agent-w.jsonl"), other];
        let before = tokens::session_subagents_total(Some(50_000), 0, &subs);
        // `start acme#614 --agent w` at the hand-off.
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"acme#614","stage":"running","agent_id":"w","started":{s614}}},
              {{"key":"acme#613","stage":"done","agent_id":"w","started":{s613},"ended":{s614}}}
            ]}}"#,
            s613 = T0 - 60 * MIN,
            s614 = T0 - 40 * MIN,
        ));
        let one = build_at(None, &rows, T0, &Clock::plain(), &logs);
        let split = build_at(Some(&f), &rows, T0, &Clock::plain(), &logs);
        assert_eq!(one.lines.len(), 1);
        assert_eq!(split.lines.len(), 2, "a row per task");
        let after = tokens::session_subagents_total(Some(50_000), 0, &subs);
        assert_eq!(before, after);
        // Per agent, each once: the worker's last context, the other's.
        let w = 4_000 + 10_000;
        assert_eq!(
            after,
            (Some(50_000), Some(w + 30_000), Some(50_000 + w + 30_000))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_to_show_is_no_pane() {
        assert!(build(None, &[], T0).is_empty());
    }

    /// [`build_with`] as the pane calls it, for a session that is a
    /// manager or not.
    fn drawn_table(f: &Feed, live: &[SubagentRow], manager: bool) -> Table {
        let (clock, logs) = (Clock::plain(), Logs::new());
        let d = Drawn::of(manager);
        build_with(
            Some(f),
            live,
            T0,
            &clock,
            &logs,
            DoneRows::All,
            d,
            &HashSet::new(),
        )
    }

    /// giverny#357, the case that prompted it: a session that is no
    /// manager runs `giverny manage start` for its own build. Its row has
    /// no worker behind it: nothing is drawn, and its session hears no
    /// "no worker" chore.
    #[test]
    fn a_stray_start_in_a_plain_session_draws_and_nags_nothing() {
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"build","stage":"running","started":{s},"eta_s":600}},
              {{"key":"later","stage":"planned","planned":{s},"eta_s":600}}]}}"#,
            s = T0 - 30 * MIN,
        ));
        assert!(drawn_table(&f, &[], false).is_empty());
        assert_eq!(
            chores(Drawn::Agents, Some(&f), &[], &Logs::new(), &NO_SHELLS, T0),
            []
        );
        // A manager's own: both under Next up, and its chores.
        let t = drawn_table(&f, &[], true);
        let ids: Vec<_> = t.lines.iter().map(|l| (l.id.as_str(), l.stage)).collect();
        assert_eq!(ids, [("build", Stage::Planned), ("later", Stage::Planned)]);
        let kinds: Vec<_> = chores(Drawn::Manager, Some(&f), &[], &Logs::new(), &NO_SHELLS, T0)
            .into_iter()
            .map(|c| (c.key, c.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("build".to_string(), manage_chores::Kind::Worker),
                ("later".to_string(), manage_chores::Kind::Start),
            ]
        );
    }

    /// giverny#357: a real subagent is a full row in any session — its
    /// feed row's title and ETA, the worker's clock, activity and tokens —
    /// and a feed row with nobody behind it beside it is not drawn.
    #[test]
    fn a_real_worker_is_a_full_row_in_a_plain_session() {
        let rows = live(
            r#"{"session_id":"s","tasks":[{"id":"a1","status":"running",
                "description":"Work demo#12","startTime":1789999958000,
                "tokenCount":64100,"label":"Editing src/lib.rs"}]}"#,
        );
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"demo#12","stage":"running","title":"FEATURE: pane","started":{s},
                "eta_s":1800}},
              {{"key":"stray","stage":"running","started":{s},"eta_s":600}}]}}"#,
            s = T0 - 60_000,
        ));
        let t = drawn_table(&f, &rows, false);
        assert_eq!(t.lines.len(), 1, "{:?}", t.lines);
        let l = &t.lines[0];
        assert_eq!((l.id.as_str(), l.stage), ("demo#12", Stage::Running));
        assert_eq!(l.title, "FEATURE: pane");
        assert_eq!(l.elapsed, "0:42");
        assert_eq!(l.tokens, "64.1k");
        assert_eq!(l.now, "Editing src/lib.rs");
        assert!(!l.eta.is_empty() && l.eta != NO_ETA, "{:?}", l.eta);
    }

    /// giverny#357: a manager's build row — `giverny manage run` on a row
    /// of its own, no worker behind it — shows, Running or Done.
    #[test]
    fn a_managers_build_row_shows_with_no_worker() {
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"integ-build","stage":"running","started":{s},"eta_s":600,
                "usage":{{"runs":1,"peak_mb":1500,"cpu_s":60,"wall_s":60}}}},
              {{"key":"old-build","stage":"done","started":{s},"ended":{s},
                "usage":{{"runs":1}}}},
              {{"key":"stray-done","stage":"done","started":{s},"ended":{s}}}]}}"#,
            s = T0 - MIN,
        ));
        let t = drawn_table(&f, &[], true);
        let ids: Vec<_> = t.lines.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, ["integ-build", "old-build"]);
        assert!(drawn_table(&f, &[], false).is_empty());
    }

    // ------------------------------------------------- resources ----

    /// A feed with a granted Running row and a queued Next up row, as
    /// `giverny manage claim` leaves them.
    const LEASED: &str = r#"{"session":"s","rows":[
        {"key":"demo#12","stage":"running","eta_s":1800,"started":1790000000000,
         "lease":{"state":"granted","id":"s:demo#12","cpu":3,"ram_mb":3072,
                  "slots":["cargo:/x/target"],"granted_at":1790000000000}},
        {"key":"demo#13","stage":"planned","eta_s":600,
         "lease":{"state":"queued","position":1,"behind":"demo#12","short":["ram"],
                  "cpu":2,"ram_mb":3072}}]}"#;

    fn ledger(json: &str) -> Ledger {
        Ledger::parse(json.as_bytes()).expect("test ledger parses")
    }

    fn task_live(task: &str, cpu_pct: u32, mem_mb: u64) -> TaskLive {
        task_live_gpu(task, cpu_pct, mem_mb, None)
    }

    fn task_live_gpu(task: &str, cpu_pct: u32, mem_mb: u64, gpu_mb: Option<u64>) -> TaskLive {
        TaskLive {
            session: "s".into(),
            task: task.into(),
            live: RunLive {
                cpu_pct,
                mem_mb,
                gpu_mb,
            },
            agent: None,
        }
    }

    #[test]
    fn the_use_cell_reads_compactly() {
        let now = RunLive {
            cpu_pct: 14,
            mem_mb: 4300,
            gpu_mb: None,
        };
        assert_eq!(
            usage_cell(Some(now), None, false, false),
            "     14% CPU  ·  4.2G"
        );
        assert_eq!(
            usage_cell(None, Some(2150), false, false),
            "                 2.1G"
        );
        assert_eq!(
            usage_cell(None, Some(128), true, false),
            "OOM              0.1G"
        );
        assert_eq!(
            usage_cell(None, Some(0), false, false),
            "                 0.0G"
        );
        assert_eq!(
            usage_cell(None, None, false, false),
            "                 0.0G"
        );
        assert_eq!(
            usage_cell(Some(RunLive::default()), None, true, false),
            "OOM   0% CPU  ·  0.0G"
        );
        // A machine with a GPU: a place for it, filled on a Running row.
        let on_gpu = RunLive {
            gpu_mb: Some(1229),
            ..now
        };
        assert_eq!(
            usage_cell(Some(on_gpu), None, false, true),
            "     14% CPU  ·  4.2G  ·  gpu 1.2G"
        );
        assert_eq!(
            usage_cell(Some(now), None, false, true),
            "     14% CPU  ·  4.2G             "
        );
        assert_eq!(
            usage_cell(None, Some(2150), false, true),
            "                 2.1G             "
        );
        // GPU figures only where the machine has one.
        assert_eq!(
            usage_cell(Some(on_gpu), None, false, false),
            "     14% CPU  ·  4.2G"
        );
        let mut l = feed(LEASED).rows[0].lease.clone().unwrap();
        l.state = LeaseState::Queued;
        l.position = Some(2);
        l.short = vec!["ram".into()];
        assert_eq!(lease_fact(&l), "queued #2 for 3G");
        // Granted smaller: what it asked for, beside what it holds.
        let mut s = l.clone();
        s.state = LeaseState::Smaller;
        s.wanted_ram_mb = None;
        assert!(!lease_fact(&s).contains("asked"), "{}", lease_fact(&s));
        s.wanted_ram_mb = Some(4096);
        assert!(
            lease_fact(&s).ends_with(" (asked 4G)"),
            "{}",
            lease_fact(&s)
        );
        // giverny#429: granted on another session's call, untaken; and
        // queued again once that grant lapsed.
        let mut g = l.clone();
        g.state = LeaseState::Granted;
        g.uncollected = true;
        assert_eq!(
            lease_fact(&g),
            "holds 3G, slot cargo:/x/target; granted, waiting for its manager"
        );
        let mut q = l.clone();
        q.lapsed = true;
        assert_eq!(
            queued_note(&q),
            "queued, grant lapsed, waiting for its manager"
        );
    }

    /// Every cell is as wide as every other, whatever
    /// the figures and the state, so the column and what follows it hold
    /// still.
    #[test]
    fn every_use_cell_is_the_same_width() {
        let mems = [
            0,
            1,
            9,
            99,
            512,
            980,
            999,
            1000,
            1023,
            1024,
            1100,
            4300,
            10_188,
            10_189,
            12_288,
            131_072,
            1_022_976,
            1_023_487,
            1_023_488,
            1_048_576,
            1_572_864,
            10_485_760,
            u64::MAX / 2,
        ];
        let cpus = [0, 5, 45, 99, 100, 145, 1200];
        let mut seen = 0;
        for &m in &mems {
            assert_eq!(mem4(m).chars().count(), 4, "{m}: {:?}", mem4(m));
            for oom in [false, true] {
                for peak in [None, Some(0), Some(m)] {
                    for gpu in [false, true] {
                        let cell = usage_cell(None, peak, oom, gpu);
                        assert_eq!(cell.chars().count(), usage_w(gpu), "{cell:?}");
                    }
                    seen += 1;
                }
                for &cpu_pct in &cpus {
                    for (gpu_mb, gpu) in [(None, false), (None, true), (Some(m), true)] {
                        let live = RunLive {
                            cpu_pct,
                            mem_mb: m,
                            gpu_mb,
                        };
                        let cell = usage_cell(Some(live), None, oom, gpu);
                        assert_eq!(cell.chars().count(), usage_w(gpu), "{cell:?}");
                    }
                    seen += 1;
                }
            }
        }
        assert!(seen > 300);
        assert_eq!(usage_w(false), 21);
        assert_eq!(usage_w(true), 34);
        assert_eq!(mem4(0), "0.0G");
        assert_eq!(mem4(1), "0.1G");
        assert_eq!(mem4(100), "0.1G");
        assert_eq!(mem4(980), "1.0G");
        assert_eq!(mem4(1000), "1.0G");
        assert_eq!(mem4(1_022_976), "999G");
        assert_eq!(mem4(10_188), "9.9G");
        assert_eq!(mem4(10_189), " 10G");
        assert_eq!(mem4(1_572_864), "1.5T");
    }

    /// giverny#452: the OOM mark is the latest run's, not the row's past. A
    /// row whose OOM was followed by a clean run (here an old-format row,
    /// poly's 138: `oom_kills` 1, `last_exit` 0) draws none and says the
    /// kill was earlier; one whose last run was killed draws it.
    #[test]
    fn an_oom_followed_by_a_clean_run_draws_no_mark() {
        let json = r#"{"session":"s","rows":[
            {"key":"poly#138","stage":"running","eta_s":1800,"started":1790000000000,
             "usage":{"runs":16,"peak_mb":9000,"oom_kills":1,"last_exit":0,"last_peak_mb":239}},
            {"key":"poly#139","stage":"running","eta_s":1800,"started":1790000000000,
             "usage":{"runs":2,"peak_mb":3000,"oom_kills":1,"last_exit":137}},
            {"key":"poly#140","stage":"running","eta_s":1800,"started":1790000000000,
             "usage":{"runs":3,"peak_mb":3000,"oom_kills":2,"last_exit":137,"oom_open":false}}]}"#;
        let t = build(Some(&feed(json)), &[], T0 + 60_000);
        let line = |k: &str| t.lines.iter().find(|l| l.click.key == k).unwrap();
        assert!(!line("poly#138").oom);
        assert!(!line("poly#138").usage.contains("OOM"));
        assert!(
            line("poly#138")
                .click
                .facts
                .iter()
                .any(|f| f.ends_with("OOM-killed x1 earlier")),
            "{:?}",
            line("poly#138").click.facts
        );
        assert!(line("poly#139").oom, "an old row whose last run was killed");
        assert!(line("poly#139").usage.starts_with("OOM"));
        assert!(!line("poly#140").oom, "closed by a bigger grant");
    }

    /// A Running row's cell is what its commands use now, not
    /// its lease; a Done row's is its memory peak alone.
    #[test]
    fn a_running_row_shows_its_use_now_and_a_done_row_its_peak() {
        let json = r#"{"session":"s","rows":[
            {"key":"demo#12","stage":"running","eta_s":1800,"started":1790000000000,
             "lease":{"state":"granted","id":"s:demo#12","cpu":3,"ram_mb":3072,
                      "granted_at":1790000000000},
             "usage":{"runs":1,"peak_mb":2150,"cpu_s":45,"cap_ram_mb":3072}},
            {"key":"demo#13","stage":"running","started":1790000000000,
             "lease":{"state":"granted","id":"s:demo#13","cpu":1,"ram_mb":128,
                      "granted_at":1790000000000},
             "usage":{"runs":1,"peak_mb":128,"oom_kills":1,"oom_open":true}},
            {"key":"demo#14","stage":"done","started":1790000000000,"ended":1790000600000,
             "usage":{"runs":2,"peak_mb":512}},
            {"key":"demo#15","stage":"done","started":1790000000000,"ended":1790000600000}]}"#;
        // No run going: zero in the cell, the lease in the header only.
        let t = build(Some(&feed(json)), &[], T0 + 60_000);
        assert_eq!(t.lines[0].usage.trim_end(), "      0% CPU  ·  0.0G");
        assert!(!t.lines[0].oom);
        let facts = &t.lines[0].click.facts;
        assert!(facts.iter().any(|f| f == "holds 3G"), "{facts:?}");
        assert!(
            facts
                .iter()
                .any(|f| f.starts_with("peak 2.1G of 3G, 45s CPU"))
        );
        assert_eq!(t.lines[1].usage.trim_end(), "OOM   0% CPU  ·  0.0G");
        assert!(t.lines[1].oom);
        assert_eq!(t.lines[2].usage.trim_end(), "                 0.5G");
        // Done with nothing measured: a zero peak, not a blank.
        assert_eq!(t.lines[3].usage.trim_end(), "                 0.0G");
        // A run going: its use now. Another session's run of the same key
        // is not this row's.
        let mut other = task_live("demo#13", 50, 100);
        other.session = "elsewhere".into();
        let f = with_live(
            feed(json),
            &["s"],
            &[
                task_live("demo#12", 14, 4300),
                other,
                task_live("demo#14", 9, 9),
            ],
        );
        let t = build(Some(&f), &[], T0 + 60_000);
        // The GPU slot follows the machine; the rest is the same anywhere.
        let gpu = session_use::gpu::present();
        let live = |cpu_pct, mem_mb| {
            Some(RunLive {
                cpu_pct,
                mem_mb,
                gpu_mb: None,
            })
        };
        assert_eq!(
            t.lines[0].usage,
            usage_cell(live(14, 4300), None, false, gpu)
        );
        assert!(t.lines[0].usage.starts_with("     14% CPU  ·  4.2G"));
        assert_eq!(t.lines[1].usage, usage_cell(live(0, 0), None, true, gpu));
        assert!(t.lines[1].usage.starts_with("OOM   0% CPU  ·  0.0G"));
        assert_eq!(
            t.lines[2].usage,
            usage_cell(None, Some(512), false, gpu),
            "a Done row keeps its peak"
        );
        assert!(t.lines[2].usage.starts_with("                 0.5G"));
    }

    /// A worker's commands show in its row with no `manage run` at all; a
    /// run under that worker is not added twice, one elsewhere is added; a
    /// worker with no feed row shows its own; a Done row keeps its peak.
    #[test]
    fn a_workers_processes_show_in_its_row_without_a_run() {
        let json = r#"{"session":"s","rows":[
            {"key":"sim#1","stage":"running","agent_id":"a1","started":1790000000000,"eta_s":1800},
            {"key":"sim#2","stage":"running","agent_id":"a2","started":1790000000000,"eta_s":1800},
            {"key":"sim#3","stage":"running","agent_id":"a3","started":1790000000000,"eta_s":1800},
            {"key":"sim#4","stage":"done","agent_id":"a1","started":1790000000000,"ended":1790000600000,
             "usage":{"runs":1,"peak_mb":512}}]}"#;
        let rl = |cpu_pct, mem_mb| RunLive {
            cpu_pct,
            mem_mb,
            gpu_mb: None,
        };
        let workers = HashMap::from([
            ("a1".to_string(), rl(60, 2048)),
            ("a2".to_string(), rl(30, 1024)),
        ]);
        let mut under_a2 = task_live("sim#2", 20, 512);
        under_a2.agent = Some("a2".into());
        let elsewhere = task_live("sim#3", 10, 100);
        let runs = [under_a2, elsewhere];
        let f = with_workers(
            with_live(feed(json), &["s"], &runs),
            &["s"],
            &runs,
            &workers,
        );
        let row = |k: &str| f.rows.iter().find(|r| r.key == k).unwrap().live;
        assert_eq!(row("sim#1"), Some(rl(60, 2048)), "no run: the worker's");
        assert_eq!(row("sim#2"), Some(rl(30, 1024)), "its run is inside it");
        assert_eq!(
            row("sim#3"),
            Some(rl(10, 100)),
            "no worker figure: the run's"
        );
        let t = build(Some(&f), &[], T0 + 60_000);
        assert!(
            t.lines[0].usage.contains("60% CPU"),
            "{:?}",
            t.lines[0].usage
        );
        let done = t.lines.iter().find(|l| l.stage == Stage::Done).unwrap();
        assert!(
            done.usage.trim_start().starts_with("0.5G"),
            "{:?}",
            done.usage
        );
        // A worker with no feed row at all: its row shows the worker's.
        let live = live(
            r#"{"session_id":"s","tasks":[{"id":"a1","status":"running",
                "description":"sims","startTime":1789999958000}]}"#,
        );
        let clock = Clock {
            workers: &workers,
            ..Clock::plain()
        };
        let t = build_at(None, &live, T0 + 60_000, &clock, &Logs::new());
        assert!(
            t.lines[0].usage.contains("60% CPU"),
            "{:?}",
            t.lines[0].usage
        );
    }

    /// A Running row's live CPU and memory are each coloured by their
    /// weight on the whole machine, as the rail's figures are
    /// (giverny#305): the row's own colour under 20 %, amber to 50 %, poppy
    /// over it. Memory is a share of all the RAM. The OOM tag, the
    /// separator, the GPU slot and a Done row's peak keep the cell's
    /// colour, and the runs sit in the cell's own columns.
    #[test]
    fn a_running_rows_figures_are_coloured_by_their_weight_on_the_machine() {
        let c = Chrome::from_theme(&giverny_term::render::theme::Theme::monet_dark());
        let rl = |cpu_pct, mem_mb| RunLive {
            cpu_pct,
            mem_mb,
            gpu_mb: None,
        };
        // Of 24G: 4.56G is 19 %, 6G 25 %, 12.24G 51 %.
        let ram = 24_000;
        assert_eq!(run_load(rl(5, 4_560), ram), (Load::Light, Load::Light));
        assert_eq!(run_load(rl(25, 100), ram), (Load::Busy, Load::Light));
        assert_eq!(run_load(rl(50, 6_000), ram), (Load::Busy, Load::Busy));
        assert_eq!(run_load(rl(51, 12_240), ram), (Load::Heavy, Load::Heavy));
        assert_eq!(run_load(rl(5, 0), 0), (Load::Light, Load::Light));

        // Drawn: each figure in its own colour, in its own columns.
        for gpu in [false, true] {
            let on_gpu = RunLive {
                gpu_mb: Some(1_200),
                ..rl(25, 100)
            };
            let cell = usage_cell(Some(on_gpu), None, true, gpu);
            let runs = usage_inks(&cell, Some((Load::Busy, Load::Light)), RUNNING, &c);
            let mut back = vec![' '; cell.chars().count()];
            for (at, text, _) in &runs {
                for (i, ch) in text.chars().enumerate() {
                    back[at + i] = ch;
                }
            }
            assert_eq!(back.into_iter().collect::<String>(), cell, "same columns");
            let ink = |piece: &str| {
                runs.iter()
                    .find(|(_, t, _)| t.contains(piece))
                    .map(|r| r.2)
                    .unwrap_or_else(|| panic!("{piece:?} in {runs:?}"))
            };
            assert_eq!(ink("25% CPU"), c.amber);
            assert_eq!(ink("0.1G"), RUNNING);
            assert_eq!(ink("OOM"), RUNNING, "the tag keeps the cell's colour");
            assert_eq!(ink("·"), RUNNING);
            if gpu {
                assert_eq!(ink("gpu"), RUNNING);
            }
        }
        let cell = usage_cell(Some(rl(60, 13_000)), None, false, false);
        let runs = usage_inks(&cell, Some((Load::Heavy, Load::Heavy)), RUNNING, &c);
        let inks: Vec<_> = runs.iter().map(|r| (r.1.trim(), r.2)).collect();
        assert_eq!(
            inks,
            [("60% CPU", c.poppy), ("·", RUNNING), ("13G", c.poppy)]
        );
        // A Done row's peak is the cell's colour, whatever it weighs.
        let done = usage_cell(None, Some(20_000), false, false);
        assert_eq!(usage_inks(&done, None, DONE, &c), [(0, done.clone(), DONE)]);

        // From the feed: a Running row carries its load, a Done row none.
        let json = r#"{"session":"s","rows":[
            {"key":"sim#1","stage":"running","agent_id":"a1","started":1790000000000,"eta_s":1800},
            {"key":"sim#2","stage":"running","agent_id":"a2","started":1790000000000,"eta_s":1800},
            {"key":"sim#3","stage":"running","agent_id":"a3","started":1790000000000,"eta_s":1800},
            {"key":"sim#4","stage":"done","agent_id":"a1","started":1790000000000,"ended":1790000600000,
             "usage":{"runs":1,"peak_mb":512}}]}"#;
        let workers = HashMap::from([
            ("a1".to_string(), rl(60, 1)),
            ("a2".to_string(), rl(25, 1)),
            ("a3".to_string(), rl(5, 1)),
        ]);
        let f = with_workers(feed(json), &["s"], &[], &workers);
        let t = build(Some(&f), &[], T0 + 60_000);
        let load = |k: &str| {
            t.lines
                .iter()
                .find(|l| l.id.ends_with(k) || l.click.key == k)
                .unwrap()
                .usage_load
        };
        assert_eq!(load("sim#1"), Some((Load::Heavy, Load::Light)));
        assert_eq!(load("sim#2"), Some((Load::Busy, Load::Light)));
        assert_eq!(load("sim#3"), Some((Load::Light, Load::Light)));
        assert_eq!(load("sim#4"), None);
    }

    #[test]
    fn a_running_row_keeps_its_lease_in_the_header_and_a_queued_next_up_row_says_what_it_waits_for()
    {
        let t = build(Some(&feed(LEASED)), &[], T0 + 60_000);
        let (run, next) = (&t.lines[0], &t.lines[1]);
        assert_eq!(
            run.usage.trim_end(),
            "      0% CPU  ·  0.0G",
            "the lease is not the cell"
        );
        assert!(
            run.click
                .facts
                .iter()
                .any(|f| f == "holds 3G, slot cargo:/x/target"),
            "{:?}",
            run.click.facts
        );
        assert_eq!(next.now, "queued for 3G behind demo#12");
        assert_eq!(next.usage, "", "said in NOW, not twice");
        // A Running row between commands: the column is there all the same.
        assert_eq!(Cols::new(&t, 100).usew, t.lines[0].usage.chars().count());
        // Every row Next up: no use column, the title keeps its width.
        let queued = Table {
            lines: vec![next.clone()],
            ..Default::default()
        };
        assert_eq!(Cols::new(&queued, 100).usew, 0);
        let busy = build(
            Some(&with_live(
                feed(LEASED),
                &["s"],
                &[task_live("demo#12", 14, 4300)],
            )),
            &[],
            T0 + 60_000,
        );
        let cols = Cols::new(&busy, 100);
        let drawn = compose(&cols.segments(&busy.lines[1]));
        assert!(drawn.ends_with("queued for 3G behind demo#12"), "{drawn}");
        let drawn = compose(&cols.segments(&busy.lines[0]));
        assert!(drawn.contains("14% CPU  ·  4.2G"), "{drawn}");
        assert_eq!(cols.usew, busy.lines[0].usage.chars().count());
        assert!(cols.taskw < Cols::new(&queued, 100).taskw);
    }

    /// giverny#282, giverny#310: a Next up row says what it waits for; one
    /// that waits on nothing recorded is drawn as any Next up row, and is
    /// its manager's chore once it has sat a while.
    #[test]
    fn a_next_up_row_says_what_it_waits_for_and_an_idle_one_is_its_managers() {
        let at = |m: u64| ts(T0 + m * 60_000);
        let json = format!(
            r#"{{"version":1,"rows":[
              {{"key":"280-reuse","stage":"running","started":"{s}","eta_s":1800}},
              {{"key":"265-x","stage":"planned","planned":"{p}","held":"a quiet machine"}},
              {{"key":"282-hold","stage":"planned","planned":"{p}","after":"280-reuse"}},
              {{"key":"283-y","stage":"planned","planned":"{p}",
                "lease":{{"state":"queued","position":2,"behind":"280-reuse","short":["cpu"],
                          "cpu":3,"ram_mb":3072}}}},
              {{"key":"284-idle","stage":"planned","planned":"{p}","eta_s":600}},
              {{"key":"285-new","stage":"planned","planned":"{fresh}"}},
              {{"key":"286-unheld","stage":"planned","planned":"{p}","unheld":"{fresh}"}},
              {{"key":"287-after-done","stage":"planned","planned":"{p}","after":"279-done"}},
              {{"key":"288-old-writer","stage":"planned"}},
              {{"key":"279-done","stage":"done","started":"{p}","ended":"{fresh}"}}]}}"#,
            s = at(0),
            p = at(0),
            fresh = at(28),
        );
        let t = build(Some(&feed(&json)), &[], T0 + 30 * 60_000);
        let line = |k: &str| t.lines.iter().find(|l| l.id == k).unwrap();
        let now = |k: &str| (line(k).now.as_str(), line(k).flag);
        assert_eq!(now("265-x"), ("held: a quiet machine", false));
        assert!(
            line("265-x")
                .click
                .facts
                .iter()
                .any(|f| f == "held: a quiet machine"),
            "the reason in full in the header"
        );
        assert_eq!(now("282-hold"), ("after 280-reuse", false));
        assert_eq!(now("283-y"), ("queued for CPU behind 280-reuse", false));
        assert_eq!(now("284-idle"), ("", false), "idle 30m: the manager's");
        assert_eq!(now("285-new"), ("", false), "planned two minutes ago");
        assert_eq!(now("286-unheld"), ("", false), "free for two minutes");
        assert_eq!(
            now("287-after-done"),
            ("", false),
            "its task landed two minutes ago"
        );
        assert_eq!(
            now("288-old-writer"),
            ("", false),
            "never said when it was planned"
        );
        let later = build(Some(&feed(&json)), &[], T0 + 40 * 60_000);
        assert!(
            later
                .lines
                .iter()
                .all(|l| !l.flag || l.stage == Stage::Running),
            "no Next up row flagged for the person"
        );
        // The manager's, from the same logic: every idle row with its
        // instant, due only past five minutes of it.
        let f = feed(&json);
        let idle = |now: u64| -> Vec<(String, u64)> {
            due_chores(Drawn::Manager, &f, &[], &Logs::new(), &NO_SHELLS, now)
                .into_iter()
                // 280-reuse has no worker: its manager's to staff
                // (giverny#334), not an idle Next up row.
                .filter(|c| {
                    let staff = c.kind == manage_chores::Kind::Worker;
                    assert_eq!(staff, c.key == "280-reuse", "{c:?}");
                    !staff
                })
                .map(|c| {
                    assert_eq!(c.kind, manage_chores::Kind::Start);
                    (c.key, c.since)
                })
                .collect()
        };
        let k = |s: &str, m: u64| (s.to_string(), T0 + m * 60_000);
        assert_eq!(idle(T0 + 30 * 60_000), [k("284-idle", 0)]);
        assert_eq!(
            idle(T0 + 40 * 60_000),
            [
                k("284-idle", 0),
                k("285-new", 28),
                k("286-unheld", 28),
                k("287-after-done", 28),
            ]
        );
        let line = chores(Drawn::Manager, Some(&f), &[], &Logs::new(), &NO_SHELLS, 0)
            .into_iter()
            .find(|c| c.key == "284-idle")
            .unwrap()
            .line(T0 + 30 * 60_000);
        assert!(
            line.contains("`284-idle` has waited 30m with a free lane"),
            "{line}"
        );
    }

    /// A row's copy is only as fresh as its session's
    /// last claim. The ledger decides.
    #[test]
    fn the_ledger_wins_over_a_rows_stale_copy() {
        let f = feed(LEASED);
        // Both gone from the ledger (released, expired): nothing is held.
        let empty = with_ledger(&f, &["s"], &Ledger::default());
        assert!(empty.rows.iter().all(|r| r.lease.is_none()));

        // demo#12 released; demo#13 still waits, now first, short of RAM
        // another session's task holds: behind that one, not its copy's.
        let l = ledger(
            r#"{"version":1,"leases":[
              {"id":"o:acme#5","session":"o","task":"acme#5","cpu":4,"ram_mb":8192,
               "granted_at":1790000000000,"heartbeat_at":1790000000000}],
             "queue":[
              {"id":"s:demo#13","session":"s","task":"demo#13","cpu":2,"ram_mb":3072,
               "queued_at":1790000000000,"heartbeat_at":1790000000000}]}"#,
        );
        let g = with_ledger(&f, &["s"], &l);
        assert!(g.rows[0].lease.is_none(), "the released lease is gone");
        let q = g.rows[1].lease.as_ref().unwrap();
        assert_eq!(q.state, LeaseState::Queued);
        assert_eq!(q.position, Some(1));
        assert_eq!(
            q.behind.as_deref(),
            Some("acme#5"),
            "demo#12 holds nothing now"
        );
        assert_eq!(queued_note(q), "queued for 3G behind acme#5");

        // giverny#429: granted on another session's call and untaken; a
        // grant that lapsed is queued again, waiting for its manager.
        let l = ledger(
            r#"{"version":1,"leases":[
              {"id":"s:demo#12","session":"s","task":"demo#12","ram_mb":3072,
               "granted_at":1790000000000,"heartbeat_at":1790000000000,
               "uncollected":{"queued_at":1790000000000,"request":{"ram_mb":3072}}}],
             "queue":[
              {"id":"s:demo#13","session":"s","task":"demo#13","ram_mb":3072,
               "queued_at":1790000000000,"heartbeat_at":1790000000000,
               "lapsed":{"at":1790000000000}}]}"#,
        );
        let g = with_ledger(&f, &["s"], &l);
        assert!(g.rows[0].lease.as_ref().unwrap().uncollected);
        assert_eq!(
            queued_note(g.rows[1].lease.as_ref().unwrap()),
            "queued, grant lapsed, waiting for its manager"
        );

        // A lease in the ledger the row never got a copy of (another
        // writer, or the tab's older session id) is drawn.
        let l = ledger(
            r#"{"version":1,"leases":[
              {"id":"old:demo#12","session":"old","task":"demo#12","cpu":1,"ram_mb":1024,
               "slots":["cargo:/x/target"],"granted_at":1790000000000,"heartbeat_at":1790000000000}],
             "queue":[
              {"id":"s:demo#13","session":"s","task":"demo#13","cpu":1,"ram_mb":0,
               "slots":["cargo:/x/target"],"queued_at":1790000000000,"heartbeat_at":1790000000000}]}"#,
        );
        let bare = feed(r#"{"session":"s","rows":[{"key":"demo#12","stage":"running"}]}"#);
        let g = with_ledger(&bare, &["s", "old"], &l);
        assert_eq!(
            lease_fact(g.rows[0].lease.as_ref().unwrap()),
            "holds 1G, slot cargo:/x/target"
        );
        assert!(
            with_ledger(&bare, &["s"], &l).rows[0].lease.is_none(),
            "not our session"
        );
        // Queued with no copy: behind the holder of the slot it asks for,
        // and what is short is not known, so not named.
        let g = with_ledger(
            &feed(r#"{"session":"s","rows":[{"key":"demo#13","stage":"planned"}]}"#),
            &["s"],
            &l,
        );
        let q = g.rows[0].lease.as_ref().unwrap();
        assert_eq!(queued_note(q), "queued behind demo#12");
    }

    /// giverny#333: a queued row names what its claim found short, in its
    /// own sizes, and who is in the way of that now — the request queued
    /// ahead that wants it, else a lease holding it — never just its RAM
    /// behind whoever holds a lease.
    #[test]
    fn a_queued_row_names_what_is_short_and_who_is_in_the_way() {
        // fold146 holds 2G; 899x, first in line, asks 3G; 283 asks 3G. The
        // old `cpu` figures are ignored (giverny#387).
        let l = ledger(
            r#"{"version":1,"leases":[
              {"id":"o:fold146","session":"o","task":"fold146","cpu":10,"ram_mb":2048,
               "granted_at":1790000000000,"heartbeat_at":1790000000000}],
             "queue":[
              {"id":"o:899x","session":"o","task":"899x","cpu":3,"ram_mb":3072,
               "queued_at":1790000000000,"heartbeat_at":1790000000000},
              {"id":"s:283","session":"s","task":"283","cpu":3,"ram_mb":3072,
               "queued_at":1790000060000,"heartbeat_at":1790000060000}]}"#,
        );
        let note = |short: &str, behind: &str| {
            let f = feed(&format!(
                r#"{{"session":"s","rows":[{{"key":"283","stage":"planned",
                  "lease":{{"state":"queued","position":2,"behind":"{behind}",
                            "short":{short},"cpu":3,"ram_mb":3072}}}}]}}"#
            ));
            let g = with_ledger(&f, &["s"], &l);
            queued_note(g.rows[0].lease.as_ref().unwrap())
        };
        // CPU busy: held back by the request ahead, which goes first.
        assert_eq!(note(r#"["cpu"]"#, "fold146"), "queued for CPU behind 899x");
        // Both short: both named, and 899x is still ahead.
        assert_eq!(
            note(r#"["cpu","ram"]"#, "fold146"),
            "queued for CPU, 3G behind 899x"
        );
        // Short of a slot nobody ahead wants or holds now: behind nobody,
        // whatever its copy named.
        assert_eq!(note(r#"["slot cargo:/x"]"#, "899x"), "queued for a slot");

        // With nobody ahead: behind the lease holding what is short.
        let first = ledger(
            r#"{"version":1,"leases":[
              {"id":"o:fold146","session":"o","task":"fold146","cpu":10,"ram_mb":12288,
               "granted_at":1790000000000,"heartbeat_at":1790000000000},
              {"id":"o:idle","session":"o","task":"idle","cpu":0,"ram_mb":1024,
               "granted_at":1790000000000,"heartbeat_at":1790000000000}],
             "queue":[
              {"id":"s:283","session":"s","task":"283","cpu":3,"ram_mb":3072,
               "queued_at":1790000060000,"heartbeat_at":1790000060000}]}"#,
        );
        let only = |short: &str| {
            let f = feed(&format!(
                r#"{{"session":"s","rows":[{{"key":"283","stage":"planned",
                  "lease":{{"state":"queued","position":1,"behind":"idle",
                            "short":{short},"cpu":3,"ram_mb":3072}}}}]}}"#
            ));
            let g = with_ledger(&f, &["s"], &first);
            queued_note(g.rows[0].lease.as_ref().unwrap())
        };
        // A busy CPU with nobody ahead: the pane does not know who builds.
        assert_eq!(only(r#"["cpu"]"#), "queued for CPU");
        assert_eq!(only(r#"["ram"]"#), "queued for 3G behind fold146");
        assert_eq!(
            only(r#"["cpu","ram"]"#),
            "queued for CPU, 3G behind fold146"
        );
    }

    #[test]
    fn a_session_no_plugin_hook_ran_in_is_marked_first_in_the_footer() {
        assert_eq!(with_hookless(None, false), None);
        assert_eq!(
            with_hookless(Some("s 3".into()), false).as_deref(),
            Some("s 3")
        );
        assert_eq!(with_hookless(None, true).as_deref(), Some(HOOKLESS));
        let both = with_hookless(Some("s 3".into()), true).unwrap();
        assert!(
            both.starts_with("no guard: ") && both.ends_with(" · s 3"),
            "{both}"
        );
    }

    #[test]
    fn the_feed_footer_is_carried_verbatim() {
        let f = feed(r#"{"rows":[{"key":"k","stage":"planned"}],"footer":{"text":"session 3"}}"#);
        let t = build(Some(&f), &[], T0);
        assert_eq!(t.footer.as_deref(), Some("session 3"));
    }

    /// The manager's name is the status line's (giverny#330), not the pane's.
    #[test]
    fn the_managers_name_leaves_the_pane_as_it_was() {
        let named = feed(r#"{"name":"Juniper Belfry","rows":[{"key":"k","stage":"planned"}]}"#);
        let plain = feed(r#"{"rows":[{"key":"k","stage":"planned"}]}"#);
        assert_eq!(build(Some(&named), &[], T0), build(Some(&plain), &[], T0));
    }

    #[test]
    fn a_row_is_tinted_only_while_the_pointer_is_on_it() {
        // Once the pointer has left, a clicked row looks like
        // any other.
        assert_eq!(row_tint(false, false), None);
        assert_eq!(row_tint(true, false), Some(0.10));
        assert_eq!(row_tint(true, true), Some(0.22));
    }

    #[test]
    fn right_at_ends_the_text_on_the_column() {
        assert_eq!(right_at(10, "12m"), 7);
        assert_eq!(right_at(2, "long"), 0);
    }

    #[test]
    fn the_columns_leave_the_bar_its_lane_and_ignore_whether_it_shows() {
        let (width, cw, lane) = (800.0, 8.0, 10.0);
        let cols = table_cols(width, cw, 1.0, lane);
        // The last cell ends clear of the bar's lane.
        assert!((INSET + cols) as f32 * cw <= width - lane);
        // The width the scroll area hands its content drops by the lane
        // while the bar shows; the table is laid out from the width outside
        // it, so that drop changes nothing; laid out from the inner width,
        // it would have lost a column.
        assert_ne!(cols, table_cols(width - lane, cw, 1.0, lane));
        // A lane wider than the two cells right of the table: the table
        // gives way rather than run under the bar.
        let wide = table_cols(width, cw, 1.0, 30.0);
        assert!((INSET + wide) as f32 * cw <= width - 30.0, "{wide}");
        // A cramped pane still gets a readable table.
        assert_eq!(table_cols(100.0, cw, 1.0, lane), 40);
    }

    /// The table is as wide as Claude Code's status line: the terminal's
    /// columns less two at each side, counted as the terminal counts them,
    /// in whole device pixels, at any zoom.
    #[test]
    fn the_table_spans_the_status_line() {
        // 100 columns of 8pt.
        assert_eq!(table_cols(800.0, 8.0, 1.0, 10.0), 96);
        assert_eq!(table_cols(807.9, 8.0, 1.0, 10.0), 96);
        assert_eq!(table_cols(808.0, 8.0, 1.0, 10.0), 97);
        // 11px cells at 125%: 8.8pt; 803.3pt is 1004.1px, 91 columns.
        assert_eq!(table_cols(803.3, 8.8, 1.25, 10.0), 87);
        // At 150%, a cell of 9px is 6pt: 1200pt is 1800px, 200 columns.
        assert_eq!(table_cols(1200.0, 6.0, 1.5, 10.0), 196);
    }

    /// The use column is the last, and on screen it ends where the status
    /// line drawn right above the pane does: [`INSET`] cells in from the
    /// terminal's edges. The status line no longer shows the session's use
    /// for it to line up under (giverny#384).
    #[test]
    fn the_use_column_ends_where_the_status_line_does() {
        let gpu_live = |gpu_mb| task_live_gpu("demo#12", 14, 4300, gpu_mb);
        for (columns, gpu) in [(120usize, None), (137, None), (160, Some(1229))] {
            let width_pt = columns as f32 * 8.0 + 3.0;
            let cols = table_cols(width_pt, 8.0, 1.0, 10.0);
            let mut f = with_live(feed(LEASED), &["s"], &[gpu_live(gpu)]);
            // The machine's GPU stands in for `gpu::present`.
            let t = build(Some(&f), &[], T0 + 60_000);
            let mut line = t.lines[0].clone();
            let live = f.rows[0].live.take();
            line.usage = usage_cell(live, None, false, gpu.is_some());
            let table = Table {
                lines: vec![line.clone()],
                ..Default::default()
            };
            let layout = Cols::new(&table, cols);
            let segs = layout.segments(&line);
            // The use cell is the last one drawn, ending on the last column.
            let (at, cell) = segs
                .iter()
                .max_by_key(|(at, s)| at + s.chars().count())
                .unwrap();
            assert_eq!(cell, &line.usage);
            assert_eq!(at + cell.chars().count(), cols);
            let row = format!("{}{}", " ".repeat(INSET), compose(&segs));
            assert!(row.ends_with(&line.usage), "{row:?}");
            // It ends where the status line does: two columns in from
            // the terminal's right edge.
            assert_eq!(row.chars().count(), columns - INSET);
        }
    }

    /// The pane's shape in a headless egui (Chrome's scroll style, a bottom
    /// panel, a vertical scroll area), with `rows` rows of 20pt: the width
    /// measured outside the scroll area, where the table is laid out from,
    /// and the one its content is handed inside.
    fn pane_widths(rows: usize) -> (f32, f32) {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| {
            s.spacing.scroll.floating_allocated_width = s.spacing.scroll.bar_width;
            s.animation_time = 0.0;
        });
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let (mut outer, mut inner) = (0.0, 0.0);
        // The scroll area learns its content size a frame late.
        for _ in 0..3 {
            let _ = ctx.run_ui(input(), |ui| {
                egui::Panel::bottom("pane")
                    .exact_size(200.0)
                    .show(ui, |ui| {
                        outer = ui.available_width();
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                inner = ui.available_width();
                                for _ in 0..rows {
                                    ui.allocate_exact_size(
                                        egui::vec2(ui.available_width(), 20.0),
                                        Sense::hover(),
                                    );
                                }
                            });
                    });
            });
        }
        (outer, inner)
    }

    #[test]
    fn the_columns_stay_put_as_the_rows_start_to_overflow() {
        let (cw, lane) = (8.0, 10.0);
        let (outer_fit, inner_fit) = pane_widths(3);
        let (outer_over, inner_over) = pane_widths(30);
        // The bug: inside the scroll area the width drops once a bar shows.
        assert!(inner_over < inner_fit, "{inner_over} vs {inner_fit}");
        // The fix: the table is laid out from the width outside it.
        assert_eq!(outer_fit, outer_over);
        assert_eq!(
            table_cols(outer_fit, cw, 1.0, lane),
            table_cols(outer_over, cw, 1.0, lane)
        );
    }

    /// Frames of the pane's own shape — its frame, a resizable bottom panel
    /// sized by [`follow_rows`], a vertical scroll area of `rows` rows of
    /// 20pt, with `height` giving the fitted height from (rows, gap, margin)
    /// — run in a headless egui over one `fit`. Returns the panel's stored
    /// outer height, and the scroll area's content and viewport heights.
    fn run_pane(
        ctx: &egui::Context,
        fit: &mut Option<f32>,
        rows: usize,
        height: impl Fn(usize, f32, f32) -> f32,
    ) -> (f32, f32, f32) {
        let row_h = 20.0;
        let id = egui::Id::new("pane");
        let (mut content, mut viewport) = (0.0, 0.0);
        // The scroll area learns its content size a frame late.
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                let frame = pane_frame(Color32::BLACK);
                let want = height(
                    rows,
                    ui.spacing().item_spacing.y,
                    frame.total_margin().sum().y,
                );
                let want = want.clamp(row_h * 1.5, 300.0);
                follow_rows(ui.ctx(), id, fit, want);
                egui::Panel::bottom(id)
                    .resizable(true)
                    .default_size(want)
                    .size_range(row_h * 1.5..=300.0)
                    .frame(frame)
                    .show(ui, |ui| {
                        let out = egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for _ in 0..rows {
                                    ui.allocate_exact_size(
                                        egui::vec2(ui.available_width(), row_h),
                                        Sense::hover(),
                                    );
                                }
                            });
                        content = out.content_size.y;
                        viewport = out.inner_rect.height();
                    });
            });
        }
        let stored = egui::containers::panel::PanelState::load(ctx, id).map_or(0.0, |s| s.size().y);
        (stored, content, viewport)
    }

    fn chrome_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| {
            s.spacing.scroll.floating_allocated_width = s.spacing.scroll.bar_width;
            s.animation_time = 0.0;
        });
        ctx
    }

    #[test]
    fn the_pane_is_tall_enough_for_every_row_it_holds() {
        let fitted = |n, gap, margin| pane_height(n, 20.0, gap, margin);
        // What the pane used to ask for: rows and margin, no gaps.
        let gapless = |n: usize, _gap: f32, margin: f32| n as f32 * 20.0 + margin;
        for rows in [1, 2, 3, 7, 12] {
            let (_, content, viewport) = run_pane(&chrome_ctx(), &mut None, rows, fitted);
            assert!(content <= viewport, "{rows} rows: {content} > {viewport}");
        }
        // The bug: seven rows overflow by six gaps, and the last one sits
        // under the scroll area's fade.
        let (_, content, viewport) = run_pane(&chrome_ctx(), &mut None, 7, gapless);
        assert!(content > viewport, "{content} <= {viewport}");
    }

    #[test]
    fn the_pane_follows_its_rows_until_it_is_dragged() {
        let fitted = |n, gap, margin| pane_height(n, 20.0, gap, margin);
        let ctx = chrome_ctx();
        let mut fit = None;
        let (three, ..) = run_pane(&ctx, &mut fit, 3, fitted);
        let (seven, content, viewport) = run_pane(&ctx, &mut fit, 7, fitted);
        assert!(seven > three, "{seven} vs {three}");
        assert!(content <= viewport, "{content} > {viewport}");
        // A height the pane did not choose is the user's drag: it stands.
        ctx.data_mut(|d| {
            d.insert_persisted(
                egui::Id::new("pane"),
                egui::containers::panel::PanelState {
                    outer_rect: egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 90.0),
                    ),
                },
            )
        });
        let (dragged, ..) = run_pane(&ctx, &mut fit, 12, fitted);
        assert_eq!(dragged, 90.0);
        assert_eq!(fit, None);
    }

    // ------------------------------------------------------ held clocks ----

    const MIN: u64 = 60_000;

    fn utc<'a>(holds: &'a [Hold], limit: Option<&'a Limit>) -> Clock<'a> {
        Clock {
            holds,
            sleeps: &[],
            limit,
            tz: jiff::tz::TimeZone::UTC,
            workers: &NO_WORKERS,
            etas: &NO_ETAS,
            shells: &NO_SHELLS,
        }
    }

    /// A worker ten minutes into a 30-minute task at `T0`.
    fn ten_minutes_in() -> (Vec<SubagentRow>, Feed) {
        let rows = live(
            r#"{"session_id":"s","tasks":[{"id":"a1","status":"running",
                "startTime":1789999400000,"label":"Editing"}]}"#,
        );
        let f = feed(r#"{"rows":[{"key":"g#1","stage":"running","agent_id":"a1","eta_s":1800}]}"#);
        (rows, f)
    }

    #[test]
    fn a_running_row_is_frozen_while_the_limit_is_out() {
        let (rows, f) = ten_minutes_in();
        // Out at T0 (Mon 14:13 UTC), reopening an hour later.
        let limit = Limit {
            reopens_ms: Some(T0 + 60 * MIN),
            window: Some("5h"),
        };
        let mut holds = Vec::new();
        track(&mut holds, Some(&limit), T0);
        for later in [0, 20 * MIN, 59 * MIN] {
            let now = T0 + later;
            track(&mut holds, Some(&limit), now);
            let t = build_at(
                Some(&f),
                &rows,
                now,
                &utc(&holds, Some(&limit)),
                &Logs::new(),
            );
            let l = &t.lines[0];
            assert_eq!(l.elapsed, "10:00", "{later}");
            assert_eq!(l.eta, "~20m", "{later}");
            assert_eq!(l.now, "5h limit → 15:13");
        }
        // Without the hold, the same row runs on past its estimate.
        let t = build(Some(&f), &rows, T0 + 59 * MIN);
        assert_eq!(t.lines[0].eta, "-~39m");
    }

    #[test]
    fn the_clocks_carry_on_from_where_they_stopped_after_the_reset() {
        let (rows, f) = ten_minutes_in();
        let limit = Limit {
            reopens_ms: Some(T0 + 60 * MIN),
            window: Some("5h"),
        };
        let mut holds = Vec::new();
        track(&mut holds, Some(&limit), T0);
        track(&mut holds, Some(&limit), T0 + 30 * MIN);
        // The pane was not drawn at the reset; the first frame after it
        // closes the hold at the reset itself, not now.
        let now = T0 + 65 * MIN;
        track(&mut holds, None, now);
        assert_eq!(holds[0].until_ms, Some(T0 + 60 * MIN));
        let t = build_at(Some(&f), &rows, now, &utc(&holds, None), &Logs::new());
        // 10 minutes before, 5 after: the hour out is not work.
        assert_eq!(t.lines[0].elapsed, "15:00");
        assert_eq!(t.lines[0].eta, "~15m");
        assert_eq!(t.lines[0].now, "Editing");
        // A meter still showing the lapsed limit is no limit: it lets go.
        let t = build_at(
            Some(&f),
            &rows,
            now,
            &utc(&holds, Some(&limit)),
            &Logs::new(),
        );
        assert_eq!(t.lines[0].now, "Editing");
        let mut again = holds.clone();
        track(&mut again, Some(&limit), now);
        assert_eq!(again, holds, "a reset that came round opens nothing");
    }

    #[test]
    fn a_night_asleep_is_off_every_clock() {
        let (rows, f) = ten_minutes_in();
        // Asleep from T0 for eight hours; drawn five minutes after waking.
        let night = [sleeps::Span {
            since: T0,
            until: T0 + 480 * MIN,
        }];
        let now = T0 + 485 * MIN;
        let clock = Clock {
            sleeps: &night,
            ..utc(&[], None)
        };
        let t = build_at(Some(&f), &rows, now, &clock, &Logs::new());
        assert_eq!(t.lines[0].elapsed, "15:00");
        assert_eq!(t.lines[0].eta, "~15m");
        // A writer that pauses rows has its pauses in `started`, not the
        // machine's sleeps: those still come off.
        let paused = feed(
            r#"{"rows":[{"key":"g#1","stage":"running","agent_id":"a1","eta_s":1800,
                "paused_s":0}]}"#,
        );
        let t = build_at(Some(&paused), &rows, now, &clock, &Logs::new());
        assert_eq!(t.lines[0].elapsed, "15:00");
        // Landed 20 minutes after waking, 30 minutes of work in all: on
        // time, not eight hours over.
        let done = feed(
            r#"{"rows":[{"key":"g#1","stage":"done","eta_s":1800,
                "started":1789999400000,"ended":1790030000000}]}"#,
        );
        let t = build_at(Some(&done), &[], now, &clock, &Logs::new());
        assert_eq!(t.lines[0].elapsed, "30:00");
        assert_eq!(t.lines[0].eta, "(±0m)");
    }

    #[test]
    fn a_limit_with_no_known_reset_holds_until_it_clears() {
        let (rows, f) = ten_minutes_in();
        let limit = Limit {
            reopens_ms: None,
            window: None,
        };
        let mut holds = Vec::new();
        track(&mut holds, Some(&limit), T0);
        let t = build_at(
            Some(&f),
            &rows,
            T0 + 90 * MIN,
            &utc(&holds, Some(&limit)),
            &Logs::new(),
        );
        assert_eq!(t.lines[0].elapsed, "10:00");
        assert_eq!(t.lines[0].now, "limit");
        track(&mut holds, None, T0 + 90 * MIN);
        assert_eq!(holds[0].until_ms, Some(T0 + 90 * MIN));
    }

    #[test]
    fn a_paused_feed_row_holds_its_clock() {
        // Paused at T0; `started` already moved on by the open
        // pause up to when the file was written, five minutes later.
        let written = T0 + 5 * MIN;
        let mut f = feed(&format!(
            r#"{{"rows":[{{"key":"g#1","stage":"running","agent_id":"a1","eta_s":1800,
                "started":{},"spawned":{},"paused_s":300,"paused_since":{T0}}}]}}"#,
            T0 - 5 * MIN,
            T0 - 10 * MIN,
        ));
        f.written_ms = Some(written);
        // The live worker's own start is the true spawn; the feed's wins.
        let (rows, _) = ten_minutes_in();
        for now in [written, written + 30 * MIN, written + 3 * 60 * MIN] {
            let t = build_at(Some(&f), &rows, now, &utc(&[], None), &Logs::new());
            assert_eq!(t.lines[0].elapsed, "10:00", "{now}");
            assert_eq!(t.lines[0].eta, "~20m");
            assert_eq!(t.lines[0].now, "paused since 14:13");
        }
        // The writer paused it for a limit: its `started` already
        // leaves the wait out, so the pane's own hold must not take it off
        // again; NOW names the limit.
        let limit = Limit {
            reopens_ms: Some(T0 + 60 * MIN),
            window: Some("5h"),
        };
        let holds = [Hold {
            since_ms: T0,
            until_ms: None,
            reopens_ms: limit.reopens_ms,
        }];
        let t = build_at(
            Some(&f),
            &rows,
            written + MIN,
            &utc(&holds, Some(&limit)),
            &Logs::new(),
        );
        assert_eq!(t.lines[0].elapsed, "10:00");
        assert_eq!(t.lines[0].now, "5h limit → 15:13");
        // A feed with no write time stops at `paused_since`.
        f.written_ms = None;
        let t = build_at(
            Some(&f),
            &rows,
            T0 + 60 * MIN,
            &utc(&[], None),
            &Logs::new(),
        );
        assert_eq!(t.lines[0].elapsed, "5:00");
    }

    /// A Running row re-estimated at `T0 + at` minutes: "`left` minutes
    /// left", ten minutes after its worker started at `T0 − 10m`. `eta_s`
    /// is what the CLI writes beside the figure: the wall time up to it
    /// plus the figure.
    fn re_estimated(at: u64, left: u64) -> Feed {
        feed(&format!(
            r#"{{"rows":[{{"key":"g#1","stage":"running","agent_id":"a1",
                "started":{s},"eta_s":{eta},"eta_at":{t},"left_s":{l},"left_at":{t}}}]}}"#,
            s = T0 - 10 * MIN,
            eta = (at + 10) * 60 + left * 60,
            t = T0 + at * MIN,
            l = left * 60,
        ))
    }

    #[test]
    fn a_re_estimate_counts_down_by_the_work_since_it() {
        // giverny#402: ten minutes of work, then a five-hour limit, then
        // "30m left" as the limit lifts. Ten minutes on, 20 are left: the
        // hold before the figure is in neither the figure nor the work.
        let (rows, _) = ten_minutes_in();
        let f = re_estimated(300, 30);
        let hold = |until| Hold {
            since_ms: T0,
            until_ms: until,
            reopens_ms: Some(T0 + 300 * MIN),
        };
        let held = [hold(Some(T0 + 300 * MIN))];
        let now = T0 + 310 * MIN;
        let t = build_at(Some(&f), &rows, now, &utc(&held, None), &Logs::new());
        assert_eq!(t.lines[0].elapsed, "20:00");
        assert_eq!(t.lines[0].eta, "~20m", "not ~5h20m");
        // A night asleep before the figure is the same.
        let night = [sleeps::Span {
            since: T0,
            until: T0 + 300 * MIN,
        }];
        let clock = Clock {
            sleeps: &night,
            ..utc(&[], None)
        };
        let t = build_at(Some(&f), &rows, now, &clock, &Logs::new());
        assert_eq!(t.lines[0].eta, "~20m");
        // A limit out after the figure still stops the countdown.
        let after = [
            hold(Some(T0 + 300 * MIN)),
            Hold {
                since_ms: T0 + 305 * MIN,
                until_ms: None,
                reopens_ms: None,
            },
        ];
        for later in [310, 400, 900] {
            let t = build_at(
                Some(&f),
                &rows,
                T0 + later * MIN,
                &utc(&after, None),
                &Logs::new(),
            );
            assert_eq!(t.lines[0].eta, "~25m", "{later}");
        }
        // A feed from before the figure was kept counts down as it did.
        let old = feed(&format!(
            r#"{{"rows":[{{"key":"g#1","stage":"running","agent_id":"a1",
                "started":{},"eta_s":{},"eta_at":{}}}]}}"#,
            T0 - 10 * MIN,
            340 * 60,
            T0 + 300 * MIN,
        ));
        let t = build_at(Some(&old), &rows, now, &utc(&held, None), &Logs::new());
        assert_eq!(t.lines[0].eta, "~5h20m");
    }

    #[test]
    fn a_done_row_is_measured_against_the_workers_first_re_estimate() {
        // giverny#402: guessed 20m, "30m left" ten minutes in, then asleep
        // five hours, "5m left" at the last; landed 25 minutes of work
        // after the first re-estimate: five minutes early against it, not
        // against the last figure nor the wall time in `eta_s`.
        let night = [sleeps::Span {
            since: T0,
            until: T0 + 300 * MIN,
        }];
        let clock = Clock {
            sleeps: &night,
            ..utc(&[], None)
        };
        let done = |reest: &str| {
            feed(&format!(
                r#"{{"rows":[{{"key":"g#1","stage":"done","agent_id":"a1",
                    "started":{},"ended":{},"eta_s":{},"eta_first_s":1200{reest}}}]}}"#,
                T0 - 10 * MIN,
                T0 + 325 * MIN,
                (330 + 5) * 60,
            ))
        };
        let f = done(r#","reest_s":1800,"reest_at_s":600"#);
        let t = build_at(Some(&f), &[], T0 + 400 * MIN, &clock, &Logs::new());
        assert_eq!(t.lines[0].elapsed, "35:00");
        assert_eq!(t.lines[0].eta, "(-5m)");
        let facts = &t.lines[0].click.facts;
        assert!(
            facts.iter().any(|x| x == "(-5m) vs worker's 30m"),
            "{facts:?}"
        );
        // Never re-estimated by its worker: against the dispatcher's 20m.
        let f = done("");
        let t = build_at(Some(&f), &[], T0 + 400 * MIN, &clock, &Logs::new());
        assert_eq!(t.lines[0].eta, "(+15m)");
        let facts = &t.lines[0].click.facts;
        assert!(
            facts.iter().any(|x| x == "(+15m) vs estimate 20m"),
            "{facts:?}"
        );
    }

    #[test]
    fn ines_night_row_reads_its_ninety_minutes() {
        // giverny#402, poly's `44-r4`: started 02:05:10Z, 38s of work, then
        // the account out of its limit until 07:26:36Z, when Ines said
        // "90m left". At 07:57:30Z the pane read ~6h20m; 59m is right.
        let started = 1_791_943_510_000;
        let given = 1_791_962_796_000;
        let now = 1_791_964_650_000;
        let rows = live(&format!(
            r#"{{"session_id":"s","tasks":[{{"id":"ae934e0c75035a881","status":"running",
                "startTime":{started},"label":"Editing"}}]}}"#
        ));
        let row = |figure: &str| {
            feed(&format!(
                r#"{{"rows":[{{"key":"44-r4","stage":"running","agent_id":"ae934e0c75035a881",
                    "started":"2026-10-14T02:05:10Z","eta_s":24686,
                    "eta_at":"2026-10-14T07:26:36Z"{figure}}}]}}"#
            ))
        };
        let held = [Hold {
            since_ms: started + 38_000,
            until_ms: Some(given),
            reopens_ms: Some(given),
        }];
        let f = row(r#","left_s":5400,"left_at":"2026-10-14T07:26:36Z""#);
        let t = build_at(Some(&f), &rows, now, &utc(&held, None), &Logs::new());
        assert_eq!(t.lines[0].eta, "~59m");
        // Her feed as it is, from before the figure was kept: the bug.
        let t = build_at(Some(&row("")), &rows, now, &utc(&held, None), &Logs::new());
        assert_eq!(t.lines[0].eta, "~6h20m");
    }

    #[test]
    fn planned_etas_hold_through_a_limit() {
        let f = feed(r#"{"rows":[{"key":"g#2","stage":"planned","eta_s":3780}]}"#);
        let limit = Limit {
            reopens_ms: Some(T0 + 60 * MIN),
            window: Some("7d"),
        };
        let mut holds = Vec::new();
        track(&mut holds, Some(&limit), T0);
        for now in [T0, T0 + 45 * MIN] {
            let t = build_at(Some(&f), &[], now, &utc(&holds, Some(&limit)), &Logs::new());
            assert_eq!(t.lines[0].eta, "~1h3m");
            assert_eq!(t.lines[0].elapsed, "");
            assert_eq!(t.lines[0].now, "");
        }
    }

    #[test]
    fn the_spent_window_is_the_one_that_reopens_last() {
        let at = |ms: u64| Some(jiff::Timestamp::from_millisecond(ms as i64).unwrap());
        // Nothing at 99%.
        assert_eq!(spent(&[("5h", 99.0, at(T0 + MIN))], T0), None);
        // A reset already past is a window that is over.
        assert_eq!(spent(&[("5h", 100.0, at(T0 - MIN))], T0), None);
        // No reset known: not something to hold on.
        assert_eq!(spent(&[("5h", 100.0, None)], T0), None);
        let both = [
            ("5h", 100.0, at(T0 + MIN)),
            ("7d", 100.0, at(T0 + 90 * MIN)),
        ];
        assert_eq!(
            spent(&both, T0),
            Some(Limit {
                reopens_ms: Some(T0 + 90 * MIN),
                window: Some("7d"),
            })
        );
    }

    #[test]
    fn a_reset_on_another_day_names_the_day() {
        let tz = jiff::tz::TimeZone::UTC;
        let l = |at| Limit {
            reopens_ms: Some(at),
            window: Some("7d"),
        };
        assert_eq!(limit_note(&l(T0 + 60 * MIN), T0, &tz), "7d limit → 15:13");
        assert_eq!(
            limit_note(&l(T0 + 2 * 24 * 60 * MIN), T0, &tz),
            "7d limit → Wed 14:13"
        );
    }

    #[test]
    fn held_spans_are_clipped_to_the_rows_own_run() {
        let holds = [
            Hold {
                since_ms: 0,
                until_ms: Some(10),
                reopens_ms: None,
            },
            Hold {
                since_ms: 20,
                until_ms: None,
                reopens_ms: None,
            },
        ];
        assert_eq!(off_ms(&holds, &[], 5, 30), 5 + 10);
        assert_eq!(off_ms(&holds, &[], 12, 18), 0);
        // A sleep comes off too, and inside a hold only once.
        let slept = [
            sleeps::Span {
                since: 12,
                until: 16,
            },
            sleeps::Span {
                since: 25,
                until: 40,
            },
        ];
        assert_eq!(off_ms(&holds, &slept, 5, 30), 5 + 4 + 10);
    }

    // --------------------------------------------------- selectable text ----

    fn rows_text() -> Vec<String> {
        vec![
            "Running  demo#84 FEATURE: selectable      1:02".into(),
            "NextUp   demo#85 BUG: something".into(),
            "session 3".into(),
        ]
    }

    #[test]
    fn a_selection_copies_its_rows_line_by_line() {
        let rows = rows_text();
        // Backwards drag, from the middle of row 1 to the id on row 0.
        let sel = Selection {
            anchor: (1, 12),
            head: (0, 9),
        };
        assert_eq!(
            selected_text(&rows, &sel),
            "demo#84 FEATURE: selectable      1:02\nNextUp   dem"
        );
        // Past the row's end is the row's end, and blanks are not copied.
        let sel = Selection {
            anchor: (0, 0),
            head: (2, 99),
        };
        assert_eq!(selected_text(&rows, &sel), rows.join("\n"));
        let one = Selection {
            anchor: (0, 9),
            head: (0, 16),
        };
        assert_eq!(selected_text(&rows, &one), "demo#84");
        assert_eq!(one.span(1, 30), None);
    }

    #[test]
    fn a_row_reads_as_it_is_drawn() {
        let segs = vec![
            (0, "Done".to_string()),
            (9, "g#1 title".to_string()),
            (26, "5:00".to_string()),
            (40, String::new()),
        ];
        assert_eq!(compose(&segs), "Done     g#1 title        5:00");
    }

    /// Three 20pt rows of 8pt cells from the origin, driven by a pointer
    /// in a headless egui: one frame per event list.
    struct Rows {
        ctx: egui::Context,
        sel: Option<Selection>,
        time: f64,
    }

    impl Rows {
        fn new() -> Self {
            Rows {
                ctx: egui::Context::default(),
                sel: None,
                time: 0.0,
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> (RowsInput, Vec<String>) {
            self.frame_mods(events, egui::Modifiers::NONE)
        }

        fn frame_mods(
            &mut self,
            events: Vec<egui::Event>,
            modifiers: egui::Modifiers,
        ) -> (RowsInput, Vec<String>) {
            self.time += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                time: Some(self.time),
                modifiers,
                events,
                ..Default::default()
            };
            let texts = rows_text();
            let mut got = RowsInput::default();
            let sel = &mut self.sel;
            let out = self.ctx.run_ui(input, |ui| {
                let rects: Vec<egui::Rect> = (0..3)
                    .map(|i| {
                        egui::Rect::from_min_size(
                            egui::pos2(0.0, 20.0 * i as f32),
                            egui::vec2(400.0, 20.0),
                        )
                    })
                    .collect();
                if let Some((o, _)) = rows_input(ui, egui::Id::new("t"), &rects, &texts, 8.0, sel) {
                    got = o;
                }
            });
            let copied = out
                .platform_output
                .commands
                .into_iter()
                .filter_map(|c| match c {
                    egui::OutputCommand::CopyText(t) => Some(t),
                    _ => None,
                })
                .collect();
            (got, copied)
        }

        fn press(&mut self, at: egui::Pos2) -> (RowsInput, Vec<String>) {
            self.frame(vec![egui::Event::PointerMoved(at)]);
            self.frame(vec![button(at, true)])
        }
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn a_plain_click_opens_the_row_and_selects_nothing() {
        let mut r = Rows::new();
        let at = egui::pos2(100.0, 30.0);
        r.press(at);
        let (input, copied) = r.frame(vec![button(at, false)]);
        assert_eq!(input.clicked, Some(1));
        assert_eq!(input.click_col, Some(12), "the cell it fell in");
        assert_eq!(r.sel, None);
        assert!(copied.is_empty());
    }

    #[test]
    fn a_drag_selects_and_copies_and_is_never_a_click() {
        let mut r = Rows::new();
        // From cell 9 of row 0 to cell 12 of row 1.
        let (from, to) = (egui::pos2(72.0, 10.0), egui::pos2(96.0, 30.0));
        r.press(from);
        let mut seen = Vec::new();
        for k in 1..=4 {
            let p = from + (to - from) * (k as f32 / 4.0);
            let (input, copied) = r.frame(vec![egui::Event::PointerMoved(p)]);
            assert_eq!(input.clicked, None);
            assert!(copied.is_empty(), "copied before the drag ended");
            seen.push(input.dragging);
        }
        assert!(seen.iter().any(|d| *d), "never dragging: {seen:?}");
        assert_eq!(
            r.sel,
            Some(Selection {
                anchor: (0, 9),
                head: (1, 12),
            })
        );
        let (input, copied) = r.frame(vec![button(to, false)]);
        assert_eq!(input.clicked, None, "a drag is not a click");
        let want = "demo#84 FEATURE: selectable      1:02\nNextUp   dem";
        assert_eq!(copied, [want]);
        assert_eq!(input.copied.as_deref(), Some(want));
        // The selection stays lit until the next press, which lets it go.
        let (_, copied) = r.frame(vec![]);
        assert!(copied.is_empty());
        assert!(r.sel.is_some());

        // Ctrl+Shift+C copies it again, and the terminal never sees it.
        let ctrl_shift = egui::Modifiers {
            ctrl: true,
            shift: true,
            command: true,
            ..Default::default()
        };
        let (_, copied) = r.frame_mods(vec![egui::Event::Copy], ctrl_shift);
        assert_eq!(copied, [want]);
        // Plain Ctrl+C is the terminal's interrupt: left alone.
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        let (_, copied) = r.frame_mods(vec![egui::Event::Copy], ctrl);
        assert!(copied.is_empty());

        r.press(egui::pos2(10.0, 50.0));
        assert_eq!(r.sel, None);
    }

    #[test]
    fn cut_keeps_room_for_the_ellipsis() {
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abc", 4), "abc");
    }
}

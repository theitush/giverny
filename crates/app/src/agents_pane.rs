//! The agents pane: a table under a tab's terminal listing that tab's Claude
//! Code subagents — Running, Planned and Done — one row each.
//!
//! Off by default (`claude.agents_pane`); off, nothing here runs. On, it is
//! drawn only in a tab whose session has rows, as a resizable bottom panel
//! inside the terminal's area.
//!
//! The rows are a [`Tracker`] (Claude Code's own data: live list,
//! transcripts, notifications) merged with the optional feed file
//! ([`feed::merge`], `docs/agents-pane.md`). This module owns only the look:
//! the columns STAGE, TASK, ELAPSED, ETA, NOW and TOKENS, every one but TASK a
//! fixed width, no header row, the whole row tinted by its stage, a
//! stopwatch ELAPSED ticking every second, `~1h3m` ETAs, a Done row's
//! `(+5m)`/`(-1h20m)`, `"` for a cell that repeats the same worker's row
//! above it — a compact status table pinned under
//! Claude Code. The session's token total is not here: it is on the status
//! line's model row.
//!
//! **The rows are not kept here.** [`show`] is handed the tab's tracker —
//! `ClaudeWatch::agents` (`agents_live.rs`), fed by the relay, persisted,
//! emptied on `/clear` and refreshed once a second — and keeps only what the
//! pane itself needs per tab: the feed it last read.
//!
//! **Clocks stop while nothing can run**: while the tab's
//! account is out of a usage limit ([`Limit`]), a Running row's ELAPSED
//! stops and its ETA holds instead of counting down past zero, and NOW says
//! `5h limit → 13:00`. The pane remembers each such span ([`Hold`]) and
//! both clocks carry on from where they stopped once the limit resets. A
//! feed row the orchestrator has paused (`paused_since`) is held
//! the same way, and reads `paused since 12:58`. Planned ETAs are durations
//! and hold by themselves; a Done row is measured history and never moves.
//!
//! **Clicks** produce a [`RowClick`], which the app receives as
//! `Action::AgentRowClicked`. What a click *does* is not decided here, and
//! it leaves no mark: a row is tinted only while the pointer is on it
//! ([`row_tint`]), so nothing stays highlighted after a click.
//!
//! **Resources**: what a row's `giverny orchestrator-session run`
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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Color32, CursorIcon, Sense, Ui};
use giverny_claude::agent_eta::{self, Etas};
use giverny_claude::feed::{self, Feed, FeedCache, LeaseState, PaneRow, RowLease, RowUsage, Stage};
use giverny_claude::resources::{self, Ledger};
use giverny_claude::run_live::{RunLive, TaskLive};
use giverny_claude::session_use;
use giverny_claude::subagents::{Outcome, SubagentRow, Tracker};
use giverny_claude::worker_log::WorkerLog;
use giverny_core::config::{AgentsPanelConfig, DoneRows, PaneColumns};
use giverny_core::limits::{Limits, Machine, Mem, Resolved};
use giverny_core::tabs::TabId;
use giverny_term::widget::RenderShared;

use crate::chrome::Chrome;
use crate::claude_watch::ClaudeWatch;

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
    /// orchestrator session's task.
    etas: agent_eta::Cache,
    etas_now: Etas,
    feed_session: Option<String>,
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
}

impl View {
    /// Re-read the feed for `session`, at most once a [`POLL`] (or at once
    /// when the session changed). A session Claude Code re-id'd is looked
    /// up by its earlier ids too, newest first, until its files name the
    /// new one (giverny#105).
    fn poll_feed(&mut self, session: Option<&str>, aliases: &[String]) {
        let changed = self.feed_session.as_deref() != session;
        if !changed && self.last_poll.is_some_and(|t| t.elapsed() < POLL) {
            return;
        }
        self.last_poll = Some(Instant::now());
        self.feed_session = session.map(str::to_string);
        let ids: Vec<&str> = session
            .into_iter()
            .chain(aliases.iter().rev().map(String::as_str))
            .collect();
        let dir = feed::feed_dir();
        self.feed_now = session.and_then(|_| self.feed.poll_any(&dir, &ids).cloned());
        self.etas_now = session
            .map(|_| self.etas.poll_any(&dir, &ids).clone())
            .unwrap_or_default();
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
#[derive(Default)]
pub struct Views {
    tabs: HashMap<TabId, View>,
    /// The machine ledger, read off the UI thread: one for every tab, as
    /// there is one ledger for the whole machine.
    ledger: LedgerWatch,
}

impl Views {
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

/// Milliseconds of `[start, upto]` that fall inside a hold — what comes off
/// a row's clock.
pub fn held_ms(holds: &[Hold], start: u64, upto: u64) -> u64 {
    holds
        .iter()
        .map(|h| {
            let a = h.since_ms.max(start);
            let b = h.until_ms.unwrap_or(upto).min(upto);
            b.saturating_sub(a)
        })
        .sum()
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
    pub limit: Option<&'a Limit>,
    pub tz: jiff::tz::TimeZone,
    /// Each worker's processes' use now, by agent id: what a Running row
    /// with no feed figure of its own shows.
    pub workers: &'a HashMap<String, RunLive>,
    /// Each worker's agent ETA, by agent id: what a row with no feed
    /// estimate counts down from.
    pub etas: &'a Etas,
}

/// No worker measured.
#[cfg(any(test, debug_assertions))]
static NO_WORKERS: std::sync::LazyLock<HashMap<String, RunLive>> =
    std::sync::LazyLock::new(HashMap::new);

/// No agent ETAs.
#[cfg(any(test, debug_assertions))]
static NO_ETAS: std::sync::LazyLock<Etas> = std::sync::LazyLock::new(Etas::new);

#[cfg(any(test, debug_assertions))]
impl Clock<'_> {
    pub(crate) fn plain() -> Clock<'static> {
        Clock {
            holds: &[],
            limit: None,
            tz: jiff::tz::TimeZone::system(),
            workers: &NO_WORKERS,
            etas: &NO_ETAS,
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
    /// `[orchestrator.limits]` resolved for this machine; `None` when the
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
                .name("agents-pane-ledger".into())
                .spawn(move || read_ledger_loop(&s, &ctx))
            {
                tracing::warn!("agents pane: the ledger reader did not start: {err}");
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
/// whether or not they ran under `giverny orchestrator-session run`. A run of the row's
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
/// holds nothing. A queued row's place is its place now, and it waits
/// behind the task its copy named while that one still holds or waits
/// ahead, else behind whoever holds a slot it asks for, else the request
/// queued before it.
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
                cpu: l.cpu,
                ram_mb: l.ram_mb,
                gpus: l.gpus.clone(),
                vram_mb: l.vram_mb,
                slots: l.slots.clone(),
                granted_ms: Some(l.granted_at),
                wanted_ram_mb: smaller.and_then(|c| c.wanted_ram_mb),
                position: None,
                behind: None,
            });
        } else if let Some(i) = queue
            .iter()
            .position(|w| w.task == row.key && mine(&w.session))
        {
            let w = queue[i];
            let ahead = &queue[..i];
            let still = |t: &str| {
                ledger.leases.iter().any(|l| l.task == t) || ahead.iter().any(|a| a.task == t)
            };
            let behind = copy
                .as_ref()
                .and_then(|c| c.behind.clone())
                .filter(|b| still(b))
                .or_else(|| {
                    ledger
                        .leases
                        .iter()
                        .find(|l| l.slots.iter().any(|s| w.request.slots.contains(s)))
                        .map(|l| l.task.clone())
                })
                .or_else(|| ahead.last().map(|a| a.task.clone()));
            row.lease = Some(RowLease {
                state: LeaseState::Queued,
                id: None,
                cpu: w.request.cpu,
                ram_mb: w.request.ram_mb,
                gpus: Vec::new(),
                vram_mb: w.request.vram_mb,
                slots: w.request.slots.clone(),
                granted_ms: None,
                wanted_ram_mb: None,
                position: Some(i as u64 + 1),
                behind,
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

/// A row's use in its cell, read like the status line's use part right
/// above the pane and built from the same pieces, so the figures sit in
/// the same columns: a Running row's commands now, ` 14% CPU  ·  4.2G` (CPU
/// as a share of the whole machine, so at most `100%`), a Done row's
/// memory peak alone in the same place, `4.2G`; on a machine with a GPU
/// (`gpu`), a Running row's GPU memory after, `  ·  gpu 1.2G`. `OOM`
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
    }
    s
}

/// A Next up row waiting in the ledger, in its NOW cell: `queued for 3G
/// behind demo#12` — what it waits for (its RAM, else VRAM, cores, a
/// slot) and on whom.
pub fn queued_note(l: &RowLease) -> String {
    let need = if l.ram_mb > 0 {
        mem(l.ram_mb)
    } else if l.vram_mb > 0 {
        format!("gpu {}", mem(l.vram_mb))
    } else if l.cpu > 0 {
        format!("{}c", l.cpu)
    } else if !l.slots.is_empty() {
        "a slot".into()
    } else {
        String::new()
    };
    let mut s = "queued".to_string();
    if !need.is_empty() {
        s.push_str(&format!(" for {need}"));
    }
    if let Some(b) = &l.behind {
        s.push_str(&format!(" behind {b}"));
    }
    s
}

/// The lease in full for the row's overlay header: `holds 3 cpu, 3G, slot
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
                resources::describe_held(l.cpu, l.ram_mb, &l.gpus, l.vram_mb, &l.slots)
            );
            if let Some(w) = l.wanted_ram_mb.filter(|_| l.state == LeaseState::Smaller) {
                s.push_str(&format!(" (asked {})", mem(w)));
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

/// What a row click does: a click on the row the tab is
/// already showing takes it back to the orchestrator.
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
    /// A `giverny orchestrator-session run` of the row was killed by its memory cap: the
    /// lease cell is drawn in the warning colour.
    pub oom: bool,
    /// A Running row with nothing running it ([`unworked`]): its worker has
    /// finished, or it has none. NOW says which, in the warning colour.
    pub flag: bool,
    pub click: RowClick,
}

/// The whole table: rows and the feed's footer line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Table {
    pub lines: Vec<Line>,
    pub footer: Option<String>,
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
    build_with(feed, live, now_ms, clock, logs, DoneRows::All)
}

/// [`build_at`], showing only the Done rows `done` keeps
/// (`agents_panel.done_rows`). Rows are dropped before they are formatted,
/// so a `"` never stands for a row that is not drawn.
pub fn build_with(
    feed: Option<&Feed>,
    live: &[SubagentRow],
    now_ms: u64,
    clock: &Clock,
    logs: &Logs,
    done: DoneRows,
) -> Table {
    let log = |id: &str| logs.get(id);
    let handed = feed::with_handoffs(feed, live, log);
    let feed = handed.as_ref().or(feed);
    let mut rows = feed::merge_with(feed, live, log);
    keep_done(&mut rows, done, |r| r.stage);
    let written = feed.and_then(|f| f.written_ms);
    let mut lines: Vec<Line> = Vec::with_capacity(rows.len());
    for row in &rows {
        lines.push(format_row(row, now_ms, written, clock));
    }
    dittos(&rows, &mut lines);
    Table {
        lines,
        footer: feed.and_then(|f| f.footer.clone()),
    }
}

/// Drop the Done rows `done` does not keep. Done rows sit newest first, so
/// `Last(n)` keeps the first `n` of them met.
fn keep_done<R>(rows: &mut Vec<R>, done: DoneRows, stage: impl Fn(&R) -> Stage) {
    let keep = match done {
        DoneRows::All => return,
        DoneRows::Hide => 0,
        DoneRows::Last(n) => n,
    };
    let mut seen = 0;
    rows.retain(|r| {
        if stage(r) != Stage::Done {
            return true;
        }
        seen += 1;
        seen <= keep
    });
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
    // A feed row that carries its pauses has its stops taken off `started`
    // already (a writer may also pause rows for a limit itself): the
    // pane's own holds would take them off twice.
    let writer_pauses = f.is_some_and(|f| f.paused_s.is_some() || f.paused_since_ms.is_some());
    let holds = if writer_pauses { &[][..] } else { clock.holds };
    // A Running row's work so far, in seconds: wall time up to where its
    // clock stands, less every span the account was out of its limit.
    // A worker stopped on an API error, or ended badly under a Running
    // row, stands still from then, and its error spans are off its clock.
    let stopped = l
        .filter(|_| row.stage == Stage::Running)
        .and_then(|l| l.stopped());
    let error_ms = |s: u64, e: u64| l.map_or(0, |l| l.stopped_ms(s, e));
    let work_s = match (row.stage, row_start) {
        (Stage::Running, Some(s)) => {
            let mut stop = clock_stop(paused, written, now_ms);
            if let Some((since, _)) = &stopped {
                stop = stop.min((*since).max(s));
            }
            Some(
                stop.saturating_sub(s)
                    .saturating_sub(held_ms(holds, s, stop))
                    .saturating_sub(error_ms(s, stop))
                    / 1000,
            )
        }
        _ => None,
    };
    let elapsed = match row.stage {
        Stage::Planned => String::new(),
        Stage::Running => work_s.map(stopwatch).unwrap_or_default(),
        Stage::Done => {
            let end = row.ended_ms();
            match (row_start, end) {
                (Some(s), Some(e)) => {
                    stopwatch(e.saturating_sub(s).saturating_sub(error_ms(s, e)) / 1000)
                }
                _ => String::new(),
            }
        }
    };
    // A worker no feed row carries: its agent ETA, counted from its start.
    let agent_eta = l
        .filter(|_| f.is_none())
        .and_then(|l| clock.etas.get(&l.id))
        .zip(row_start)
        .map(|(e, s)| e.total_s(s));
    let eta_s = f.and_then(|f| f.eta_s).or(agent_eta);
    let eta = match row.stage {
        Stage::Running => match (eta_s, work_s) {
            (Some(eta), Some(work)) => countdown(eta as i64 - work as i64),
            _ => String::new(),
        },
        Stage::Planned => eta_s
            .map(|e| format!("~{}", feed::fmt_span(e as i64)))
            .unwrap_or_default(),
        Stage::Done => row
            .eta_delta_s()
            .or_else(|| {
                let took_ms = row.ended_ms()?.checked_sub(row_start?)?;
                Some((took_ms / 1000) as i64 - agent_eta? as i64)
            })
            .map(feed::fmt_delta)
            .unwrap_or_default(),
    };
    let no_eta = row.stage == Stage::Running && eta_s.is_none();
    // What the task holds in the machine ledger is in the overlay header; a
    // Next up row that waits for it says so in NOW.
    let held = f
        .and_then(|f| f.lease.as_ref())
        .filter(|_| row.stage != Stage::Done);
    let queued = held.filter(|l| row.stage == Stage::Planned && l.state == LeaseState::Queued);
    // The cell is the row's use: a Running row's commands now,
    // a Done row's memory peak alone.
    let measured = f.and_then(|f| f.usage.as_ref());
    let oom = measured.is_some_and(|u| u.oom_kills > 0) && row.stage != Stage::Planned;
    // Running and Done rows always have a cell, zero when nothing is going
    // or was measured; Next up rows say what they wait for in NOW instead.
    let peak = measured.and_then(|u| u.peak_mb);
    let gpu = session_use::gpu::present();
    let usage = match row.stage {
        Stage::Running => usage_cell(
            Some(
                f.and_then(|f| f.live)
                    .or_else(|| row.agent_id().and_then(|id| clock.workers.get(id)).copied())
                    .unwrap_or_default(),
            ),
            None,
            oom,
            gpu,
        ),
        Stage::Done => usage_cell(None, peak, oom, gpu),
        Stage::Planned => String::new(),
    };
    let limit = clock.limit.filter(|l| l.out_at(now_ms));
    let flag = (row.stage == Stage::Running)
        .then(|| unworked(row, now_ms))
        .flatten();
    let now = match row.stage {
        // What is not running comes first: a Running row whose worker has
        // finished, or that has none, says so before anything else.
        Stage::Running if flag.is_some() => flag.clone().unwrap_or_default(),
        // The limit next: a row the writer paused for it says why.
        Stage::Running => match (limit, paused) {
            (Some(limit), _) => limit_note(limit, now_ms, &clock.tz),
            (None, Some(p)) => format!("paused since {}", clock_at(p, now_ms, &clock.tz)),
            (None, None) if stopped.is_some() => stopped.map(|(_, why)| why).unwrap_or_default(),
            (None, None) => l
                .filter(|l| l.running())
                .and_then(|l| l.activity.clone())
                .unwrap_or_default(),
        },
        // Waiting in the machine ledger, else queued on a
        // worker busy with another task.
        Stage::Planned => queued
            .map(queued_note)
            .or_else(|| row.after_key.as_ref().map(|k| format!("after {k}")))
            .unwrap_or_default(),
        Stage::Done => f
            .and_then(|f| f.landing.clone())
            .or_else(|| row.next_key.as_ref().map(|k| format!("→ {k}")))
            .or_else(|| l.and_then(|l| l.outcome).map(outcome_word))
            .unwrap_or_default(),
    };
    let tokens = row.tokens().map(fmt_tokens).unwrap_or_default();
    let mut facts = row_facts(row.stage, &elapsed, &eta, &now, &tokens);
    if no_eta {
        facts.insert(
            1.min(facts.len()),
            no_eta_hint(&key, l.map(|l| l.agent_id())),
        );
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
    Line {
        stage: row.stage,
        id,
        title,
        elapsed,
        eta,
        no_eta,
        now,
        tokens,
        usage,
        oom,
        flag: flag.is_some(),
        click: RowClick {
            stage: row.stage,
            key,
            agent_id: row.agent_id().map(str::to_string),
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

/// What a Running row's NOW says when nothing is running it (giverny#258):
/// the feed says the task runs, but its worker has finished — handed back,
/// failed, killed or stopped (on an API error, that error) — and nobody
/// has landed it; or it has no
/// worker at all, [`feed::HANDOFF_WINDOW_MS`] after it started (a
/// dispatcher runs `start` a moment before the spawn), and no command of
/// its own runs either. `None` while a worker works on it.
fn unworked(row: &PaneRow<'_, SubagentRow>, now_ms: u64) -> Option<String> {
    if let Some(l) = row.live {
        if l.running() {
            return None;
        }
        // Ended on an API error it never wrote past: that error is why.
        if let Some(s) = l.stops.last().filter(|s| s.to_ms.is_none()) {
            return Some(format!("stopped: {}", s.reason));
        }
        let how = match l.outcome {
            Some(Outcome::Failed) => "failed",
            Some(Outcome::Killed) => "killed",
            Some(Outcome::Stopped) => "stopped",
            _ => "finished",
        };
        return Some(format!("worker {how} — {NOT_LANDED}"));
    }
    let f = row.feed?;
    let started = f.started_ms?;
    let fresh = now_ms < started.saturating_add(feed::HANDOFF_WINDOW_MS);
    (!fresh && f.live.is_none()).then(|| NO_WORKER.to_string())
}

/// The end of [`unworked`]'s line for a finished worker.
const NOT_LANDED: &str = "not landed";
/// [`unworked`]'s line for a Running row nothing works on.
pub const NO_WORKER: &str = "no worker running";

/// The overlay header's facts, from the row's own cells before any ditto.
fn row_facts(stage: Stage, elapsed: &str, eta: &str, now: &str, tokens: &str) -> Vec<String> {
    let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
    let v = match stage {
        Stage::Done => [
            some(now),
            some(elapsed).map(|e| format!("took {e}")),
            some(eta).map(|d| format!("{d} vs estimate")),
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

/// How a Running row with no estimate gets one, as the overlay header says
/// it: an orchestrator session's task is re-estimated on its row; any other
/// worker is given an agent ETA by its id.
fn no_eta_hint(key: &str, agent_id: Option<&str>) -> String {
    if !key.is_empty() {
        return format!(
            "no ETA — add one: giverny-orchestrator-session eta {key} <min> --why scope"
        );
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
/// whose view the tab's Claude Code shows (its id): its row is lit as the
/// selection. `header` names the worker the terminal's
/// header is about, whose row comes back with the click, as
/// the pane drew it.
#[allow(clippy::too_many_arguments)]
pub fn show(
    views: &mut Views,
    tab: TabId,
    reading: Option<&giverny_claude::use_reading::Reading>,
    tracker: Option<&Tracker>,
    viewed: Option<&str>,
    header: Option<&str>,
    limit: Option<Limit>,
    panel: &AgentsPanelConfig,
    chrome: &Chrome,
    shared: &mut RenderShared,
    ui: &mut Ui,
) -> (Option<RowClick>, Option<Line>) {
    let Some(tracker) = tracker else {
        return (None, None);
    };
    let view = views.tabs.entry(tab).or_default();
    view.poll_feed(tracker.session_id.as_deref(), &tracker.aliases);
    let now = now_ms();
    track(&mut view.holds, limit.as_ref(), now);
    // What the rows' processes use: the reading the tab's status line
    // shows, which the sidebar shows the sum of (`sessions_load::for_tab`).
    let runs = reading
        .map(crate::sessions_load::task_lives)
        .unwrap_or_default();
    let workers = reading
        .map(crate::sessions_load::workers)
        .unwrap_or_default();
    let clock = Clock {
        holds: &view.holds,
        limit: limit.as_ref(),
        tz: jiff::tz::TimeZone::system(),
        workers: &workers,
        etas: &view.etas_now,
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
    let table = build_with(feed, tracker.rows(), now, &clock, &view.logs, panel.done());
    if table.is_empty() {
        return (None, None);
    }
    // The first of a worker's rows: the one with no ditto in it.
    let header_line = header.and_then(|id| {
        table
            .lines
            .iter()
            .find(|l| l.click.agent_id.as_deref() == Some(id))
            .cloned()
    });
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
    let id = egui::Id::new(("agents_pane", tab));
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
                    let (click, rects, hover) = draw_table(
                        ui,
                        id.with("rows"),
                        &table,
                        &panel.columns,
                        viewed,
                        chrome,
                        shared,
                        cell,
                        row_h,
                        cols,
                        &mut view.sel,
                    );
                    clicked = click;
                    view.hover = hover;
                    #[cfg(debug_assertions)]
                    {
                        view.clicks = table.lines.iter().map(|l| l.click.clone()).collect();
                    }
                    view.rows_at = rects;
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
/// last column, the use column, ends exactly under the status line's use
/// part and the figures line up. The bar's lane is always left free, so it
/// never paints over a cell and the columns never move when it appears; on
/// the usual sizes it fits in the two cells right of the table anyway.
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
/// (`[agents_panel.columns]`) takes no width: the rest close up, each a
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
    /// the table's last column, so it sits right under the status line's
    /// use part ([`table_cols`]).
    usew: usize,
    x_use: usize,
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
        let taskw = if on.title {
            cols.saturating_sub(stage_cols + fixed_right).max(MIN_TITLE)
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
        }
    }

    /// A row's cells as drawn: each one's text and the column it starts at.
    /// Always the same seven, in this order ([`ETA_SEG`], [`LEASE_SEG`]); a
    /// column switched off is an empty cell.
    fn segments(&self, line: &Line) -> Vec<(usize, String)> {
        let on = &self.on;
        let idw = self.idw;
        // The id is never cut; the title takes what is left.
        let task = if !on.title {
            line.id.clone()
        } else if idw > 0 {
            let title_w = self.taskw.saturating_sub(idw + 1);
            format!("{:<idw$} {}", line.id, cut(&line.title, title_w))
        } else {
            cut(&line.title, self.taskw)
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
        out.clicked = at(resp.interact_pointer_pos()).map(|(r, _)| r);
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
    viewed: Option<&str>,
    chrome: &Chrome,
    shared: &mut RenderShared,
    cell: egui::Vec2,
    row_h: f32,
    cols: usize,
    sel: &mut Option<Selection>,
) -> (Option<RowClick>, Vec<egui::Rect>, Option<Option<RowClick>>) {
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
    let Some((input, _)) = rows_input(ui, id, &rects, &texts, cw, sel) else {
        return (None, rects, None);
    };
    let n = table.lines.len();
    let hovered_line = input.hovered.filter(|&r| r < n);
    if hovered_line.is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let sel_fill = ui.visuals().selection.bg_fill;
    let p = ui.painter().clone();

    for (i, rect) in rects.iter().copied().enumerate() {
        let line = table.lines.get(i);
        let color = line.map_or(chrome.dim, |l| stage_color(l.stage));
        if let Some(line) = line {
            // The worker whose view the terminal above shows:
            // held lit, with a bar at its left edge, as a selection.
            let on_view = viewed.is_some() && line.click.agent_id.as_deref() == viewed;
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
            shared.paint_text(&p, at, s, ink);
        }
    }
    let clicked = input
        .clicked
        .and_then(|i| table.lines.get(i))
        .map(|l| l.click.clone());
    let hover = input
        .hovered
        .map(|i| table.lines.get(i).map(|l| l.click.clone()));
    (clicked, rects, hover)
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

    #[test]
    fn a_running_row_with_no_estimate_says_so() {
        // A worker spawned outside an orchestrator session: a row, but no ETA.
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
        assert!(t.lines[0].click.facts.contains(
            &"no ETA — add one: giverny-orchestrator-session eta g#3 <min> --why scope".into()
        ));
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
    const DONE_FOUR: &str = r#"{"rows":[
      {"key":"g#1","stage":"running","title":"one","started":1789999400000,"eta_s":1800},
      {"key":"g#2","stage":"planned","title":"two","eta_s":600},
      {"key":"g#3","stage":"done","title":"three","started":1789990000000,"ended":1789993900000},
      {"key":"g#4","stage":"done","title":"four","started":1789990000000,"ended":1789994900000},
      {"key":"g#5","stage":"done","title":"five","started":1789990000000,"ended":1789995900000},
      {"key":"g#6","stage":"done","title":"six","started":1789990000000,"ended":1789996900000}]}"#;

    #[test]
    fn done_rows_are_all_none_or_the_newest_few() {
        let f = feed(DONE_FOUR);
        let keys = |done: DoneRows| -> Vec<String> {
            build_with(Some(&f), &[], T0, &Clock::plain(), &Logs::new(), done)
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
        let mut panel = AgentsPanelConfig::default();
        assert_eq!(panel.done(), DoneRows::All);
        panel.done_rows = "last".into();
        panel.done_last = 1;
        assert_eq!(keys(panel.done()), ["g#1", "g#2", "g#6"]);
        panel.done_rows = "hide".into();
        assert_eq!(keys(panel.done()), ["g#1", "g#2"]);
    }

    #[test]
    fn the_settings_list_the_columns_in_the_panes_order() {
        // Settings → Agents panel shows a switch per column in `SETTINGS`'
        // order; that order must be the one the pane draws them in.
        let t = build(Some(&feed(DONE_FOUR)), &[], T0);
        let c = Cols::new(&t, 100);
        let at: Vec<(&str, usize)> = giverny_core::settings::SETTINGS
            .iter()
            .filter_map(|d| d.key.strip_prefix("agents_panel.columns."))
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

        // A feed row's estimate wins, and so does its key.
        let f = feed(r#"{"rows":[{"key":"g#3","stage":"running","agent_id":"a1","eta_s":1800}]}"#);
        let t = build_at(Some(&f), &rows[..1], T0, &clock, &Logs::new());
        assert_eq!(t.lines[0].id, "g#3");
        assert_eq!(t.lines[0].eta, countdown(1800 - 300));
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
        let dir =
            std::env::temp_dir().join(format!("giverny-pane-141-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-w.jsonl");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let status = if running { "running" } else { "completed" };
        let mut rows = live(&format!(
            r#"{{"session_id":"s","tasks":[{{"id":"w","status":"{status}",
                "description":"acme#613 market SD graph","startTime":{},
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
    /// landed late with no span, acme#614 started eight minutes after the
    /// message, and both rows carried the worker's whole count.
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
        // Running: timed from the message, counted from it too.
        assert_eq!(run.stage, Stage::Running);
        assert_eq!(run.elapsed, "30:00");
        assert_eq!(run.eta, "~30m");
        assert_eq!(
            run.tokens,
            fmt_tokens(8_000 + 3_000),
            "not the worker's 156.3k"
        );
        // Queued behind it on the same worker.
        assert_eq!(next.stage, Stage::Planned);
        assert_eq!(next.now, "after acme#614");
        assert_eq!(next.tokens, "", "nothing spent on a task before it starts");
        // Done: from the spawn to the hand-off, and only what it added.
        assert_eq!(done.stage, Stage::Done);
        assert_eq!(done.elapsed, "30:00");
        assert_eq!(done.eta, "(+10m)");
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

        // The next task is sent and nobody runs `start`: the queued row is
        // the worker's from the message, and acme#614 is Done there.
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
                (Stage::Running, "acme#616", "10:00", "0"),
                (Stage::Done, "acme#614", "20:00", "11k"),
                (Stage::Done, "acme#613", "30:00", "28k"),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// giverny#217, the inbar orchestrator session: a plain `start` and the task handed
    /// over in the dispatcher's own words, with no `New task for you`. The
    /// row with no worker is the worker's from the message, and each task
    /// counts its own tokens — the next one named only as "a review round
    /// on #613".
    #[test]
    fn a_task_handed_by_message_in_any_words_counts_its_own_tokens() {
        let lines = [
            reply("m1", T0 - 59 * MIN, 20_000, 0, 1_000),
            sent(
                T0 - 40 * MIN + 13_000,
                "#613 verified and landed in Review — thanks. Next you hold acme#614 and nothing else.",
            ),
            reply("m2", T0 - 35 * MIN, 5_000, 20_000, 2_000),
        ];
        let (dir, rows, logs) = reused_worker("plain-start", &lines, true);
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"acme#614","stage":"running","started":{s614}}},
              {{"key":"acme#613","stage":"done","agent_id":"w","started":{s613},"ended":{s614}}}
            ]}}"#,
            s613 = T0 - 60 * MIN,
            s614 = T0 - 40 * MIN,
        ));
        let t = build_at(Some(&f), &rows, T0, &Clock::plain(), &logs);
        let got: Vec<(Stage, &str, &str, Option<&str>)> = t
            .lines
            .iter()
            .map(|l| {
                (
                    l.stage,
                    l.id.as_str(),
                    l.tokens.as_str(),
                    l.click.agent_id.as_deref(),
                )
            })
            .collect();
        let (k21, k7) = (fmt_tokens(21_000), fmt_tokens(7_000));
        assert_eq!(
            got,
            [
                (Stage::Running, "acme#614", k7.as_str(), Some("w")),
                (Stage::Done, "acme#613", k21.as_str(), Some("w")),
            ]
        );

        // acme#614 landed (the orchestrator session linked it), a round of #613 started
        // with no worker, and the message names only #614 and #613.
        let more = [
            sent(
                T0 - 20 * MIN + 7_000,
                "#614 verified and landed in Review. Now a review round on #613.",
            ),
            reply("m3", T0 - 10 * MIN, 3_000, 25_000, 500),
        ];
        let (dir2, rows, logs) =
            reused_worker("plain-round", &[&lines[..], &more[..]].concat(), true);
        let f = feed(&format!(
            r#"{{"session":"s","rows":[
              {{"key":"acme#613-r1","stage":"running","started":{r1}}},
              {{"key":"acme#614","stage":"done","agent_id":"w","started":{s614},"ended":{e614}}},
              {{"key":"acme#613","stage":"done","agent_id":"w","started":{s613},"ended":{s614}}}
            ]}}"#,
            s613 = T0 - 60 * MIN,
            s614 = T0 - 40 * MIN,
            e614 = T0 - 25 * MIN,
            r1 = T0 - 20 * MIN,
        ));
        let t = build_at(Some(&f), &rows, T0, &Clock::plain(), &logs);
        let got: Vec<(Stage, &str, &str, Option<&str>)> = t
            .lines
            .iter()
            .map(|l| {
                (
                    l.stage,
                    l.id.as_str(),
                    l.tokens.as_str(),
                    l.click.agent_id.as_deref(),
                )
            })
            .collect();
        let k35 = fmt_tokens(3_500);
        assert_eq!(
            got,
            [
                (Stage::Running, "acme#613-r1", k35.as_str(), Some("w")),
                (Stage::Done, "acme#614", k7.as_str(), Some("w")),
                (Stage::Done, "acme#613", k21.as_str(), Some("w")),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// No feed at all: the dispatcher sent `New task for you: …` twice and
    /// recorded nothing. Each task still gets its row, the earlier ones Done
    /// with their own span, and the Done counts add up to what the worker
    /// added before its current task, each turn once.
    #[test]
    fn hand_offs_nobody_recorded_are_read_from_the_transcript() {
        let lines = [
            reply("m1", T0 - 55 * MIN, 10_000, 0, 1_000),
            sent(
                T0 - 45 * MIN,
                "One more thing on acme#613: keep the old axis.",
            ),
            reply("m2", T0 - 44 * MIN, 2_000, 10_000, 500),
            sent(T0 - 40 * MIN, "New task for you: acme#614, same template."),
            reply("m3", T0 - 30 * MIN, 4_000, 12_000, 600),
            reply("m3", T0 - 30 * MIN, 4_000, 12_000, 900),
            sent(T0 - 20 * MIN, "Next task: acme#616 (the legend)."),
            reply("m4", T0 - 10 * MIN, 3_000, 16_000, 700),
        ];
        let (dir, rows, logs) = reused_worker("derived", &lines, true);
        let t = build_at(None, &rows, T0, &Clock::plain(), &logs);
        let got: Vec<(Stage, &str, &str, &str, &str)> = t
            .lines
            .iter()
            .map(|l| {
                (
                    l.stage,
                    l.id.as_str(),
                    l.elapsed.as_str(),
                    l.now.as_str(),
                    l.tokens.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (Stage::Running, "acme#616", "20:00", "", "3.7k"),
                (Stage::Done, "acme#614", "20:00", "→ acme#616", "4.9k"),
                (Stage::Done, "acme#613", "20:00", "→ acme#614", "13.5k"),
            ]
        );
        assert!(
            t.lines[0].no_eta,
            "a hand-off nobody recorded has no estimate"
        );
        assert!(t.lines[0].title.starts_with("Next task: acme#616"));
        assert_eq!(
            logs["w"].added(0, Some(T0 - 20 * MIN)),
            11_000 + 2_500 + 4_900
        );

        // Once the worker has finished, its last task is Done too, with the rest.
        let (dir2, rows, logs) = reused_worker("derived-done", &lines, false);
        let t = build_at(None, &rows, T0, &Clock::plain(), &logs);
        let last = t.lines.iter().find(|l| l.id == "acme#616").unwrap();
        assert_eq!((last.stage, last.elapsed.as_str()), (Stage::Done, "15:00"));
        assert_eq!(last.tokens, "3.7k");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// giverny#258: what the pane draws for a Running row that nothing
    /// runs. A row linked to a worker that has finished — here acme#615,
    /// which a hand-off message to `w` only mentioned — stays Running, as
    /// its writer says, and says its worker finished and it has not landed;
    /// a Running row with no worker at all says so once a spawn would have
    /// come; one just started, or whose worker works, says nothing of it.
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
        assert_eq!(got.now, "worker finished — not landed");
        assert!(got.flag);
        assert!(
            got.click
                .facts
                .iter()
                .any(|f| f == "worker finished — not landed")
        );
        assert_eq!(got.now.chars().count(), NOW_W, "fits NOW");
        let none = line("acme#617");
        assert_eq!((none.stage, none.now.as_str()), (Stage::Running, NO_WORKER));
        assert!(none.flag);
        assert_eq!(none.click.agent_id, None, "only mentioned: not w's");
        let fresh = line("acme#618");
        assert_eq!((fresh.now.as_str(), fresh.flag), ("", false));
        assert!(!line("acme#614").flag, "a Done row is not flagged");

        // A worker that failed says how; one that works says nothing of it.
        let (dir2, mut rows, logs) = reused_worker("failed", &lines, false);
        rows[0].outcome = Some(Outcome::Failed);
        let t = build_at(Some(&f), &rows, T0, &Clock::plain(), &logs);
        let got = t.lines.iter().find(|l| l.id == "acme#615").unwrap();
        assert_eq!(got.now, "worker failed — not landed");
        let (dir3, rows, logs) = reused_worker("working", &lines, true);
        let t = build_at(Some(&f), &rows, T0, &Clock::plain(), &logs);
        let got = t.lines.iter().find(|l| l.id == "acme#615").unwrap();
        assert!(!got.flag, "{got:?}");
        for d in [dir, dir2, dir3] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// Claude Code lists a worker woken by a message afresh, its
    /// `startTime` moved to that message. A worker on its third task, so
    /// listed, still has a row per task: each Done row counts its own span
    /// and stays put while the worker goes on, the Running row counts from
    /// its own start, and a count the feed froze at landing is the one shown.
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
        let table = |lines: &[String], f: &Feed| {
            std::fs::write(&path, lines.join("\n") + "\n").unwrap();
            let mut rows = live(&format!(
                r#"{{"session_id":"s","tasks":[{{"id":"w","status":"running",
                    "description":"acme#613 market SD graph","startTime":{},
                    "tokenCount":156313}}]}}"#,
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
            row("acme#616", "20:00", 3_700),
            row("acme#614", "20:00", 4_600),
            row("acme#613", "20:00", 11_000),
        ];
        assert_eq!(table(&lines, &f), want);
        // The worker goes on: only its Running row moves.
        lines.push(reply("m4", T0 - 5 * MIN, 50_000, 17_000, 900));
        let mut later = want.clone();
        later[0].2 = fmt_tokens(3_700 + 50_900);
        assert_eq!(table(&lines, &f), later);
        // A count frozen when the row landed is the one drawn.
        let mut f = f;
        f.rows[1].task_tokens = Some(4_321);
        assert_eq!(table(&lines, &f)[1].2, fmt_tokens(4_321));
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
        let one = build_at(None, &rows, T0, &Clock::plain(), &Logs::new());
        let split = build_at(None, &rows, T0, &Clock::plain(), &logs);
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

    // ------------------------------------------------- resources ----

    /// A feed with a granted Running row and a queued Next up row, as
    /// `giverny orchestrator-session claim` leaves them.
    const LEASED: &str = r#"{"session":"s","rows":[
        {"key":"demo#12","stage":"running","eta_s":1800,"started":1790000000000,
         "lease":{"state":"granted","id":"s:demo#12","cpu":3,"ram_mb":3072,
                  "slots":["cargo:/x/target"],"granted_at":1790000000000}},
        {"key":"demo#13","stage":"planned","eta_s":600,
         "lease":{"state":"queued","position":1,"behind":"demo#12","cpu":2,"ram_mb":3072}}]}"#;

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
        assert_eq!(lease_fact(&l), "queued #2 for 3G");
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
             "usage":{"runs":1,"peak_mb":128,"oom_kills":1}},
            {"key":"demo#14","stage":"done","started":1790000000000,"ended":1790000600000,
             "usage":{"runs":2,"peak_mb":512}},
            {"key":"demo#15","stage":"done","started":1790000000000,"ended":1790000600000}]}"#;
        // No run going: zero in the cell, the lease in the header only.
        let t = build(Some(&feed(json)), &[], T0 + 60_000);
        assert_eq!(t.lines[0].usage.trim_end(), "      0% CPU  ·  0.0G");
        assert!(!t.lines[0].oom);
        let facts = &t.lines[0].click.facts;
        assert!(facts.iter().any(|f| f == "holds 3 cpu, 3G"), "{facts:?}");
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

    /// A worker's commands show in its row with no `orchestrator-session run` at all; a
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
                .any(|f| f == "holds 3 cpu, 3G, slot cargo:/x/target"),
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

    /// A row's copy is only as fresh as its session's
    /// last claim. The ledger decides.
    #[test]
    fn the_ledger_wins_over_a_rows_stale_copy() {
        let f = feed(LEASED);
        // Both gone from the ledger (released, expired): nothing is held.
        let empty = with_ledger(&f, &["s"], &Ledger::default());
        assert!(empty.rows.iter().all(|r| r.lease.is_none()));

        // demo#12 released; demo#13 still waits, now first, behind
        // another session's task holding nothing it named: behind nobody.
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
        assert_eq!(q.behind, None, "demo#12 holds nothing now");

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
            "holds 1 cpu, 1G, slot cargo:/x/target"
        );
        assert!(
            with_ledger(&bare, &["s"], &l).rows[0].lease.is_none(),
            "not our session"
        );
        // Queued with no copy: behind the holder of the slot it asks for.
        let g = with_ledger(
            &feed(r#"{"session":"s","rows":[{"key":"demo#13","stage":"planned"}]}"#),
            &["s"],
            &l,
        );
        let q = g.rows[0].lease.as_ref().unwrap();
        assert_eq!(queued_note(q), "queued for 1c behind demo#12");
    }

    #[test]
    fn the_feed_footer_is_carried_verbatim() {
        let f = feed(r#"{"rows":[{"key":"k","stage":"planned"}],"footer":{"text":"session 3"}}"#);
        let t = build(Some(&f), &[], T0);
        assert_eq!(t.footer.as_deref(), Some("session 3"));
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

    /// The use column is the last, and on screen its figures sit in the
    /// very columns of the status line's use part drawn right above it:
    /// both start [`INSET`] cells into the terminal, both are as wide, and
    /// both are built from the same fixed-width pieces.
    #[test]
    fn the_use_column_lines_up_under_the_status_line() {
        let gpu_live = |gpu_mb| task_live_gpu("demo#12", 14, 4300, gpu_mb);
        for (columns, gpu) in [(120usize, None), (137, None), (160, Some(1229))] {
            let su = session_use::SessionUse {
                cpu_pct: 14,
                mem_mb: 4300,
                gpu_mb: gpu,
            };
            let right = session_use::segments(&su).join(session_use::SEP);
            let width = session_use::line_width(Some(&columns.to_string()));
            let status = session_use::align("Opus 5.5  ·  session: 1.2k", &right, width);
            let status = format!("{}{status}", " ".repeat(INSET));

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
            // Character for character under the status line's part.
            assert_eq!(
                status.chars().count(),
                row.chars().count(),
                "{status:?}\n{row:?}"
            );
            let tail = right.chars().count();
            let above: String = status.chars().skip(status.chars().count() - tail).collect();
            let below: String = row.chars().skip(row.chars().count() - tail).collect();
            assert_eq!(above, below, "{columns} columns");
            // And it ends where the status line does: two columns in from
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
            limit,
            tz: jiff::tz::TimeZone::UTC,
            workers: &NO_WORKERS,
            etas: &NO_ETAS,
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
        assert_eq!(held_ms(&holds, 5, 30), 5 + 10);
        assert_eq!(held_ms(&holds, 12, 18), 0);
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

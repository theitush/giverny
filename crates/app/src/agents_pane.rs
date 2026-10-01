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
//! above it — the same table `coo/tools/orchestrate-status` pins under
//! Claude Code. The session's token total is not here: it is on the status
//! line's model row.
//!
//! **The rows are not kept here.** [`show`] is handed the tab's tracker —
//! `ClaudeWatch::agents` (`agents_live.rs`), fed by the relay, persisted,
//! emptied on `/clear` and refreshed once a second — and keeps only what the
//! pane itself needs per tab: the feed it last read.
//!
//! **Clocks stop while nothing can run** (giverny#53): while the tab's
//! account is out of a usage limit ([`Limit`]), a Running row's ELAPSED
//! stops and its ETA holds instead of counting down past zero, and NOW says
//! `5h limit → 13:00`. The pane remembers each such span ([`Hold`]) and
//! both clocks carry on from where they stopped once the limit resets. A
//! feed row the orchestrator has paused (`paused_since`, coo#170) is held
//! the same way, and reads `paused since 12:58`. Planned ETAs are durations
//! and hold by themselves; a Done row is measured history and never moves.
//!
//! **Clicks** produce a [`RowClick`], which the app receives as
//! `Action::AgentRowClicked`. What a click *does* is not decided here, and
//! it leaves no mark: a row is tinted only while the pointer is on it
//! ([`row_tint`]), so nothing stays highlighted after a click (giverny#40).
//!
//! **Text is selectable** (giverny#84): a drag — never a click — selects
//! the pane's text the way the terminal does, as a stream of cells across
//! rows, copies it as plain text line by line when the drag ends, and
//! `Ctrl+Shift+C` copies it again. The next press anywhere lets it go.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Color32, CursorIcon, Sense, Ui};
use giverny_claude::feed::{self, Feed, FeedCache, PaneRow, Stage};
use giverny_claude::subagents::{Outcome, SubagentRow, Tracker};
use giverny_core::tabs::TabId;
use giverny_term::widget::RenderShared;

use crate::chrome::Chrome;
use crate::claude_watch::ClaudeWatch;

/// The three stage tints, the 256-colour codes `orchestrate-status` uses
/// (38;5;32, 38;5;67, 38;5;28), picked there to read on dark and light
/// themes alike.
pub const RUNNING: Color32 = Color32::from_rgb(0x00, 0x87, 0xd7);
pub const PLANNED: Color32 = Color32::from_rgb(0x5f, 0x87, 0xaf);
pub const DONE: Color32 = Color32::from_rgb(0x00, 0x87, 0x00);

/// How often the feed file is stat'ed.
const POLL: Duration = Duration::from_secs(1);

// Column widths, in characters (orchestrate-status: HEAD_W, EL_W, ETA_W,
// NOW_W, TOK_W).
const STAGE_W: usize = 7;
const EL_W: usize = 8;
const ETA_W: usize = 10;
const NOW_W: usize = 28;
const TOK_W: usize = 6;
const GAP: usize = 2;
const MIN_TITLE: usize = 12;

/// What a Running row's ETA cell says when nobody gave it an estimate —
/// drawn dim, so a row the dispatcher forgot to estimate reads as missing
/// one rather than as a blank (giverny#140).
pub const NO_ETA: &str = "no ETA";
/// Where in [`Cols::segments`] the ETA cell sits.
const ETA_SEG: usize = 3;

// --------------------------------------------------------- view state ----

/// What the pane itself keeps per tab: the feed it last read and the text
/// dragged over. Never the rows — those are the tracker's — and no row
/// selection: a click acts and leaves no mark (giverny#40).
#[derive(Default)]
struct View {
    feed: FeedCache,
    feed_now: Option<Feed>,
    feed_session: Option<String>,
    last_poll: Option<Instant>,
    /// The height the pane last sized itself to, while the user has not
    /// dragged it; `None` once they have.
    fit: Option<f32>,
    /// Every usage-limit span this tab's clocks were held through.
    holds: Vec<Hold>,
    /// The text dragged over, until the next press (giverny#84).
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
    /// not on a worker's row, `Some(Some(row))` on that row (giverny#132).
    hover: Option<Option<RowClick>>,
}

impl View {
    /// Re-read the feed for `session`, at most once a [`POLL`] (or at once
    /// when the session changed).
    fn poll_feed(&mut self, session: Option<&str>) {
        let changed = self.feed_session.as_deref() != session;
        if !changed && self.last_poll.is_some_and(|t| t.elapsed() < POLL) {
            return;
        }
        self.last_poll = Some(Instant::now());
        self.feed_session = session.map(str::to_string);
        self.feed_now = session.and_then(|sid| self.feed.poll(&feed::feed_dir(), sid).cloned());
    }
}

/// Every tab's pane view state.
#[derive(Default)]
pub struct Views {
    tabs: HashMap<TabId, View>,
}

impl Views {
    /// Forget a closed tab.
    pub fn forget(&mut self, tab: TabId) {
        self.tabs.remove(&tab);
    }

    /// Where the pointer was over `tab`'s rows when the pane was last drawn,
    /// taken so it is never read twice: `None` off the pane (or when the
    /// pane was not drawn), `Some(None)` on it but off every row,
    /// `Some(Some(row))` on a row (giverny#132).
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
}

#[cfg(any(test, debug_assertions))]
impl Clock<'_> {
    pub(crate) fn plain() -> Clock<'static> {
        Clock {
            holds: &[],
            limit: None,
            tz: jiff::tz::TimeZone::system(),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ------------------------------------------------------------- clicks ----

/// What a click on a row names — everything the action behind it (build
/// task D) could want, so deciding what to do never needs the pane again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowClick {
    pub stage: Stage,
    /// The feed's key (`coo#158`), else empty.
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
    /// how long it took against its estimate, its tokens (giverny#41).
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

/// What a row click does (giverny#121): a click on the row the tab is
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
    /// drawn as a dim [`NO_ETA`] (giverny#140).
    pub no_eta: bool,
    pub now: String,
    pub tokens: String,
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
    build_at(feed, live, now_ms, &Clock::plain())
}

/// [`build`], with the clocks held through `clock`'s usage-limit spans.
pub fn build_at(feed: Option<&Feed>, live: &[SubagentRow], now_ms: u64, clock: &Clock) -> Table {
    let rows = feed::merge(feed, live);
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
/// (coo moves `started` on by the open pause up to then), never before the
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
            f.key.clone(),
            f.title
                .clone()
                .or_else(|| l.map(|l| l.display_name().to_string()))
                .unwrap_or_default(),
        ),
        (None, Some(l)) => match (&l.name, &l.description) {
            (Some(n), Some(d)) => (n.clone(), d.clone()),
            _ => (String::new(), l.display_name().to_string()),
        },
        (None, None) => (String::new(), String::new()),
    };
    // This row's own start: the feed's (a worker holding several tasks
    // started each at a different time, and coo moves it on by the row's
    // pauses), else the worker's.
    let row_start = row.started_ms();
    let paused = row.paused_since_ms();
    // A feed row that carries its pauses has its stops taken off `started`
    // already (coo#170, and coo#200 pauses rows for a limit itself): the
    // pane's own holds would take them off twice.
    let writer_pauses = f.is_some_and(|f| f.paused_s.is_some() || f.paused_since_ms.is_some());
    let holds = if writer_pauses { &[][..] } else { clock.holds };
    // A Running row's work so far, in seconds: wall time up to where its
    // clock stands, less every span the account was out of its limit.
    // A worker stopped on an API error, or ended badly under a Running
    // row, stands still from then, and its error spans are off its clock
    // (giverny#91).
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
            let end = f
                .and_then(|f| f.ended_ms)
                .or_else(|| l.filter(|l| !l.running()).and_then(|l| l.ended_ms));
            match (row_start, end) {
                (Some(s), Some(e)) => {
                    stopwatch(e.saturating_sub(s).saturating_sub(error_ms(s, e)) / 1000)
                }
                _ => String::new(),
            }
        }
    };
    let eta_s = f.and_then(|f| f.eta_s);
    let eta = match row.stage {
        Stage::Running => match (eta_s, work_s) {
            (Some(eta), Some(work)) => countdown(eta as i64 - work as i64),
            _ => String::new(),
        },
        Stage::Planned => eta_s
            .map(|e| format!("~{}", feed::fmt_span(e as i64)))
            .unwrap_or_default(),
        Stage::Done => row.eta_delta_s().map(feed::fmt_delta).unwrap_or_default(),
    };
    let no_eta = row.stage == Stage::Running && eta_s.is_none();
    let limit = clock.limit.filter(|l| l.out_at(now_ms));
    let now = match row.stage {
        // The limit first: a row the writer paused for it says why.
        Stage::Running => match (limit, paused) {
            (Some(limit), _) => limit_note(limit, now_ms, &clock.tz),
            (None, Some(p)) => format!("paused since {}", clock_at(p, now_ms, &clock.tz)),
            (None, None) if stopped.is_some() => stopped.map(|(_, why)| why).unwrap_or_default(),
            (None, None) => l
                .filter(|l| l.running())
                .and_then(|l| l.activity.clone())
                .unwrap_or_default(),
        },
        Stage::Planned => String::new(),
        Stage::Done => f
            .and_then(|f| f.landing.clone())
            .or_else(|| l.and_then(|l| l.outcome).map(outcome_word))
            .unwrap_or_default(),
    };
    let tokens = row.tokens().map(fmt_tokens).unwrap_or_default();
    let mut facts = row_facts(row.stage, &elapsed, &eta, &now, &tokens);
    if no_eta {
        facts.insert(
            1.min(facts.len()),
            no_eta_hint(&key, l.map(|l| l.agent_id()), &title),
        );
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

/// How the dispatcher gives a Running row with no estimate one, as the
/// overlay header says it (giverny#140): a feed row is re-estimated in
/// place; a worker with no feed row at all needs one planned and started
/// first, since `orchestrate-status eta` answers "nothing to re-estimate"
/// when there is no pass.
fn no_eta_hint(key: &str, agent_id: Option<&str>, title: &str) -> String {
    if !key.is_empty() {
        return format!("no ETA — add one: orchestrate-status eta {key} <min> --why scope");
    }
    let id = task_key(title).unwrap_or("<task>");
    let agent = agent_id
        .map(|a| format!(" --agent {a}"))
        .unwrap_or_default();
    format!(
        "no ETA — add one: echo '{id} | <title> | 1 | <min>' | orchestrate-status plan \
         && orchestrate-status start {id} --eta <min>{agent}"
    )
}

/// The first `repo#n` a worker's description names (`inbar#613 market SD
/// graph` → `inbar#613`), the id its row would be planned under.
fn task_key(text: &str) -> Option<&str> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .find(|w| {
            w.split_once('#').is_some_and(|(repo, n)| {
                !repo.is_empty()
                    && repo
                        .chars()
                        .all(|c| c.is_alphanumeric() || "-_./".contains(c))
                    && !n.is_empty()
                    && n.chars().all(|c| c.is_ascii_digit())
            })
        })
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
/// selection (giverny#75). `header` names the worker the terminal's
/// header is about (giverny#82), whose row comes back with the click, as
/// the pane drew it.
#[allow(clippy::too_many_arguments)]
pub fn show(
    views: &mut Views,
    tab: TabId,
    tracker: Option<&Tracker>,
    viewed: Option<&str>,
    header: Option<&str>,
    limit: Option<Limit>,
    chrome: &Chrome,
    shared: &mut RenderShared,
    ui: &mut Ui,
) -> (Option<RowClick>, Option<Line>) {
    let Some(tracker) = tracker else {
        return (None, None);
    };
    let view = views.tabs.entry(tab).or_default();
    view.poll_feed(tracker.session_id.as_deref());
    let now = now_ms();
    track(&mut view.holds, limit.as_ref(), now);
    let clock = Clock {
        holds: &view.holds,
        limit: limit.as_ref(),
        tz: jiff::tz::TimeZone::system(),
    };
    // Done rows cleared by hand go from the feed too, whoever wrote it.
    let cleared = tracker
        .done_cleared_ms
        .and_then(|c| view.feed_now.as_ref().map(|f| f.without_done_by(c)));
    let feed = cleared.as_ref().or(view.feed_now.as_ref());
    let table = build_at(feed, tracker.rows(), now, &clock);
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
            // columns would jump as the pane is resized across that point
            // (giverny#45).
            let cols = table_cols(ui.available_width(), cell.x, bar_lane(ui));
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let (click, rects, hover) = draw_table(
                        ui,
                        id.with("rows"),
                        &table,
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
        });
    (clicked, header_line)
}

/// The pane's frame: the session's own background, not the rail's lifted
/// panel colour, so the pane reads as part of the terminal above it. egui's
/// separator line (on by default) keeps the boundary between the two.
fn pane_frame(bg: Color32) -> egui::Frame {
    egui::Frame::NONE
        .fill(bg)
        .inner_margin(egui::Margin::symmetric(8, 5))
}

/// The pane's outer height for `rows` rows of `row_h`: the rows, egui's
/// `gap` between each two of them, and the frame's `margin`. Leave out the
/// gaps and the rows overflow by one gap a row — about one row in seven —
/// and the scroll area's fade dims the last row shown (giverny#38).
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
/// input — a click leaves nothing behind to highlight (giverny#40).
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

/// How many character columns the table lays out in, from the pane's width
/// *outside* the scroll area. The bar's lane is always left free, so it
/// never paints over TOKENS and the columns never move when it appears.
fn table_cols(width: f32, cell_w: f32, bar_lane: f32) -> usize {
    let cw = cell_w.max(1.0);
    let usable = width - bar_lane - cw;
    ((usable / cw).floor() as usize).max(40)
}

/// Where each column of a row starts or ends, in characters, for a table
/// laid out in `cols` columns.
struct Cols {
    idw: usize,
    taskw: usize,
    x_task: usize,
    x_el_end: usize,
    x_eta_end: usize,
    x_now: usize,
    x_tok_end: usize,
}

impl Cols {
    fn new(table: &Table, cols: usize) -> Self {
        let idw = table
            .lines
            .iter()
            .map(|l| l.id.chars().count())
            .max()
            .unwrap_or(0);
        let fixed = STAGE_W + GAP + EL_W + GAP + ETA_W + GAP + NOW_W + GAP + TOK_W;
        let taskw = cols.saturating_sub(fixed).max(MIN_TITLE);
        let x_task = STAGE_W + GAP;
        let x_el_end = x_task + taskw + GAP + EL_W;
        let x_eta_end = x_el_end + GAP + ETA_W;
        let x_now = x_eta_end + GAP;
        let x_tok_end = x_now + NOW_W + GAP + TOK_W;
        Cols {
            idw,
            taskw,
            x_task,
            x_el_end,
            x_eta_end,
            x_now,
            x_tok_end,
        }
    }

    /// A row's cells as drawn: each one's text and the column it starts at.
    fn segments(&self, line: &Line) -> Vec<(usize, String)> {
        let idw = self.idw;
        // The id is never cut; the title takes what is left.
        let task = if idw > 0 {
            let title_w = self.taskw.saturating_sub(idw + 1);
            format!("{:<idw$} {}", line.id, cut(&line.title, title_w))
        } else {
            cut(&line.title, self.taskw)
        };
        vec![
            (0, stage_word(line.stage).to_string()),
            (self.x_task, task),
            // Right-aligned: the text ends at the column's last cell.
            (right_at(self.x_el_end, &line.elapsed), line.elapsed.clone()),
            (
                right_at(self.x_eta_end, eta_cell(line)),
                eta_cell(line).into(),
            ),
            (self.x_now, cut(&line.now, NOW_W)),
            (right_at(self.x_tok_end, &line.tokens), line.tokens.clone()),
        ]
    }
}

/// The ETA cell's text: the row's own, or [`NO_ETA`] for a Running row
/// nobody estimated.
fn eta_cell(line: &Line) -> &str {
    if line.no_eta { NO_ETA } else { &line.eta }
}

/// A row's text as it reads on screen, one character a cell: what a
/// selection over it copies (giverny#84).
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

/// A drag over the pane's text (giverny#84), in `(row, column)` cells —
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
    viewed: Option<&str>,
    chrome: &Chrome,
    shared: &mut RenderShared,
    cell: egui::Vec2,
    row_h: f32,
    cols: usize,
    sel: &mut Option<Selection>,
) -> (Option<RowClick>, Vec<egui::Rect>, Option<Option<RowClick>>) {
    let cw = cell.x.max(1.0);
    let layout = Cols::new(table, cols);
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
            // The worker whose view the terminal above shows (giverny#75):
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
            shared.paint_text(&p, at, s, if missing { chrome.dim } else { color });
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
        let r = row("giverny#1", Some("a1"), Stage::Running);
        assert_eq!(toggle(&r, Some("a1"), None), Toggle::Home);
        // The worker finished while it was viewed: still its row.
        let done = row("giverny#1", Some("a1"), Stage::Done);
        assert_eq!(toggle(&done, Some("a1"), None), Toggle::Home);
    }

    #[test]
    fn clicking_another_row_while_viewing_opens_it() {
        let r = row("giverny#2", Some("b2"), Stage::Running);
        assert_eq!(toggle(&r, Some("a1"), None), Toggle::Open);
        let planned = row("giverny#3", None, Stage::Planned);
        assert_eq!(toggle(&planned, Some("a1"), None), Toggle::Open);
    }

    #[test]
    fn clicking_with_nothing_viewed_opens() {
        let r = row("giverny#1", Some("a1"), Stage::Running);
        assert_eq!(toggle(&r, None, None), Toggle::Open);
        let keyless = row("", None, Stage::Done);
        assert_eq!(toggle(&keyless, None, None), Toggle::Open);
    }

    #[test]
    fn clicking_the_row_whose_overlay_is_up_closes_it() {
        let done = row("giverny#1", Some("a1"), Stage::Done);
        assert_eq!(toggle(&done, None, Some(&done)), Toggle::Close);
        // The overlay was opened while the row was Running.
        let was = row("giverny#1", Some("a1"), Stage::Running);
        assert_eq!(toggle(&done, None, Some(&was)), Toggle::Close);
        let other = row("giverny#2", Some("b2"), Stage::Done);
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
                "tokenCount":64100,"label":"Editing tools/board"}]}"#,
        );
        let t = build(None, &rows, T0);
        assert_eq!(t.lines.len(), 1);
        let l = &t.lines[0];
        assert_eq!(l.stage, Stage::Running);
        assert_eq!(l.title, "Fix the board");
        assert_eq!(l.elapsed, "0:42");
        assert_eq!(l.now, "Editing tools/board");
        assert_eq!(l.tokens, "64.1k");
        assert_eq!(l.eta, "");
        assert_eq!(l.click.agent_id.as_deref(), Some("a1"));
    }

    #[test]
    fn a_running_row_with_no_estimate_says_so() {
        // A worker spawned outside a pass (giverny#140): a row, but no ETA.
        let rows = live(
            r#"{"session_id":"s","tasks":[{"id":"a1","status":"running",
                "description":"inbar#613 market SD graph","startTime":1789999958000}]}"#,
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
        assert!(
            hint.contains("orchestrate-status start inbar#613 --eta <min> --agent a1"),
            "{hint}"
        );
        assert!(hint.contains("echo 'inbar#613 | <title> | 1 | <min>' | orchestrate-status plan"));

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
                .contains(&"no ETA — add one: orchestrate-status eta g#3 <min> --why scope".into())
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

    #[test]
    fn a_task_key_is_the_first_repo_and_number_named() {
        assert_eq!(task_key("inbar#613 market SD graph"), Some("inbar#613"));
        assert_eq!(
            task_key("Work theitush/giverny#82: x"),
            Some("theitush/giverny#82")
        );
        assert_eq!(task_key("fix #12 and coo#3"), Some("coo#3"));
        assert_eq!(task_key("Fix the board"), None);
    }

    #[test]
    fn the_feed_gives_ids_titles_etas_and_landings() {
        let rows = live(
            r#"{"session_id":"s","tasks":[{"id":"a1","status":"running",
                "startTime":1789999400000,"tokenCount":1000}]}"#,
        );
        let f = feed(
            r#"{"version":1,"session":"s","rows":[
              {"key":"coo#1","stage":"running","title":"FEATURE: one","agent_id":"a1",
               "started":1789999400000,"eta_s":1800},
              {"key":"coo#2","stage":"planned","title":"FEATURE: two","eta_s":3780},
              {"key":"coo#3","stage":"done","title":"BUG: three","started":1789990000000,
               "ended":1789993900000,"eta_s":3600,"landing":"Review — ita","tokens":5000}
            ]}"#,
        );
        let t = build(Some(&f), &rows, T0);
        let ids: Vec<&str> = t.lines.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, ["coo#1", "coo#2", "coo#3"]);
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

    /// giverny#83: a feed written at `start` (no `agent_id`, no tokens) and
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
                    "description":"Work giverny#82 open direct","startTime":1789999400000,
                    "tokenCount":49000}]}"#,
            ),
            T0,
        );
        let f = feed(
            r#"{"session":"s","rows":[
              {"key":"giverny#82","stage":"running","title":"FEATURE: open direct",
               "started":1789999400000,"eta_s":3600},
              {"key":"giverny#84","stage":"planned","title":"FEATURE: selectable","eta_s":1800}
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
            assert_eq!(l.id, "giverny#82");
            assert_eq!(l.title, "FEATURE: open direct");
            assert_eq!(l.eta, "~50m");
            assert_eq!(l.click.agent_id.as_deref(), Some("w82"));
            assert_eq!(l.now, format!("Edit: {file}"));
            seen.push(l.tokens.clone());
        }
        assert_eq!(seen, ["50k", "61.2k", "74.9k"]);
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn nothing_to_show_is_no_pane() {
        assert!(build(None, &[], T0).is_empty());
    }

    #[test]
    fn the_feed_footer_is_carried_verbatim() {
        let f = feed(r#"{"rows":[{"key":"k","stage":"planned"}],"footer":{"text":"session 3"}}"#);
        let t = build(Some(&f), &[], T0);
        assert_eq!(t.footer.as_deref(), Some("session 3"));
    }

    #[test]
    fn a_row_is_tinted_only_while_the_pointer_is_on_it() {
        // giverny#40: once the pointer has left, a clicked row looks like
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
        let cols = table_cols(width, cw, lane);
        // TOKENS' last cell ends clear of the bar's lane.
        assert!(cols as f32 * cw <= width - lane);
        // The width the scroll area hands its content drops by the lane
        // while the bar shows; the table is laid out from the width outside
        // it, so that drop changes nothing; laid out from the inner width,
        // it would have lost a column.
        assert_ne!(cols, table_cols(width - lane, cw, lane));
        // A cramped pane still gets a readable table.
        assert_eq!(table_cols(100.0, cw, lane), 40);
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
            table_cols(outer_fit, cw, lane),
            table_cols(outer_over, cw, lane)
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

    // ------------------------------------------- giverny#53: held clocks ----

    const MIN: u64 = 60_000;

    fn utc<'a>(holds: &'a [Hold], limit: Option<&'a Limit>) -> Clock<'a> {
        Clock {
            holds,
            limit,
            tz: jiff::tz::TimeZone::UTC,
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
            let t = build_at(Some(&f), &rows, now, &utc(&holds, Some(&limit)));
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
        let t = build_at(Some(&f), &rows, now, &utc(&holds, None));
        // 10 minutes before, 5 after: the hour out is not work.
        assert_eq!(t.lines[0].elapsed, "15:00");
        assert_eq!(t.lines[0].eta, "~15m");
        assert_eq!(t.lines[0].now, "Editing");
        // A meter still showing the lapsed limit is no limit: it lets go.
        let t = build_at(Some(&f), &rows, now, &utc(&holds, Some(&limit)));
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
        let t = build_at(Some(&f), &rows, T0 + 90 * MIN, &utc(&holds, Some(&limit)));
        assert_eq!(t.lines[0].elapsed, "10:00");
        assert_eq!(t.lines[0].now, "limit");
        track(&mut holds, None, T0 + 90 * MIN);
        assert_eq!(holds[0].until_ms, Some(T0 + 90 * MIN));
    }

    #[test]
    fn a_paused_feed_row_holds_its_clock() {
        // coo#170: paused at T0; `started` already moved on by the open
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
            let t = build_at(Some(&f), &rows, now, &utc(&[], None));
            assert_eq!(t.lines[0].elapsed, "10:00", "{now}");
            assert_eq!(t.lines[0].eta, "~20m");
            assert_eq!(t.lines[0].now, "paused since 14:13");
        }
        // The writer paused it for a limit (coo#200): its `started` already
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
        let t = build_at(Some(&f), &rows, written + MIN, &utc(&holds, Some(&limit)));
        assert_eq!(t.lines[0].elapsed, "10:00");
        assert_eq!(t.lines[0].now, "5h limit → 15:13");
        // A feed with no write time stops at `paused_since`.
        f.written_ms = None;
        let t = build_at(Some(&f), &rows, T0 + 60 * MIN, &utc(&[], None));
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
            let t = build_at(Some(&f), &[], now, &utc(&holds, Some(&limit)));
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

    // ------------------------------------- giverny#84: selectable text ----

    fn rows_text() -> Vec<String> {
        vec![
            "Running  giverny#84 FEATURE: selectable      1:02".into(),
            "NextUp   giverny#85 BUG: something".into(),
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
            "giverny#84 FEATURE: selectable      1:02\nNextUp   giv"
        );
        // Past the row's end is the row's end, and blanks are not copied.
        let sel = Selection {
            anchor: (0, 0),
            head: (2, 99),
        };
        assert_eq!(selected_text(&rows, &sel), rows.join("\n"));
        let one = Selection {
            anchor: (0, 9),
            head: (0, 19),
        };
        assert_eq!(selected_text(&rows, &one), "giverny#84");
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
        let want = "giverny#84 FEATURE: selectable      1:02\nNextUp   giv";
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

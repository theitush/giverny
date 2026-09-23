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
//! above it, and a ledger `total:` row — the same table
//! `coo/tools/orchestrate-status` pins under Claude Code.
//!
//! **The seam.** [`show`] takes the tab's tracker as an argument and owns
//! none: whoever keeps the trackers (the relay's per-tab store) hands one
//! in. Until that store exists on this branch, [`LocalTrackers`] stands in —
//! one tracker per tab, bound to the tab's session and refreshed from disk,
//! guessing Running from transcript mtimes since nothing relays the live
//! list yet. Swapping it for the relay's store is one argument in `main.rs`.
//!
//! **Clicks** produce a [`RowClick`], which the app receives as
//! `Action::AgentRowClicked`. What a click *does* is not decided here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Sense, Ui};
use giverny_claude::feed::{self, Feed, FeedCache, PaneRow, Stage};
use giverny_claude::subagents::{self, LiveSnapshot, LiveTask, Outcome, SubagentRow, Tracker};
use giverny_core::tabs::{Tab, TabId};

use crate::chrome::Chrome;

/// The three stage tints, the 256-colour codes `orchestrate-status` uses
/// (38;5;32, 38;5;67, 38;5;28), picked there to read on dark and light
/// themes alike.
pub const RUNNING: Color32 = Color32::from_rgb(0x00, 0x87, 0xd7);
pub const PLANNED: Color32 = Color32::from_rgb(0x5f, 0x87, 0xaf);
pub const DONE: Color32 = Color32::from_rgb(0x00, 0x87, 0x00);

/// How often the disk is read and the feed file stat'ed.
const POLL: Duration = Duration::from_secs(1);
/// Disk guess only: a transcript written this recently is a running worker.
const FRESH: Duration = Duration::from_secs(600);

// Column widths, in characters (orchestrate-status: HEAD_W, EL_W, ETA_W,
// NOW_W, TOK_W).
const STAGE_W: usize = 7;
const EL_W: usize = 8;
const ETA_W: usize = 10;
const NOW_W: usize = 28;
const TOK_W: usize = 6;
const GAP: usize = 2;
const MIN_TITLE: usize = 12;

const FONT_SIZE: f32 = 12.0;

// --------------------------------------------------------- view state ----

/// What the pane itself keeps per tab: the feed it last read and the row
/// last clicked. Never the rows — those are the tracker's.
#[derive(Default)]
struct View {
    feed: FeedCache,
    feed_now: Option<Feed>,
    feed_session: Option<String>,
    last_poll: Option<Instant>,
    /// The clicked row, by [`Line::ident`]. Only it is highlighted.
    selected: Option<String>,
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
}

// ------------------------------------------------------ interim store ----

/// Stand-in for the relay's per-tab tracker store, until it lands here: one
/// [`Tracker`] per tab, bound to the tab's session, refreshed from disk once
/// a second, with Running guessed from transcripts written in the last
/// [`FRESH`] (nothing relays the live list yet). In memory only. Retired by
/// the merge with the relay branch — see the module docs.
#[derive(Default)]
pub struct LocalTrackers {
    tabs: HashMap<TabId, (Tracker, Option<Instant>)>,
}

impl LocalTrackers {
    /// `tab`'s tracker, synced to its session and refreshed if due. `None`
    /// until the tab has a Claude session.
    pub fn tracker(&mut self, tab: &Tab) -> Option<&Tracker> {
        let sid = tab.claude_session.as_deref()?;
        let (tracker, last) = self.tabs.entry(tab.id).or_insert_with(|| {
            let dir = tab.claude_config_dir.clone().or_else(default_config_dir);
            (Tracker::new(dir), None)
        });
        if tracker.session_id.as_deref() != Some(sid) {
            tracker.set_session(sid);
            *last = None;
        }
        if last.is_none_or(|t| t.elapsed() >= POLL) {
            *last = Some(Instant::now());
            disk_guess(tracker);
            tracker.refresh();
        }
        Some(&*tracker)
    }

    /// The tab's `/clear`, or its closing: a new conversation starts empty,
    /// not as an alias of the old one (whose Done rows would come back).
    pub fn clear(&mut self, tab: TabId) {
        self.tabs.remove(&tab);
    }
}

/// A synthetic live list from disk: every worker whose transcript was written
/// in the last [`FRESH`] is Running. Notifications (read by `refresh` right
/// after) still land them Done.
fn disk_guess(tracker: &mut Tracker) {
    let Some(config) = tracker.config_dir.clone() else {
        return;
    };
    let sessions: Vec<String> = tracker
        .session_id
        .iter()
        .chain(tracker.aliases.iter())
        .cloned()
        .collect();
    let mut tasks = Vec::new();
    for sid in &sessions {
        let Some(dir) = subagents::subagents_dir(&config, sid) else {
            continue;
        };
        for id in subagents::list_agent_ids(&dir) {
            let path = subagents::agent_transcript(&dir, &id);
            let fresh = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age < FRESH);
            if !fresh {
                continue;
            }
            let known = tracker.get(&id);
            if known.is_some_and(|r| !r.running()) {
                // Done; `refresh` decides revivals itself.
                continue;
            }
            // Only a row new to us pays for the meta and first-line reads.
            let (description, start_ms) = match known {
                Some(r) => (r.description.clone(), r.started_ms),
                None => (
                    subagents::read_meta(&dir, &id).description,
                    subagents::first_line_ms(&path),
                ),
            };
            tasks.push(LiveTask {
                id,
                kind: None,
                status: "running".into(),
                description,
                label: None,
                name: None,
                start_ms,
                model: None,
                tokens: None,
                cwd: None,
            });
        }
    }
    let snap = LiveSnapshot {
        session_id: None,
        tasks,
    };
    tracker.apply_live(&snap, now_ms());
}

fn default_config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
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
    pub now: String,
    pub tokens: String,
    pub click: RowClick,
}

impl Line {
    /// Stable identity for selection: stage, key and worker.
    fn ident(&self) -> String {
        format!(
            "{:?}|{}|{}",
            self.stage,
            self.click.key,
            self.click.agent_id.as_deref().unwrap_or("")
        )
    }
}

/// The whole table: rows, the ledger total, the feed's footer line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Table {
    pub lines: Vec<Line>,
    /// `total:`'s number — every worker counted once — when any row has one.
    pub total: Option<String>,
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

/// Merge and format. Pure: `now_ms` in, text out.
pub fn build(feed: Option<&Feed>, live: &[SubagentRow], now_ms: u64) -> Table {
    let rows = feed::merge(feed, live);
    let mut lines: Vec<Line> = Vec::with_capacity(rows.len());
    let mut counted: Vec<&str> = Vec::new();
    let mut total: u64 = 0;
    let mut any_tokens = false;
    for row in &rows {
        let line = format_row(row, now_ms);
        if let Some(n) = row.tokens() {
            any_tokens = true;
            match row.agent_id() {
                Some(id) if counted.contains(&id) => {}
                Some(id) => {
                    counted.push(id);
                    total += n;
                }
                None => total += n,
            }
        }
        lines.push(line);
    }
    dittos(&rows, &mut lines);
    Table {
        lines,
        total: any_tokens.then(|| fmt_tokens(total)),
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

fn format_row(row: &PaneRow<'_, SubagentRow>, now_ms: u64) -> Line {
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
    // started each at a different time), else the worker's.
    let row_start = f
        .and_then(|f| f.started_ms)
        .or_else(|| l.and_then(|l| l.started_ms));
    let elapsed = match row.stage {
        Stage::Planned => String::new(),
        Stage::Running => row
            .started_ms()
            .map(|s| stopwatch(now_ms.saturating_sub(s) / 1000))
            .unwrap_or_default(),
        Stage::Done => {
            let end = f
                .and_then(|f| f.ended_ms)
                .or_else(|| l.filter(|l| !l.running()).and_then(|l| l.ended_ms));
            match (row_start, end) {
                (Some(s), Some(e)) => stopwatch(e.saturating_sub(s) / 1000),
                _ => String::new(),
            }
        }
    };
    let eta_s = f.and_then(|f| f.eta_s);
    let eta = match row.stage {
        Stage::Running => match (eta_s, row_start) {
            (Some(eta), Some(s)) => {
                let left = s as i64 / 1000 + eta as i64 - now_ms as i64 / 1000;
                countdown(left)
            }
            _ => String::new(),
        },
        Stage::Planned => eta_s
            .map(|e| format!("~{}", feed::fmt_span(e as i64)))
            .unwrap_or_default(),
        Stage::Done => row.eta_delta_s().map(feed::fmt_delta).unwrap_or_default(),
    };
    let now = match row.stage {
        Stage::Running => l
            .filter(|l| l.running())
            .and_then(|l| l.activity.clone())
            .unwrap_or_default(),
        Stage::Planned => String::new(),
        Stage::Done => f
            .and_then(|f| f.landing.clone())
            .or_else(|| l.and_then(|l| l.outcome).map(outcome_word))
            .unwrap_or_default(),
    };
    let tokens = row.tokens().map(fmt_tokens).unwrap_or_default();
    Line {
        stage: row.stage,
        id,
        title,
        elapsed,
        eta,
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
        },
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
        Stage::Planned => "Planned",
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
pub fn show(
    views: &mut Views,
    tab: TabId,
    tracker: Option<&Tracker>,
    chrome: &Chrome,
    ui: &mut Ui,
) -> Option<RowClick> {
    let tracker = tracker?;
    let view = views.tabs.entry(tab).or_default();
    view.poll_feed(tracker.session_id.as_deref());
    let table = build(view.feed_now.as_ref(), tracker.rows(), now_ms());
    if table.is_empty() {
        return None;
    }
    if table.any_running() {
        // ELAPSED is a stopwatch.
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }

    let font = FontId::monospace(FONT_SIZE);
    let cw = ui.ctx().fonts_mut(|f| f.glyph_width(&font, '0')).max(1.0);
    let row_h = (FONT_SIZE * 1.45).round();
    let extra = usize::from(table.total.is_some()) + usize::from(table.footer.is_some());
    let want = (table.lines.len() + extra) as f32 * row_h + 10.0;
    let max_h = (ui.available_height() * 0.5).max(row_h * 3.0);

    let mut clicked = None;
    egui::Panel::bottom(egui::Id::new(("agents_pane", tab)))
        .resizable(true)
        .default_size(want.min(max_h))
        .size_range(row_h * 1.5..=max_h)
        .frame(
            egui::Frame::NONE
                .fill(chrome.panel)
                .inner_margin(egui::Margin::symmetric(8, 5)),
        )
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    clicked = draw_table(ui, &table, &mut view.selected, chrome, &font, cw, row_h);
                });
        });
    clicked
}

fn draw_table(
    ui: &mut Ui,
    table: &Table,
    selected: &mut Option<String>,
    chrome: &Chrome,
    font: &FontId,
    cw: f32,
    row_h: f32,
) -> Option<RowClick> {
    let cols = ((ui.available_width() / cw).floor() as usize).max(40);
    let idw = table
        .lines
        .iter()
        .map(|l| l.id.chars().count())
        .max()
        .unwrap_or(0);
    let fixed = STAGE_W + GAP + EL_W + GAP + ETA_W + GAP + NOW_W + GAP + TOK_W;
    let taskw = cols.saturating_sub(fixed).max(MIN_TITLE);
    // Character offsets of each column.
    let x_task = STAGE_W + GAP;
    let x_el_end = x_task + taskw + GAP + EL_W;
    let x_eta_end = x_el_end + GAP + ETA_W;
    let x_now = x_eta_end + GAP;
    let x_tok_end = x_now + NOW_W + GAP + TOK_W;

    let mut clicked = None;
    for line in &table.lines {
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), Sense::click());
        let color = stage_color(line.stage);
        let ident = line.ident();
        let is_sel = selected.as_deref() == Some(ident.as_str());
        if is_sel {
            ui.painter()
                .rect_filled(rect, 2.0, color.gamma_multiply(0.22));
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(rect, 2.0, color.gamma_multiply(0.10));
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let x = |chars: usize| rect.left() + chars as f32 * cw;
        let y = rect.center().y;
        let p = ui.painter();
        let left = |at: usize, s: &str| {
            p.text(
                egui::pos2(x(at), y),
                Align2::LEFT_CENTER,
                s,
                font.clone(),
                color,
            );
        };
        let right = |end: usize, s: &str| {
            p.text(
                egui::pos2(x(end), y),
                Align2::RIGHT_CENTER,
                s,
                font.clone(),
                color,
            );
        };
        left(0, stage_word(line.stage));
        // The id is never cut; the title takes what is left.
        let task = if idw > 0 {
            let title_w = taskw.saturating_sub(idw + 1);
            format!("{:<idw$} {}", line.id, cut(&line.title, title_w))
        } else {
            cut(&line.title, taskw)
        };
        left(x_task, &task);
        right(x_el_end, &line.elapsed);
        right(x_eta_end, &line.eta);
        left(x_now, &cut(&line.now, NOW_W));
        right(x_tok_end, &line.tokens);

        let resp = match &line.click.note {
            Some(note) => resp.on_hover_text(note),
            None => resp,
        };
        if resp.clicked() {
            *selected = Some(ident);
            clicked = Some(line.click.clone());
        }
    }
    if let Some(total) = &table.total {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), Sense::hover());
        let x = |chars: usize| rect.left() + chars as f32 * cw;
        let y = rect.center().y;
        // Ledger style: `total:` right-aligned in the column left of TOKENS.
        ui.painter().text(
            egui::pos2(x(x_now + NOW_W), y),
            Align2::RIGHT_CENTER,
            "total:",
            font.clone(),
            chrome.fg,
        );
        ui.painter().text(
            egui::pos2(x(x_tok_end), y),
            Align2::RIGHT_CENTER,
            total,
            font.clone(),
            chrome.fg,
        );
    }
    if let Some(footer) = &table.footer {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), Sense::hover());
        ui.painter().text(
            egui::pos2(rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            cut(footer, cols),
            font.clone(),
            chrome.dim,
        );
    }
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_790_000_000_000; // an epoch-ms "now"

    fn feed(json: &str) -> Feed {
        feed::parse(json.as_bytes()).expect("test feed parses")
    }

    fn live(json: &str) -> Vec<SubagentRow> {
        let mut t = Tracker::new(None);
        t.apply_live(&LiveSnapshot::parse(json), T0);
        t.rows().to_vec()
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
        assert_eq!(t.total.as_deref(), Some("64.1k"));
        assert_eq!(l.click.agent_id.as_deref(), Some("a1"));
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
        assert_eq!(t.total.as_deref(), Some("6k"));
    }

    #[test]
    fn a_worker_holding_two_rows_is_dittoed_and_counted_once() {
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
        assert_eq!(t.total.as_deref(), Some("64.1k"));
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

    #[test]
    fn nothing_to_show_is_no_pane() {
        assert!(build(None, &[], T0).is_empty());
    }

    #[test]
    fn the_feed_footer_is_carried_verbatim() {
        let f = feed(r#"{"rows":[{"key":"k","stage":"planned"}],"footer":{"text":"session 3"}}"#);
        let t = build(Some(&f), &[], T0);
        assert_eq!(t.footer.as_deref(), Some("session 3"));
        assert_eq!(t.total, None);
    }

    #[test]
    fn cut_keeps_room_for_the_ellipsis() {
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abc", 4), "abc");
    }
}

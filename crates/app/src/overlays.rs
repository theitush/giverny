//! Overlay windows: the fuzzy tab palette (Ctrl+Shift+P), the past-session
//! picker (right-click a tab → sessions…), and the worker overlay an
//! agents-pane row opens.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Key, Modifiers, RichText};
use giverny_claude::registry::PastSession;
use giverny_claude::transcript;
use giverny_core::tabs::TabId;

use crate::agents_pane::RowClick;
use crate::{Action, App};

// ---- fuzzy tab palette -----------------------------------------------------

pub struct PaletteState {
    pub query: String,
    pub selected: usize,
    pub needs_focus: bool,
}

impl Default for PaletteState {
    fn default() -> Self {
        Self {
            query: String::new(),
            selected: 0,
            needs_focus: true,
        }
    }
}

/// Subsequence fuzzy match; higher is better, `None` = no match.
pub fn fuzzy_score(needle: &str, hay: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay_lc: Vec<char> = hay.to_lowercase().chars().collect();
    let mut score = 0i32;
    let mut pos = 0usize;
    let mut last_hit: Option<usize> = None;
    for nc in needle.to_lowercase().chars() {
        let found = hay_lc[pos..].iter().position(|&hc| hc == nc)?;
        let idx = pos + found;
        score += 2;
        if last_hit == Some(idx.wrapping_sub(1)) {
            score += 3; // consecutive run
        }
        if idx == 0 || !hay_lc[idx - 1].is_alphanumeric() {
            score += 2; // word start
        }
        last_hit = Some(idx);
        pos = idx + 1;
    }
    Some(score - (hay_lc.len() as i32 / 8))
}

pub fn palette_ui(app: &mut App, ctx: &egui::Context) -> Vec<Action> {
    let mut actions = Vec::new();
    let Some(mut st) = app.palette.take() else {
        return actions;
    };
    let c = app.chrome;

    let mut close = false;
    let mut commit = false;
    ctx.input_mut(|i| {
        if i.consume_key(Modifiers::NONE, Key::Escape) {
            close = true;
        }
        if i.consume_key(Modifiers::NONE, Key::Enter) {
            commit = true;
        }
        if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
            st.selected = st.selected.saturating_add(1);
        }
        if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
            st.selected = st.selected.saturating_sub(1);
        }
    });

    let mut items: Vec<(TabId, String, i32)> = app
        .ws
        .tabs
        .iter()
        .filter_map(|t| {
            let cat = app
                .ws
                .category(t.category)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            let cwd = t
                .cwd
                .as_deref()
                .map(|p| giverny_core::short_path(p, 28))
                .unwrap_or_default();
            let label = format!("{cat} › {}   {cwd}", t.display_title(&app.cfg.titles));
            fuzzy_score(&st.query, &label).map(|s| (t.id, label, s))
        })
        .collect();
    if !st.query.is_empty() {
        items.sort_by_key(|(_, _, s)| -s);
    }
    items.truncate(12);
    if !items.is_empty() {
        st.selected = st.selected.min(items.len() - 1);
    }

    if commit {
        if let Some((id, ..)) = items.get(st.selected) {
            actions.push(Action::Select(*id));
        }
        close = true;
    }

    egui::Window::new("giverny-palette")
        .title_bar(false)
        .resizable(false)
        .anchor(Align2::CENTER_TOP, [0.0, 90.0])
        .show(ctx, |ui| {
            ui.set_width(420.0);
            let te = ui.add(
                egui::TextEdit::singleline(&mut st.query)
                    .hint_text("jump to tab…")
                    .desired_width(f32::INFINITY)
                    .font(FontId::monospace(13.0)),
            );
            if st.needs_focus {
                te.request_focus();
                st.needs_focus = false;
            }
            if te.changed() {
                st.selected = 0;
            }
            ui.add_space(4.0);
            for (i, (id, label, _)) in items.iter().enumerate() {
                let resp = ui.selectable_label(
                    i == st.selected,
                    RichText::new(label).font(FontId::monospace(12.0)),
                );
                if resp.clicked() {
                    actions.push(Action::Select(*id));
                    close = true;
                }
            }
            if items.is_empty() {
                ui.label(
                    RichText::new("no matches")
                        .font(FontId::monospace(11.0))
                        .color(c.dim),
                );
            }
        });

    if !close {
        app.palette = Some(st);
    }
    actions
}

// ---- past-session picker ---------------------------------------------------

pub struct SessionPicker {
    pub tab: TabId,
    pub sessions: Vec<PastSession>,
}

pub fn sessions_ui(app: &mut App, ctx: &egui::Context) -> Vec<Action> {
    let mut actions = Vec::new();
    let Some(picker) = app.session_picker.take() else {
        return actions;
    };
    let c = app.chrome;

    let mut close = false;
    ctx.input_mut(|i| {
        if i.consume_key(Modifiers::NONE, Key::Escape) {
            close = true;
        }
    });

    egui::Window::new("giverny-sessions")
        .title_bar(false)
        .resizable(false)
        .anchor(Align2::CENTER_TOP, [0.0, 90.0])
        .show(ctx, |ui| {
            ui.set_width(460.0);
            ui.label(
                RichText::new("RESUME A CONVERSATION")
                    .font(FontId::monospace(10.0))
                    .color(c.dim),
            );
            ui.add_space(4.0);
            if picker.sessions.is_empty() {
                ui.label(
                    RichText::new("no past sessions in this directory")
                        .font(FontId::monospace(11.5))
                        .color(c.dim),
                );
            }
            for s in &picker.sessions {
                let age = s
                    .modified
                    .and_then(|m| m.elapsed().ok())
                    .map(humanize)
                    .unwrap_or_default();
                let account = giverny_claude::profiles::find(&app.claude.profiles, &s.config_dir)
                    .map(|p| format!("@{}", p.name))
                    .unwrap_or_default();
                let suffix = if s.live { "  · live" } else { "" };
                let label = format!("{:<44} {age:>4} {account}{suffix}", truncate(&s.title, 44));
                let text = RichText::new(label)
                    .font(FontId::monospace(11.5))
                    .color(if s.live {
                        c.dim
                    } else {
                        Color32::from_rgb(0xd7, 0xdd, 0xe2)
                    });
                let resp = ui
                    .add_enabled_ui(!s.live, |ui| ui.selectable_label(false, text))
                    .inner;
                if s.live {
                    resp.on_hover_text("already open in another terminal");
                } else if resp.clicked() {
                    actions.push(Action::ResumeSpecific(
                        picker.tab,
                        s.id.clone(),
                        s.config_dir.clone(),
                    ));
                    close = true;
                }
            }
            ui.add_space(2.0);
            ui.label(
                RichText::new("esc to close")
                    .font(FontId::monospace(9.5))
                    .color(c.amber),
            );
        });

    if !close {
        app.session_picker = Some(picker);
    }
    actions
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max - 1).collect::<String>() + "…"
}

fn humanize(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

// ---- worker overlay (agents pane: a row's brief or transcript) -------------

/// Read-only text over the terminal session (giverny#5, #41, #44): a
/// Planned row's brief, a Running worker's transcript followed live, a Done
/// worker's transcript opened at its final report, or what a row that has
/// nothing to open says instead of doing nothing.
///
/// It sits inside the terminal session's rect — never over the rail — with
/// a bare `✕` in its header, the whole text in a scroll area below, and the
/// row's action (Revive, Open in Claude Code) in its footer. While it is
/// open it takes the keyboard: `Esc` closes it, the scroll keys scroll it,
/// and nothing typed reaches the shell underneath.
pub struct BriefOverlay {
    pub title: String,
    /// Where the text came from, shown under the title when it is a file.
    pub source: Option<PathBuf>,
    /// The row's state in words (landing, timing, tokens).
    pub facts: Vec<String>,
    /// A Done row's task's Review line, once fetched (giverny#60): drawn at
    /// the top, set apart, because it is what a person has to read.
    pub review: Option<crate::review::Slot>,
    pub content: Content,
    pub button: Button,
    /// Bumped whenever the text changes; the laid-out text is cached on it.
    generation: u64,
    galley: Option<(u64, u32, std::sync::Arc<egui::Galley>)>,
    /// A scroll the keyboard asked for, applied next frame.
    scroll_to: Option<f32>,
    /// The scroll area's offset and its visible height, last frame.
    offset: f32,
    page: f32,
}

pub enum Content {
    Text(String),
    Transcript(TranscriptView),
}

/// A worker's transcript as the overlay shows it.
pub struct TranscriptView {
    pub path: PathBuf,
    /// Follow it as it grows (a Running worker).
    pub live: bool,
    tail: transcript::Tail,
    rows: Vec<transcript::Row>,
    /// Why the file could not be read, when it could not.
    error: Option<String>,
    polled: Instant,
}

/// What the overlay's footer offers.
pub enum Button {
    None,
    /// A Running worker: open it in the parent's Claude Code.
    Open {
        tab: TabId,
        click: Box<RowClick>,
    },
    /// A Done worker: submit `line` at the parent's prompt.
    Revive {
        tab: TabId,
        line: String,
    },
    /// The action this row would have, and why it cannot be taken.
    Disabled {
        label: &'static str,
        why: String,
    },
}

/// How often a live transcript is looked at.
const POLL: Duration = Duration::from_millis(300);
/// Space between the overlay and the session rect's edges.
const MARGIN: f32 = 14.0;

impl BriefOverlay {
    /// Plain text: a brief, a note, or why something could not be done.
    pub fn text(title: String, source: Option<PathBuf>, text: String) -> Self {
        Self::with(title, source, Content::Text(text))
    }

    /// A worker's transcript, read now; `live` keeps following it.
    pub fn transcript(title: String, path: PathBuf, live: bool) -> Self {
        let mut view = TranscriptView {
            path,
            live,
            tail: transcript::Tail::default(),
            rows: Vec::new(),
            error: None,
            polled: Instant::now(),
        };
        view.poll();
        Self::with(title, None, Content::Transcript(view))
    }

    fn with(title: String, source: Option<PathBuf>, content: Content) -> Self {
        BriefOverlay {
            title,
            source,
            facts: Vec::new(),
            review: None,
            content,
            button: Button::None,
            generation: 0,
            galley: None,
            // Open at the end: a transcript's final report, a live one's
            // latest turn. A brief opens at its top.
            scroll_to: None,
            offset: 0.0,
            page: 0.0,
        }
        .opened()
    }

    fn opened(mut self) -> Self {
        if matches!(self.content, Content::Transcript(_)) {
            self.scroll_to = Some(f32::INFINITY);
        }
        self
    }

    /// The text as plain lines, as the overlay shows it.
    pub fn plain_text(&self) -> String {
        match &self.content {
            Content::Text(t) => t.clone(),
            Content::Transcript(v) => v
                .lines_for_view()
                .iter()
                .map(|r| r.plain())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

impl TranscriptView {
    /// Read what was appended. True when the rows changed.
    fn poll(&mut self) -> bool {
        self.polled = Instant::now();
        match self.tail.read(&self.path) {
            Ok(update) => {
                let was_error = self.error.take().is_some();
                if update.restarted {
                    self.rows.clear();
                }
                let before = self.rows.len();
                for line in &update.lines {
                    self.rows
                        .extend(transcript::render_rows(line, transcript::Fold::OVERLAY));
                }
                was_error || update.restarted || self.rows.len() != before
            }
            Err(err) => {
                let msg = format!("Could not read {}: {err}", self.path.display());
                let changed = self.error.as_deref() != Some(msg.as_str());
                self.error = Some(msg);
                changed
            }
        }
    }

    /// The rows, and for a finished transcript its unterminated last line.
    fn lines_for_view(&self) -> Vec<transcript::Row> {
        let mut rows = self.rows.clone();
        if !self.live
            && let Some(rest) = self.tail.rest()
        {
            rows.extend(transcript::render_rows(rest, transcript::Fold::OVERLAY));
        }
        if let Some(err) = &self.error {
            rows.push(transcript::Row {
                indent: "",
                clock: None,
                tone: transcript::Tone::Error,
                text: err.clone(),
            });
        } else if rows.is_empty() {
            rows.push(transcript::Row {
                indent: "",
                clock: None,
                tone: transcript::Tone::Dim,
                text: if self.live {
                    "waiting for the worker's first turn…".into()
                } else {
                    "(the transcript is empty)".into()
                },
            });
        }
        rows
    }
}

/// The overlay's keyboard, taken before the terminal sees it (called from
/// `App::shortcuts`): `Esc` closes, `r` revives, `o` opens in Claude Code,
/// the scroll keys scroll, and every other plain key or typed text is
/// swallowed so it never reaches the shell under the overlay (giverny#20).
/// Ctrl/Alt chords pass through to Giverny's own shortcuts.
pub fn brief_keys(app: &mut App, ctx: &egui::Context) -> Vec<Action> {
    let mut actions = Vec::new();
    let Some(ov) = app.brief.as_mut() else {
        return actions;
    };
    let (act, close) = overlay_keys(ov, ctx);
    if act {
        actions.extend(button_action(ov));
    }
    if act || close {
        app.brief = None;
        app.focus_terminal = true;
    }
    actions
}

/// [`brief_keys`] on the overlay alone: (the footer's key was pressed,
/// the overlay closes).
fn overlay_keys(ov: &mut BriefOverlay, ctx: &egui::Context) -> (bool, bool) {
    let mut close = false;
    let mut act = false;
    let line = 40.0;
    let page = (ov.page - 2.0 * line).max(line);
    ctx.input_mut(|i| {
        i.events.retain(|e| match e {
            egui::Event::Key {
                key,
                pressed,
                modifiers,
                ..
            } if !(modifiers.ctrl || modifiers.alt || modifiers.command || modifiers.mac_cmd) => {
                if *pressed {
                    let to = match key {
                        Key::Escape => {
                            close = true;
                            None
                        }
                        Key::R | Key::O => {
                            let wants_revive = *key == Key::R;
                            act |= matches!(
                                (&ov.button, wants_revive),
                                (Button::Revive { .. }, true) | (Button::Open { .. }, false)
                            );
                            None
                        }
                        Key::ArrowUp => Some(ov.offset - line),
                        Key::ArrowDown => Some(ov.offset + line),
                        Key::PageUp => Some(ov.offset - page),
                        Key::PageDown | Key::Space => Some(ov.offset + page),
                        Key::Home => Some(0.0),
                        Key::End => Some(f32::INFINITY),
                        _ => None,
                    };
                    if let Some(to) = to {
                        ov.scroll_to = Some(to.max(0.0));
                    }
                }
                false
            }
            egui::Event::Text(_) | egui::Event::Paste(_) => false,
            _ => true,
        });
    });
    (act, close)
}

/// The footer button's action.
fn button_action(ov: &BriefOverlay) -> Option<Action> {
    match &ov.button {
        Button::Open { tab, click } => Some(Action::OpenWorkerInClaude(*tab, click.clone())),
        Button::Revive { tab, line } => Some(Action::ReviveWorker(*tab, line.clone())),
        Button::None | Button::Disabled { .. } => None,
    }
}

/// Where the overlay goes: the terminal session's rect, less a margin —
/// never over the rail. Before the session has been laid out, the window.
fn overlay_rect(session: Option<egui::Rect>, ctx: &egui::Context) -> egui::Rect {
    let area = session.unwrap_or_else(|| ctx.content_rect());
    let r = area.shrink(MARGIN);
    if r.width() < 240.0 || r.height() < 160.0 {
        area
    } else {
        r
    }
}

fn tone_color(c: &crate::chrome::Chrome, tone: transcript::Tone) -> Color32 {
    use transcript::Tone;
    match tone {
        Tone::Plain => c.fg,
        Tone::Reply => c.accent,
        Tone::Tool => c.green,
        Tone::Prompt => c.amber,
        Tone::Dim => c.dim,
        Tone::Error => c.poppy,
    }
}

fn layout_body(ov: &BriefOverlay, c: &crate::chrome::Chrome, width: f32) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let font = FontId::monospace(11.5);
    let fmt = |color: Color32| TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    match &ov.content {
        Content::Text(t) => job.append(t, 0.0, fmt(c.fg)),
        Content::Transcript(v) => {
            for (i, row) in v.lines_for_view().iter().enumerate() {
                if i > 0 {
                    job.append("\n", 0.0, fmt(c.fg));
                }
                job.append(row.indent, 0.0, fmt(c.fg));
                if let Some(at) = &row.clock {
                    job.append(&format!("{at} "), 0.0, fmt(c.dim));
                }
                job.append(&row.text, 0.0, fmt(tone_color(c, row.tone)));
            }
        }
    }
    job
}

pub fn brief_ui(app: &mut App, ctx: &egui::Context) -> Vec<Action> {
    let mut actions = Vec::new();
    let Some(ov) = app.brief.as_mut() else {
        return actions;
    };
    let drawn = draw_overlay(ov, &app.chrome, app.session_rect, ctx);
    if drawn.act {
        actions.extend(button_action(ov));
    }
    if drawn.act || drawn.close {
        app.brief = None;
        app.focus_terminal = true;
    }
    actions
}

/// Where the overlay's parts were drawn, and what was asked of it.
#[derive(Debug)]
struct Drawn {
    act: bool,
    close: bool,
    // The rects are read by the layout tests.
    #[cfg_attr(not(test), allow(dead_code))]
    frame: egui::Rect,
    #[cfg_attr(not(test), allow(dead_code))]
    close_button: egui::Rect,
    #[cfg_attr(not(test), allow(dead_code))]
    body: egui::Rect,
}

/// The Review line, boxed in amber under the title: the first thing read.
fn draw_review(ui: &mut egui::Ui, c: &crate::chrome::Chrome, line: &str) {
    ui.add_space(4.0);
    egui::Frame::new()
        .fill(c.amber.gamma_multiply(0.12))
        .stroke(egui::Stroke::new(1.0, c.amber))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut job = egui::text::LayoutJob::default();
            job.append(
                "Review  ",
                0.0,
                egui::TextFormat::simple(FontId::monospace(12.5), c.amber),
            );
            job.append(
                line,
                0.0,
                egui::TextFormat::simple(FontId::monospace(12.5), c.fg),
            );
            job.wrap.max_width = ui.available_width();
            ui.add(egui::Label::new(job).selectable(true).wrap());
        });
    ui.add_space(4.0);
}

fn draw_overlay(
    ov: &mut BriefOverlay,
    c: &crate::chrome::Chrome,
    session: Option<egui::Rect>,
    ctx: &egui::Context,
) -> Drawn {
    let rect = overlay_rect(session, ctx);
    let c = *c;
    // Esc is consumed in `brief_keys` before the terminal sees it; this
    // catches the frame the overlay opened on.
    let mut close = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
    let mut close_button = egui::Rect::NOTHING;
    let mut body_rect = egui::Rect::NOTHING;

    if let Content::Transcript(v) = &mut ov.content
        && v.live
    {
        if v.polled.elapsed() >= POLL && v.poll() {
            ov.generation += 1;
        }
        ctx.request_repaint_after(POLL);
    }

    let mut act = false;
    let shown = egui::Area::new(egui::Id::new("giverny-brief"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .constrain(false)
        .show(ctx, |ui| {
            egui::Frame::window(ui.style())
                .fill(c.panel)
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    let inner = rect.size() - egui::vec2(20.0, 20.0);
                    ui.set_min_size(inner);
                    ui.set_max_size(inner);

                    // Header: the ✕ first, at the right, then the title in
                    // what is left — so it can never sit over text.
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let x = ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("✕").font(FontId::proportional(14.0)),
                                    )
                                    .frame(false),
                                )
                                .on_hover_text("close (esc)");
                            close_button = x.rect;
                            if x.clicked() {
                                close = true;
                            }
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    let live =
                                        matches!(&ov.content, Content::Transcript(v) if v.live);
                                    if live {
                                        ui.label(
                                            RichText::new("● live")
                                                .font(FontId::monospace(10.5))
                                                .color(c.green),
                                        );
                                    }
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&ov.title)
                                                .font(FontId::monospace(12.5))
                                                .color(c.accent),
                                        )
                                        .truncate(),
                                    );
                                },
                            );
                        });
                    });
                    let review = ov
                        .review
                        .as_ref()
                        .and_then(|s| s.lock().ok().and_then(|g| g.clone()));
                    if let Some(line) = review {
                        draw_review(ui, &c, &line);
                    }
                    let sub = match (&ov.source, &ov.content) {
                        (Some(src), _) => Some(src.display().to_string()),
                        (None, Content::Transcript(v)) => Some(v.path.display().to_string()),
                        _ => None,
                    };
                    if !ov.facts.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(ov.facts.join("  ·  "))
                                    .font(FontId::monospace(11.0))
                                    .color(c.fg),
                            )
                            .truncate(),
                        );
                    }
                    if let Some(sub) = sub {
                        ui.add(
                            egui::Label::new(
                                RichText::new(sub)
                                    .font(FontId::monospace(10.0))
                                    .color(c.dim),
                            )
                            .truncate(),
                        );
                    }
                    ui.separator();

                    // Footer height is reserved first so the body fills the
                    // rest exactly.
                    let footer_h = 26.0;
                    let body_h = (ui.available_height() - footer_h - 8.0).max(60.0);
                    let mut area = egui::ScrollArea::vertical()
                        .id_salt("giverny-brief-body")
                        .max_height(body_h)
                        .min_scrolled_height(body_h)
                        .auto_shrink([false, false])
                        .stick_to_bottom(matches!(&ov.content, Content::Transcript(_)));
                    if let Some(to) = ov.scroll_to.take() {
                        area = area.vertical_scroll_offset(to.min(1.0e7));
                    }
                    let out = area.show(ui, |ui| {
                        let width = ui.available_width();
                        let key = (ov.generation, width.to_bits());
                        let galley = match &ov.galley {
                            Some((g, w, galley)) if (*g, *w) == key => galley.clone(),
                            _ => {
                                let job = layout_body(ov, &c, width);
                                let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
                                ov.galley = Some((key.0, key.1, galley.clone()));
                                galley
                            }
                        };
                        ui.add(egui::Label::new(galley).selectable(true));
                    });
                    ov.offset = out.state.offset.y;
                    ov.page = out.inner_rect.height();
                    body_rect = out.inner_rect;

                    ui.separator();
                    ui.horizontal(|ui| {
                        match &ov.button {
                            Button::None => {}
                            Button::Open { .. } => {
                                if ui
                                    .button(
                                        RichText::new("Open in Claude Code  (o)")
                                            .font(FontId::monospace(11.0)),
                                    )
                                    .on_hover_text(
                                        "switch this tab's Claude Code to the worker's own view",
                                    )
                                    .clicked()
                                {
                                    act = true;
                                }
                            }
                            Button::Revive { line, .. } => {
                                if ui
                                    .button(
                                        RichText::new("Revive  (r)").font(FontId::monospace(11.0)),
                                    )
                                    .on_hover_text(format!(
                                        "send to this tab's Claude Code:\n{line}"
                                    ))
                                    .clicked()
                                {
                                    act = true;
                                }
                            }
                            Button::Disabled { label, why } => {
                                ui.add_enabled(
                                    false,
                                    egui::Button::new(
                                        RichText::new(*label).font(FontId::monospace(11.0)),
                                    ),
                                )
                                .on_disabled_hover_text(why.as_str());
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(why.as_str())
                                            .font(FontId::monospace(10.0))
                                            .color(c.dim),
                                    )
                                    .truncate(),
                                );
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new("read-only · ↑↓ PgUp PgDn scroll · esc close")
                                    .font(FontId::monospace(10.0))
                                    .color(c.dim),
                            );
                        });
                    });
                });
        });

    Drawn {
        act,
        close,
        frame: shown.response.rect,
        close_button,
        body: body_rect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_prefers_word_starts_and_runs() {
        assert!(fuzzy_score("", "anything").is_some());
        assert!(fuzzy_score("xyz", "abc").is_none());
        let exact = fuzzy_score("api", "work › api server").unwrap();
        let scattered = fuzzy_score("api", "a-thing-with-p-and-i").unwrap();
        assert!(exact > scattered, "{exact} vs {scattered}");
    }

    // ---- worker overlay, headless -----------------------------------------

    use crate::chrome::Chrome;
    use giverny_term::render::theme::Theme;

    fn chrome() -> Chrome {
        Chrome::from_theme(&Theme::monet_dark())
    }

    /// One frame at 1280×820, with `events` as this frame's input.
    fn frame(
        ctx: &egui::Context,
        ov: &mut BriefOverlay,
        session: egui::Rect,
        events: Vec<egui::Event>,
    ) -> (bool, bool, Option<Drawn>, Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 820.0),
            )),
            events,
            ..Default::default()
        };
        let c = chrome();
        let mut keys = (false, false);
        let mut drawn = None;
        let mut left = Vec::new();
        let _ = ctx.run_ui(input, |ui| {
            let ctx = ui.ctx().clone();
            keys = overlay_keys(ov, &ctx);
            // What is left is what the terminal underneath would get.
            left = ctx.input(|i| i.events.clone());
            drawn = Some(draw_overlay(ov, &c, Some(session), &ctx));
        });
        (keys.0, keys.1, drawn, left)
    }

    fn key(k: Key) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    /// The session's rect: right of a 240px rail, above a pane.
    fn session() -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(248.0, 12.0), egui::pos2(1272.0, 640.0))
    }

    fn long_text() -> String {
        (1..=400).map(|i| format!("brief line {i}\n")).collect()
    }

    #[test]
    fn the_overlay_sits_in_the_session_with_the_x_above_the_body() {
        let ctx = egui::Context::default();
        let mut ov = BriefOverlay::text(
            "coo#158 · a title long enough to reach the corner of the overlay header, and then \
             some more of it so it would run under the close button if it could"
                .into(),
            Some("/b/brief.md".into()),
            long_text(),
        );
        let mut drawn = None;
        for _ in 0..3 {
            drawn = frame(&ctx, &mut ov, session(), vec![]).2;
        }
        let d = drawn.unwrap();
        let s = session();
        // Inside the session rect, never over the rail, centred on it.
        assert!(s.contains_rect(d.frame), "{:?} not in {s:?}", d.frame);
        assert!(d.frame.min.x >= 248.0);
        assert!(
            (d.frame.center().x - s.center().x).abs() < 2.0,
            "{:?}",
            d.frame
        );
        assert!(
            (d.frame.center().y - s.center().y).abs() < 2.0,
            "{:?}",
            d.frame
        );
        assert!(d.frame.width() > s.width() - 40.0, "covers the session");
        // The ✕ is in the header's top-right, above the body, not over it.
        assert!(d.frame.contains_rect(d.close_button));
        assert!(
            d.close_button.max.y <= d.body.min.y,
            "{:?} {:?}",
            d.close_button,
            d.body
        );
        assert!(d.close_button.min.x > d.frame.center().x);
        // The body scrolls: the whole text is there, taller than the view.
        assert!(d.body.height() > 300.0);
        assert!(ov.plain_text().contains("brief line 400"));
        // It follows the session rect on resize.
        let small = egui::Rect::from_min_max(egui::pos2(300.0, 20.0), egui::pos2(900.0, 500.0));
        let d = frame(&ctx, &mut ov, small, vec![]).2.unwrap();
        assert!(
            small.contains_rect(d.frame),
            "{:?} not in {small:?}",
            d.frame
        );
    }

    #[test]
    fn the_overlay_takes_the_keyboard() {
        let ctx = egui::Context::default();
        let mut ov = BriefOverlay::text("t".into(), None, long_text());
        ov.button = Button::Revive {
            tab: TabId(3),
            line: "revive worker (agent a1): continue where you left off".into(),
        };
        frame(&ctx, &mut ov, session(), vec![]);
        // Typing does not reach the terminal; Ctrl chords (Giverny's own
        // shortcuts) still do.
        let ctrl_t = egui::Event::Key {
            key: Key::T,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::CTRL | Modifiers::SHIFT,
        };
        let (act, close, _, left) = frame(
            &ctx,
            &mut ov,
            session(),
            vec![key(Key::X), egui::Event::Text("x".into()), ctrl_t.clone()],
        );
        assert!(!act && !close);
        assert_eq!(left, vec![ctrl_t]);
        // PageDown scrolls it.
        let before = ov.offset;
        frame(&ctx, &mut ov, session(), vec![key(Key::PageDown)]);
        frame(&ctx, &mut ov, session(), vec![]);
        assert!(ov.offset > before + 100.0, "{before} → {}", ov.offset);
        frame(&ctx, &mut ov, session(), vec![key(Key::Home)]);
        frame(&ctx, &mut ov, session(), vec![]);
        assert_eq!(ov.offset, 0.0);
        // `o` is not this overlay's key; `r` is, and its text is eaten too.
        let (act, _, _, left) = frame(
            &ctx,
            &mut ov,
            session(),
            vec![key(Key::O), egui::Event::Text("o".into())],
        );
        assert!(!act);
        assert!(left.is_empty());
        let (act, _, _, left) = frame(
            &ctx,
            &mut ov,
            session(),
            vec![key(Key::R), egui::Event::Text("r".into())],
        );
        assert!(act);
        assert!(left.is_empty());
        assert!(matches!(
            button_action(&ov),
            Some(Action::ReviveWorker(TabId(3), l)) if l.starts_with("revive worker")
        ));
        // Esc closes.
        let (_, close, _, left) = frame(&ctx, &mut ov, session(), vec![key(Key::Escape)]);
        assert!(close);
        assert!(left.is_empty());
    }

    #[test]
    fn a_disabled_button_does_nothing() {
        let ctx = egui::Context::default();
        let mut ov = BriefOverlay::text("t".into(), None, "x".into());
        ov.button = Button::Disabled {
            label: "Revive",
            why: "no worker id".into(),
        };
        let (act, ..) = frame(&ctx, &mut ov, session(), vec![key(Key::R)]);
        assert!(!act);
        assert!(button_action(&ov).is_none());
    }

    fn line(text: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": text}]},
        })
        .to_string()
            + "\n"
    }

    #[test]
    fn a_done_transcript_opens_at_its_end() {
        let dir = std::env::temp_dir().join(format!("giverny-ov-done-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-d.jsonl");
        let mut body = String::new();
        for i in 0..300 {
            body.push_str(&line(&format!("step {i}")));
        }
        body.push_str(&line("FINAL REPORT"));
        std::fs::write(&path, body).unwrap();
        let ctx = egui::Context::default();
        let mut ov = BriefOverlay::transcript("t".into(), path, false);
        assert!(ov.plain_text().ends_with("FINAL REPORT"));
        let mut d = None;
        for _ in 0..3 {
            d = frame(&ctx, &mut ov, session(), vec![]).2;
        }
        // Scrolled to the bottom: the final report is what is on screen.
        assert!(ov.offset > 1000.0, "offset {}", ov.offset);
        assert!(d.unwrap().body.height() > 300.0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_running_transcript_follows_as_it_grows() {
        let dir = std::env::temp_dir().join(format!("giverny-ov-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-r.jsonl");
        std::fs::write(&path, line("first turn")).unwrap();
        let ctx = egui::Context::default();
        let mut ov = BriefOverlay::transcript("t".into(), path.clone(), true);
        frame(&ctx, &mut ov, session(), vec![]);
        assert!(ov.plain_text().contains("first turn"));
        assert!(!ov.plain_text().contains("second turn"));
        let mut more = line("first turn");
        for i in 0..200 {
            more.push_str(&line(&format!("second turn {i}")));
        }
        std::fs::write(&path, &more).unwrap();
        std::thread::sleep(POLL + Duration::from_millis(20));
        for _ in 0..3 {
            frame(&ctx, &mut ov, session(), vec![]);
        }
        assert!(ov.plain_text().contains("second turn 199"));
        // It stuck to the bottom as it grew.
        assert!(ov.offset > 1000.0, "offset {}", ov.offset);
        std::fs::remove_dir_all(&dir).ok();
    }
}

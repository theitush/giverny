//! A worker Claude Code cannot show, shown in the tab as Claude Code shows
//! a running one (giverny#477).
//!
//! A click on a management-panel row whose worker is mid-turn walks the
//! tab's Claude Code into that worker's own view (`agent_open`). A worker
//! whose turn has ended (finished, or asleep on a background shell of its
//! own, #476), a Done row's, and a Next up row's that has not started have no
//! such view: Claude Code keeps none. For those Giverny draws the view
//! itself, and it takes the tab over exactly as the running view does: it
//! fills the terminal's area in the terminal's own grid, glyphs and worker
//! background ([`giverny_term::page::Page`]), under the same header
//! (`view_header`), with the same "back to manager" button where the
//! running view has it, the same Esc, and a click on the row going back.
//!
//! What is drawn is what Claude Code draws for a worker (as captured from
//! Claude Code 2.1.308, `.claude/review/477/r2/`): its banner's rows (here
//! blank, under the header), the worker's prompt on its grey band and each
//! entry under it ([`transcript`]), then the prompt box, its rules in the
//! worker's colour with its name on the top one, and the button under it.
//! No key reaches the shell under the page. Claude Code keeps no view of
//! such a worker, so nothing can be typed to it there; what is typed in
//! the page's prompt box goes to its manager instead (giverny#483), which
//! wakes the worker with it (`SendMessage`), or for a Next up row starts
//! the task with it added to the brief ([`agent_open::Ask`]). The box
//! takes only what the user types, and a line goes only on their Enter.
//! A page with nobody to send to (a note, a worker still mid-turn) keeps
//! the box dimmed and `read-only`.
//!
//! A worker still writing (asleep, or one whose view could not be walked
//! to) is followed as it grows; a finished one opens at its end; a brief
//! (Next up), drawn as the worker's prompt, opens at its top.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Key, Rect};
use giverny_claude::transcript::{self, Row, Tone};
use giverny_core::tabs::TabId;
use giverny_term::page::Page;
use giverny_term::widget::{RenderShared, RowMarks};

use crate::agent_open::Ask;
use crate::management_panel::RowClick;
use crate::{Action, App};

/// How often a live transcript is looked at.
const POLL: Duration = Duration::from_millis(300);

/// Rows Claude Code's view starts its entries under: its banner's three,
/// then two blank (Claude Code 2.1.308).
const BANNER_ROWS: usize = 5;

/// The column the running view's "back to manager" button is drawn from:
/// where the strip's `◯ main` dot was (`agent_open::row_marks`).
const BUTTON_COL: u16 = 2;

/// The worker's colour on its prompt box's rules and name tag: Claude
/// Code's for a general-purpose worker, as captured.
const WORKER_RGB: (u8, u8, u8) = (8, 145, 178);

const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

pub struct WorkerPage {
    /// The tab it is drawn in, in place of the terminal.
    pub tab: TabId,
    /// The management-panel row it shows; `None` for a message.
    pub row: Option<RowClick>,
    /// What the header names when the pane has no row for it.
    pub title: String,
    /// The worker's name on its prompt box: the task's.
    pub name: String,
    pub content: Content,
    /// The header's `×` was clicked: it stays hidden for this page.
    pub header_closed: bool,
    /// The header's height last frame, in points: the rows under it are
    /// the banner's.
    pub header_h: f32,
    page: Page,
    /// Bumped whenever the text changes.
    generation: u64,
    /// What the page was last laid out for: (generation, cols, rows, pad).
    laid: Option<(u64, u16, u16, usize)>,
    /// Open at the top rather than at the end.
    at_top: bool,
    /// The line of the text the button is on.
    button_line: usize,
    /// Where the button was drawn last frame.
    pub button_rect: Option<Rect>,
    /// The prompt box, when a line typed in it goes to the manager.
    input: Option<Input>,
}

/// The page's prompt box while it takes a line for the manager
/// (giverny#483).
struct Input {
    ask: Ask,
    text: String,
    /// The cursor, in chars into `text`.
    cursor: usize,
    state: Sending,
}

/// Where the box's last line is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sending {
    /// Nothing sent yet, or the last line went and another may follow.
    Idle,
    /// Being put at the manager's prompt: the box waits.
    Going,
    /// At the manager's prompt, submitted: what is waited for now.
    Sent(String),
    /// Not sent, and why: the line is back in the box.
    Refused(String),
}

pub enum Content {
    Text(String),
    /// Rows drawn as a transcript is: a Next up row's brief, as the
    /// worker's prompt.
    Rows(Vec<Row>),
    Transcript(TranscriptView),
}

/// A worker's transcript as the page shows it.
pub struct TranscriptView {
    pub path: PathBuf,
    /// Follow it as it grows.
    pub live: bool,
    tail: transcript::Tail,
    render: transcript::Render,
    rows: Vec<Row>,
    /// Why the file could not be read, when it could not.
    error: Option<String>,
    polled: Instant,
}

/// What a frame of the page did.
pub struct Drawn {
    /// The "back to manager" button was clicked.
    pub back: bool,
    /// Where the page is: the terminal's area.
    pub rect: Rect,
}

impl WorkerPage {
    /// Plain text: a note, or why something could not be done.
    pub fn text(tab: TabId, title: String, text: String) -> Self {
        Self::with(tab, title, Content::Text(text)).at_top()
    }

    /// A worker's transcript, read now; `live` keeps following it.
    pub fn transcript(tab: TabId, title: String, path: PathBuf, live: bool) -> Self {
        let mut view = TranscriptView {
            path,
            live,
            tail: transcript::Tail::default(),
            render: transcript::Render::new(transcript::Fold::OVERLAY),
            rows: Vec::new(),
            error: None,
            polled: Instant::now(),
        };
        view.poll();
        Self::with(tab, title, Content::Transcript(view))
    }

    /// Rows in the transcript's look: a brief as the prompt it will be.
    pub fn rows(tab: TabId, title: String, rows: Vec<Row>) -> Self {
        Self::with(tab, title, Content::Rows(rows)).at_top()
    }

    fn with(tab: TabId, title: String, content: Content) -> Self {
        WorkerPage {
            tab,
            row: None,
            name: title.clone(),
            title,
            content,
            header_closed: false,
            header_h: 0.0,
            page: Page::new(),
            generation: 0,
            laid: None,
            at_top: false,
            button_line: 0,
            button_rect: None,
            input: None,
        }
    }

    /// The page's box takes a line for the manager: `ask` says what the
    /// line asks of it.
    pub fn with_input(mut self, ask: Option<Ask>) -> Self {
        self.input = ask.map(|ask| Input {
            ask,
            text: String::new(),
            cursor: 0,
            state: Sending::Idle,
        });
        self
    }

    /// What a line typed in the box asks of the manager, if one can be.
    pub fn ask(&self) -> Option<&Ask> {
        self.input.as_ref().map(|i| &i.ask)
    }

    /// The box's line is on its way, or went, or was refused: the box says
    /// so under it. A refused line is put back in the box to send again.
    pub fn set_sending(&mut self, state: Sending, text: Option<&str>) {
        let Some(input) = &mut self.input else {
            return;
        };
        if let Some(text) = text {
            input.text = text.to_string();
            input.cursor = text.chars().count();
        }
        input.state = state;
        self.generation += 1;
    }

    fn at_top(mut self) -> Self {
        self.at_top = true;
        self
    }

    /// The page shows this row, under its task's name.
    pub fn of_row(mut self, row: RowClick, name: String) -> Self {
        self.row = Some(row);
        self.name = name;
        self
    }

    /// The entries as plain lines, as the page shows them.
    #[cfg(test)]
    pub fn plain_text(&self) -> String {
        let rows: Vec<Row> = self.entries().into_iter().map(no_clock).collect();
        transcript::lay_out(&rows, true, transcript::Style { color: false }).join("\n")
    }

    /// What the page draws between the banner and the prompt box.
    fn entries(&self) -> Vec<Row> {
        match &self.content {
            Content::Text(t) => t
                .lines()
                .map(|l| Row::one(Tone::Plain, format!("  {l}")))
                .collect(),
            Content::Rows(rows) => rows.clone(),
            Content::Transcript(v) => v.lines_for_view(),
        }
    }

    /// The page's text for a grid `cols` wide with `pad` rows under the
    /// header: its lines, and the line the button goes on.
    fn lines(&self, cols: u16, pad: usize) -> (Vec<String>, usize) {
        let cols = usize::from(cols).max(8);
        let mut out = vec![String::new(); pad];
        let mut wrapped: Vec<Row> = Vec::new();
        for row in self.entries() {
            for mut r in transcript::wrap(&no_clock(row), cols - 1) {
                if r.is_prompt() {
                    // The band runs the grid's width, as Claude Code's does.
                    let n = r.plain().chars().count();
                    if n < cols {
                        r.spans.push(transcript::Span {
                            tone: Tone::Prompt,
                            text: " ".repeat(cols - n),
                        });
                    }
                }
                wrapped.push(r);
            }
        }
        let style = transcript::Style { color: true };
        out.extend(transcript::lay_out(&wrapped, true, style));
        out.push(String::new());
        match &self.input {
            Some(input) => {
                out.extend(input_box(cols, &self.name, input));
                out.extend(status_lines(input, cols));
            }
            None => {
                out.extend(prompt_box(cols, &self.name));
                out.push(String::new());
            }
        }
        // The button's row: blank, the button drawn over it.
        out.push(String::new());
        let button = out.len() - 1;
        (out, button)
    }

    /// Draw the page over the rest of `ui`, in the tab's terminal's place.
    /// `labels` and `fill` are the terminal's for its "back to manager"
    /// button.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        shared: &mut RenderShared,
        labels: &'static [&'static str],
        fill: Color32,
    ) -> Drawn {
        if let Content::Transcript(v) = &mut self.content
            && v.live
        {
            if v.polled.elapsed() >= POLL && v.poll() {
                self.generation += 1;
            }
            ui.ctx().request_repaint_after(POLL);
        }
        let rect = ui.available_rect_before_wrap();
        let ppp = ui.ctx().pixels_per_point();
        let (cols, rows) = Page::grid(shared, rect, ppp);
        let cell_h = shared.cell_size(ppp).y.max(1.0);
        // The rows under the header, at least the banner's.
        let under = if self.header_h > 0.0 {
            (self.header_h / cell_h).ceil() as usize + 1
        } else {
            0
        };
        let pad = BANNER_ROWS.max(under);
        let key = (self.generation, cols, rows, pad);
        if self.laid != Some(key) {
            let (lines, button) = self.lines(cols, pad);
            self.page.set(lines, cols, rows);
            if self.laid.is_none() && self.at_top {
                self.page.to_top();
            }
            self.button_line = button;
            self.laid = Some(key);
        }
        let marks = RowMarks {
            button: self.page.screen_row(self.button_line).map(|r| (r, 0)),
            button_left: Some(BUTTON_COL),
            ..Default::default()
        };
        let bg = shared.theme.worker_bg();
        let shown = self.page.show(ui, shared, bg, (&marks, labels, fill));
        self.button_rect = shown.button_rect;
        Drawn {
            back: shown.button_pressed,
            rect: shown.response.rect,
        }
    }

    /// The page's keys, taken before the terminal under it sees them: the
    /// scroll keys scroll, as in Claude Code's view, and nothing typed
    /// reaches the shell. Esc goes back to the manager. Ctrl and Alt chords
    /// pass through to Giverny's own shortcuts. With a box that takes a
    /// line, typing goes into it, and Enter sends it: Esc then clears a
    /// line first, and Home/End move in it.
    fn keys(&mut self, ctx: &egui::Context) -> Keys {
        let mut out = Keys::default();
        let page = i32::from(self.page.rows()).saturating_sub(2).max(1);
        let mut moves: Vec<i32> = Vec::new();
        let mut ends: Vec<bool> = Vec::new();
        let mut edits: Vec<Edit> = Vec::new();
        let typing = self
            .input
            .as_ref()
            .is_some_and(|i| i.state != Sending::Going);
        let has_text = self.input.as_ref().is_some_and(|i| !i.text.is_empty());
        ctx.input_mut(|i| {
            i.events.retain(|e| match e {
                egui::Event::Key {
                    key,
                    pressed,
                    modifiers,
                    ..
                } if !(modifiers.ctrl
                    || modifiers.alt
                    || modifiers.command
                    || modifiers.mac_cmd) =>
                {
                    if *pressed {
                        match key {
                            Key::Escape if typing && has_text => edits.push(Edit::Clear),
                            Key::Escape => out.back = true,
                            Key::Enter if typing => edits.push(Edit::Enter),
                            Key::Backspace if typing => edits.push(Edit::Back),
                            Key::Delete if typing => edits.push(Edit::Delete),
                            Key::ArrowLeft if typing => edits.push(Edit::Left),
                            Key::ArrowRight if typing => edits.push(Edit::Right),
                            Key::Home if typing && has_text => edits.push(Edit::Start),
                            Key::End if typing && has_text => edits.push(Edit::End),
                            Key::ArrowUp => moves.push(1),
                            Key::ArrowDown => moves.push(-1),
                            Key::PageUp => moves.push(page),
                            Key::PageDown => moves.push(-page),
                            Key::Space if !typing => moves.push(-page),
                            Key::Home => ends.push(true),
                            Key::End => ends.push(false),
                            _ => {}
                        }
                    }
                    false
                }
                egui::Event::Text(t) | egui::Event::Paste(t) => {
                    if typing {
                        edits.push(Edit::Insert(t.clone()));
                    }
                    false
                }
                _ => true,
            });
        });
        if let Some(input) = &mut self.input {
            let before = (input.text.clone(), input.cursor);
            for edit in edits {
                if let Some(line) = input.edit(edit) {
                    out.send = Some(line);
                }
            }
            if (input.text.clone(), input.cursor) != before {
                // A new line: why the last one was refused is old news.
                if matches!(input.state, Sending::Refused(_)) {
                    input.state = Sending::Idle;
                }
                self.generation += 1;
                // The box is at the page's end: typing shows it.
                self.page.to_end();
            }
        }
        for n in moves {
            self.page.scroll(n);
        }
        for top in ends {
            if top {
                self.page.to_top();
            } else {
                self.page.to_end();
            }
        }
        out
    }
}

/// What the page's keys did this frame.
#[derive(Debug, Default, PartialEq, Eq)]
struct Keys {
    /// Back to the manager.
    back: bool,
    /// The box's line, sent with Enter.
    send: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Edit {
    Insert(String),
    Back,
    Delete,
    Left,
    Right,
    Start,
    End,
    Clear,
    Enter,
}

impl Input {
    /// One edit; Enter on a line with words in it hands the line over and
    /// empties the box.
    fn edit(&mut self, edit: Edit) -> Option<String> {
        let len = self.text.chars().count();
        let at = |s: &str, n: usize| s.char_indices().nth(n).map_or(s.len(), |(i, _)| i);
        match edit {
            Edit::Insert(t) => {
                // One line: a break would submit the manager's prompt.
                let t: String = t
                    .chars()
                    .map(|c| {
                        if c == '\n' || c == '\r' || c == '\t' {
                            ' '
                        } else {
                            c
                        }
                    })
                    .filter(|c| !c.is_control())
                    .collect();
                let i = at(&self.text, self.cursor);
                self.text.insert_str(i, &t);
                self.cursor += t.chars().count();
            }
            Edit::Back if self.cursor > 0 => {
                let i = at(&self.text, self.cursor - 1);
                self.text.remove(i);
                self.cursor -= 1;
            }
            Edit::Delete if self.cursor < len => {
                let i = at(&self.text, self.cursor);
                self.text.remove(i);
            }
            Edit::Left => self.cursor = self.cursor.saturating_sub(1),
            Edit::Right => self.cursor = (self.cursor + 1).min(len),
            Edit::Start => self.cursor = 0,
            Edit::End => self.cursor = len,
            Edit::Clear => {
                self.text.clear();
                self.cursor = 0;
            }
            Edit::Enter if !self.text.trim().is_empty() => {
                self.cursor = 0;
                self.state = Sending::Going;
                return Some(std::mem::take(&mut self.text));
            }
            Edit::Back | Edit::Delete | Edit::Enter => {}
        }
        None
    }
}

/// The page's keys (called from `App::shortcuts`), while the active tab
/// shows one.
pub fn keys(app: &mut App, ctx: &egui::Context) -> Vec<Action> {
    let active = app.ws.active;
    let Some(page) = app.brief.as_mut().filter(|p| Some(p.tab) == active) else {
        return Vec::new();
    };
    let keys = page.keys(ctx);
    let mut out = Vec::new();
    if let Some(text) = keys.send {
        out.push(Action::ForwardLine(page.tab, text));
    }
    if keys.back {
        out.push(Action::ClosePage(page.tab));
    }
    out
}

/// The prompt box while it takes a line for the manager: Claude Code's
/// box under a worker's view, bright, its rules in the worker's colour and
/// its name on the top one, the line typed so far with the cursor on it,
/// wrapped as Claude Code wraps a long one. Empty, it shows what the line
/// is for, dim.
fn input_box(cols: usize, name: &str, input: &Input) -> Vec<String> {
    let (r, g, b) = WORKER_RGB;
    let rule = format!("\x1b[38;2;{r};{g};{b}m");
    let mut lines = prompt_box(cols, name);
    let undim = |l: &str| l.replace(DIM, "");
    let top = undim(&lines[0]);
    let bottom = format!("{rule}{}{RESET}", "─".repeat(cols));
    const REV: &str = "\x1b[7m";
    const UNREV: &str = "\x1b[27m";
    let going = input.state == Sending::Going;
    let mut body: Vec<String> = Vec::new();
    if input.text.is_empty() {
        let hint = if going {
            "sending to the manager…".to_string()
        } else {
            input.ask.placeholder()
        };
        let hint: String = hint.chars().take(cols.saturating_sub(3)).collect();
        let cursor = if going { " " } else { "" };
        body.push(format!("❯ {REV}{cursor}{UNREV}{DIM}{hint}{RESET}"));
        if !going {
            // The cursor sits on the placeholder's first cell, as Claude
            // Code's does.
            let mut chars = hint.chars();
            let first = chars.next().unwrap_or(' ');
            let rest: String = chars.collect();
            body[0] = format!("❯ {REV}{DIM}{first}{RESET}{DIM}{rest}{RESET}");
        }
    } else {
        let width = cols.saturating_sub(2).max(1);
        let chars: Vec<char> = input.text.chars().collect();
        // One cell more than the text for the cursor at its end.
        let cells = chars.len() + 1;
        for (n, start) in (0..cells).step_by(width).enumerate() {
            let mut line = String::from(if n == 0 { "❯ " } else { "  " });
            for i in start..(start + width).min(cells) {
                let c = chars.get(i).copied().unwrap_or(' ');
                if i == input.cursor && !going {
                    line.push_str(&format!("{REV}{c}{UNREV}"));
                } else {
                    line.push(c);
                }
            }
            body.push(line);
        }
    }
    lines.clear();
    lines.push(top);
    lines.extend(body);
    lines.push(bottom);
    lines
}

/// The rows under the box: what became of the last line, if anything,
/// wrapped to the grid.
fn status_lines(input: &Input, cols: usize) -> Vec<String> {
    let (color, text) = match &input.state {
        Sending::Idle | Sending::Going => return vec![String::new()],
        Sending::Sent(what) => (DIM, what),
        Sending::Refused(why) => ("\x1b[38;2;220;90;90m", why),
    };
    let width = cols.saturating_sub(6).max(8);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let n = line.chars().count();
        if n > 0 && n + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.push(line);
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let lead = if i == 0 { "  ⎿  " } else { "     " };
            format!("{color}{lead}{l}{RESET}")
        })
        .collect()
}

/// Claude Code's prompt box under a worker's view, dimmed: nothing can be
/// typed to this worker. Its rules are in the worker's colour, the worker's
/// `name` on the top one's right, as Claude Code puts it.
fn prompt_box(cols: usize, name: &str) -> Vec<String> {
    let (r, g, b) = WORKER_RGB;
    let rule = format!("\x1b[38;2;{r};{g};{b}m");
    let tag = format!("\x1b[48;2;{r};{g};{b}m\x1b[38;2;0;0;0m");
    let max = cols.saturating_sub(6).max(1);
    let name: String = if name.chars().count() > max {
        let kept: String = name.chars().take(max - 1).collect();
        format!("{kept}…")
    } else {
        name.to_string()
    };
    let label = format!(" {name} ");
    let left = cols.saturating_sub(label.chars().count() + 1);
    vec![
        format!(
            "{DIM}{rule}{}{RESET}{DIM}{tag}{label}{RESET}{DIM}{rule}─{RESET}",
            "─".repeat(left)
        ),
        format!("{DIM}❯ read-only{RESET}"),
        format!("{DIM}{rule}{}{RESET}", "─".repeat(cols)),
    ]
}

fn no_clock(mut row: Row) -> Row {
    row.clock = None;
    row
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
                    self.render = transcript::Render::new(transcript::Fold::OVERLAY);
                }
                let before = self.rows.len();
                for line in &update.lines {
                    transcript::push(&mut self.rows, self.render.line(line));
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
    fn lines_for_view(&self) -> Vec<Row> {
        let mut rows = self.rows.clone();
        if !self.live
            && let Some(rest) = self.tail.rest()
        {
            transcript::push(&mut rows, self.render.clone().line(rest));
        }
        if let Some(err) = &self.error {
            let mut row = Row::one(Tone::Error, err.clone());
            row.starts = true;
            rows.push(row);
        } else if rows.is_empty() {
            rows.push(Row::one(
                Tone::Dim,
                if self.live {
                    "waiting for the worker's first turn…"
                } else {
                    "(the transcript is empty)"
                },
            ));
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use giverny_term::render::theme::Theme;

    const LABELS: &[&str] = &["↺ back to manager", "↺ manager", "↺ back"];

    fn shared() -> RenderShared {
        RenderShared::new(Theme::monet_dark(), 13.0).expect("fonts")
    }

    /// One frame at 1280×820 with the page in the terminal's place,
    /// `events` as its input: what it drew, and the events left for the
    /// terminal.
    fn frame(
        ctx: &egui::Context,
        p: &mut WorkerPage,
        shared: &mut RenderShared,
        events: Vec<egui::Event>,
    ) -> (bool, Drawn, Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 820.0),
            )),
            events,
            ..Default::default()
        };
        let mut out = None;
        let _ = ctx.run_ui(input, |ui| {
            let back = p.keys(ui.ctx()).back;
            let left = ui.ctx().input(|i| i.events.clone());
            let drawn = p.show(ui, shared, LABELS, Color32::LIGHT_BLUE);
            out = Some((back, drawn, left));
        });
        out.expect("ran")
    }

    fn key(k: Key) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn reply(text: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": text}]},
        })
        .to_string()
            + "\n"
    }

    fn file(name: &str, body: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("giverny-page-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-x.jsonl");
        std::fs::write(&path, body).unwrap();
        (dir, path)
    }

    /// giverny#477: the page reads as Claude Code's view of a worker —
    /// the banner's rows, the prompt, each call with its result under it —
    /// then the dimmed prompt box with the task's name on its rule, and the
    /// button's row.
    #[test]
    fn a_transcript_is_laid_out_as_claude_codes_worker_view() {
        let lines = [
            r#"{"type":"user","message":{"content":"Run seq.\nThen report."}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"b","name":"Bash","input":{"command":"seq 1 5"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"b","content":"1\n2\n3\n4\n5"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}]}}"#,
        ];
        let (dir, path) = file("look", &(lines.join("\n") + "\n"));
        let p =
            WorkerPage::transcript(TabId(1), "t".into(), path, false).of_row(row(), "g477".into());
        assert_eq!(
            p.plain_text(),
            "❯ Run seq.\n  Then report.\n\n● Bash(seq 1 5)\n  ⎿  1\n     2\n     3\n     … +2 lines\n\n● Done."
        );
        let (out, button) = p.lines(40, 5);
        let plain: Vec<String> = out.iter().map(|l| strip(l)).collect();
        assert!(plain[..5].iter().all(String::is_empty), "{plain:?}");
        assert_eq!(plain[5].trim_end(), "❯ Run seq.");
        // The band runs the grid's width.
        assert_eq!(plain[5].chars().count(), 40);
        let rule = plain.iter().position(|l| l.contains(" g477 ")).unwrap();
        assert!(plain[rule].starts_with("────"));
        assert!(plain[rule].ends_with(" g477 ─"));
        assert_eq!(plain[rule].chars().count(), 40);
        assert_eq!(plain[rule + 1], "❯ read-only");
        assert_eq!(button, out.len() - 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut esc = false;
        for c in s.chars() {
            if esc {
                esc = !c.is_ascii_alphabetic();
            } else if c == '\x1b' {
                esc = true;
            } else {
                out.push(c);
            }
        }
        out
    }

    fn text(t: &str) -> egui::Event {
        egui::Event::Text(t.into())
    }

    fn revive_page() -> WorkerPage {
        let rows = transcript::prompt_rows("Do it.", None);
        WorkerPage::rows(TabId(1), "t".into(), rows)
            .of_row(row(), "g477".into())
            .with_input(Ask::of(&row(), "g477"))
    }

    /// One frame's keys alone: what they did.
    fn keys_of(ctx: &egui::Context, p: &mut WorkerPage, events: Vec<egui::Event>) -> Keys {
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut out = None;
        let _ = ctx.run_ui(input, |ui| {
            out = Some(p.keys(ui.ctx()));
            assert!(
                ui.ctx().input(|i| i.events.is_empty()),
                "a key reached the shell"
            );
        });
        out.expect("ran")
    }

    /// giverny#483: a finished worker's page has a box like Claude Code's:
    /// what is typed goes into it, never to the shell, and Enter hands the
    /// line over — only a line with words in it, and only on Enter.
    #[test]
    fn typing_in_a_finished_workers_page_fills_its_box_and_enter_sends_it() {
        let ctx = egui::Context::default();
        let mut p = revive_page();
        let (out, _) = p.lines(60, 5);
        let plain: Vec<String> = out.iter().map(|l| strip(l)).collect();
        let at = plain.iter().position(|l| l.contains(" g477 ")).unwrap();
        assert!(
            plain[at + 1].starts_with("❯ Message this worker"),
            "{plain:?}"
        );
        assert!(!out[at + 1].contains("read-only"));
        // Space types a space here, not a page down.
        let k = keys_of(
            &ctx,
            &mut p,
            vec![text("say"), key(Key::Space), text(" hi")],
        );
        assert_eq!(k, Keys::default());
        assert_eq!(p.input.as_ref().unwrap().text, "say hi");
        // Enter on blanks sends nothing.
        let mut blank = revive_page();
        let k = keys_of(&ctx, &mut blank, vec![text("   "), key(Key::Enter)]);
        assert_eq!(k.send, None);
        // Edits: ← and Backspace, a paste's line break made a space.
        keys_of(
            &ctx,
            &mut p,
            vec![
                key(Key::ArrowLeft),
                key(Key::ArrowLeft),
                key(Key::Backspace),
                key(Key::End),
                egui::Event::Paste(" again\nplz".into()),
            ],
        );
        assert_eq!(p.input.as_ref().unwrap().text, "sayhi again plz");
        let (out, _) = p.lines(60, 5);
        let plain: Vec<String> = out.iter().map(|l| strip(l)).collect();
        assert!(plain.iter().any(|l| l == "❯ sayhi again plz "), "{plain:?}");
        let k = keys_of(&ctx, &mut p, vec![key(Key::Enter)]);
        assert_eq!(k.send.as_deref(), Some("sayhi again plz"));
        assert!(!k.back);
        // On its way: the box waits, and says so.
        assert_eq!(p.input.as_ref().unwrap().state, Sending::Going);
        keys_of(&ctx, &mut p, vec![text("x")]);
        assert_eq!(p.input.as_ref().unwrap().text, "");
        // Refused: the line is back in the box, and why is under it.
        p.set_sending(Sending::Refused("Not sent: a draft".into()), Some("sayhi"));
        let (out, _) = p.lines(60, 5);
        let plain: Vec<String> = out.iter().map(|l| strip(l)).collect();
        assert!(plain.iter().any(|l| l.contains("⎿  Not sent: a draft")));
        assert!(plain.iter().any(|l| l.starts_with("❯ sayhi")));
        // Esc clears a line first, then goes back.
        let k = keys_of(&ctx, &mut p, vec![key(Key::Escape)]);
        assert!(!k.back);
        assert_eq!(p.input.as_ref().unwrap().text, "");
        let k = keys_of(&ctx, &mut p, vec![key(Key::Escape)]);
        assert!(k.back);
    }

    #[test]
    fn a_long_line_wraps_in_the_box() {
        let ctx = egui::Context::default();
        let mut p = revive_page();
        keys_of(&ctx, &mut p, vec![text(&"word ".repeat(30))]);
        let (out, _) = p.lines(40, 5);
        let plain: Vec<String> = out.iter().map(|l| strip(l)).collect();
        let at = plain.iter().position(|l| l.contains(" g477 ")).unwrap();
        assert!(plain[at + 1].starts_with("❯ word"));
        assert!(plain[at + 2].starts_with("  "), "{plain:?}");
        assert!(plain[at + 1..].iter().all(|l| l.chars().count() <= 40));
    }

    /// A page with nobody to send to keeps the dimmed read-only box, and
    /// nothing typed goes anywhere.
    #[test]
    fn a_page_with_no_ask_stays_read_only() {
        let ctx = egui::Context::default();
        let mut p = WorkerPage::text(TabId(1), "t".into(), "a note".into());
        let (out, _) = p.lines(40, 5);
        assert!(out.iter().any(|l| strip(l) == "❯ read-only"));
        let k = keys_of(&ctx, &mut p, vec![text("hi"), key(Key::Enter)]);
        assert_eq!(k, Keys::default());
    }

    fn row() -> RowClick {
        RowClick {
            stage: giverny_claude::feed::Stage::Done,
            key: "g477".into(),
            agent_id: Some("a1".into()),
            finished: true,
            asleep: false,
            name: "g477: popup looks like worker screen".into(),
            transcript: None,
            open: None,
            brief: None,
            note: None,
            review: None,
            facts: Vec::new(),
        }
    }

    /// A Next up brief is the prompt the worker will be given.
    #[test]
    fn a_brief_is_drawn_as_the_workers_prompt() {
        let rows = transcript::prompt_rows("# g9\n\nDo it.", None);
        let p = WorkerPage::rows(TabId(1), "t".into(), rows);
        assert_eq!(p.plain_text(), "❯ # g9\n  \n  Do it.");
    }

    #[test]
    fn a_done_transcript_opens_at_its_end_and_the_keys_scroll_it() {
        let mut body = String::new();
        for i in 0..300 {
            body.push_str(&reply(&format!("step {i}")));
        }
        body.push_str(&reply("FINAL REPORT"));
        let (dir, path) = file("done", &body);
        let ctx = egui::Context::default();
        let mut shared = shared();
        let mut p = WorkerPage::transcript(TabId(1), "t".into(), path, false);
        frame(&ctx, &mut p, &mut shared, vec![]);
        assert!(p.page.screen_text().contains("FINAL REPORT"));
        // The button is on screen, under the prompt box.
        assert!(p.button_rect.is_some());
        let (back, _, left) = frame(&ctx, &mut p, &mut shared, vec![key(Key::Home)]);
        assert!(!back);
        assert!(left.is_empty(), "the shell got {left:?}");
        assert!(!p.page.screen_text().contains("FINAL REPORT"));
        assert!(p.button_rect.is_none());
        frame(&ctx, &mut p, &mut shared, vec![key(Key::End)]);
        assert!(p.page.screen_text().contains("FINAL REPORT"));
        let typed = egui::Event::Text("x".into());
        let (_, _, left) = frame(&ctx, &mut p, &mut shared, vec![typed]);
        assert!(left.is_empty(), "the shell got {left:?}");
        let (back, ..) = frame(&ctx, &mut p, &mut shared, vec![key(Key::Escape)]);
        assert!(back);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_live_transcript_is_followed_as_it_grows() {
        let (dir, path) = file("live", &reply("first turn"));
        let ctx = egui::Context::default();
        let mut shared = shared();
        let mut p = WorkerPage::transcript(TabId(1), "t".into(), path.clone(), true);
        frame(&ctx, &mut p, &mut shared, vec![]);
        assert!(p.page.screen_text().contains("first turn"));
        let mut more = reply("first turn");
        for i in 0..200 {
            more.push_str(&reply(&format!("second turn {i}")));
        }
        std::fs::write(&path, &more).unwrap();
        std::thread::sleep(POLL + Duration::from_millis(20));
        frame(&ctx, &mut p, &mut shared, vec![]);
        assert!(p.page.screen_text().contains("second turn 199"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_brief_opens_at_its_top() {
        let text: String = (1..=300).map(|i| format!("brief line {i}\n")).collect();
        let rows = transcript::prompt_rows(&text, None);
        let ctx = egui::Context::default();
        let mut shared = shared();
        let mut p = WorkerPage::rows(TabId(1), "t".into(), rows);
        frame(&ctx, &mut p, &mut shared, vec![]);
        assert!(p.page.screen_text().contains("brief line 1\n"));
        assert!(!p.page.screen_text().contains("brief line 300"));
    }

    #[test]
    fn the_header_pushes_the_entries_down() {
        let rows = transcript::prompt_rows("Do it.", None);
        let ctx = egui::Context::default();
        let mut shared = shared();
        let mut p = WorkerPage::rows(TabId(1), "t".into(), rows);
        frame(&ctx, &mut p, &mut shared, vec![]);
        let at = |p: &WorkerPage| {
            p.page
                .screen_text()
                .lines()
                .position(|l| l.contains("Do it."))
        };
        assert_eq!(at(&p), Some(BANNER_ROWS));
        p.header_h = 200.0;
        frame(&ctx, &mut p, &mut shared, vec![]);
        assert!(at(&p).unwrap() > BANNER_ROWS);
    }
}

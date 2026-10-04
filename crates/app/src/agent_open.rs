//! What a click on an agents-pane row opens.
//!
//! The pane hands the app a [`RowClick`]; this module decides, without
//! touching the app, what that click means — so the decision is tested here
//! and `main.rs::apply` only carries it out:
//!
//! * A **Running** worker opens straight into its view in the parent's
//!   Claude Code: Giverny types the keys that
//!   put Claude Code in that subagent's interactive view, driven by
//!   [`Walk`], which reads the parent's screen after every key rather than
//!   typing blind. With the agents pane on, the relay is asked to show
//!   Claude Code's agent strip while the walk runs (`hooks::show_strip`),
//!   since the strip is the only keyboard path to a worker's view; `/tasks`
//!   is never used. The tab's picture is held from the click until the
//!   view is final ([`Settle`]), so the steps between are never drawn, and
//!   a one-column [`Nudge`] of the pty's width brings the relay's run
//!   forward so the hold is short. Only when that cannot start does the
//!   row open the overlay below.
//! * **Done** workers (and a Running one that cannot be opened) open an
//!   overlay over the current terminal showing the
//!   worker's transcript, rendered as `giverny transcript` renders it: a
//!   Running one live, following it as it grows; a Done one opened at its
//!   end, on the final report. Nothing is typed into Claude Code.
//! * While a tab shows a worker's view, Esc and the terminal's "back to
//!   orchestrator" button walk it back to the main view ([`Walk::home`]),
//!   and the strip's own `main` row is not drawn
//!   ([`row_marks`]).
//! * A Done worker's overlay only reads: nothing is offered. Claude Code
//!   keeps no view of a finished subagent (it leaves the agent strip and
//!   `/tasks`, and its transcript is not a resumable session), and the one
//!   way back in — the parent's `SendMessage` — speaks for the user, so the old
//!   **Revive** that typed a line at the parent's prompt is gone.
//! * **Planned** shows the row's brief in the same overlay; without one, its
//!   task, title and note, and how a dispatcher adds a brief.
//! * A Running row that names no worker but has a feed `open` command runs
//!   it in a new tab — unless it resumes a conversation something is already
//!   running, which two claudes on one transcript would interleave.
//!
//! A row with none of these still answers the click, with an overlay saying
//! what is missing, rather than doing nothing.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use giverny_claude::feed::Stage;

use crate::agents_pane::RowClick;

/// The largest brief read; past it the overlay shows the head and says so.
/// A safety net, not a fold: no brief comes near it.
pub const BRIEF_MAX: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Show a worker's transcript in the overlay; `live` follows it.
    Watch {
        title: String,
        transcript: PathBuf,
        live: bool,
    },
    /// Run the feed's `open` command in a new tab titled `title`.
    Run { title: String, command: String },
    /// Show text in the overlay.
    Show { title: String, body: Body },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// A brief on disk, read when the overlay opens.
    File(PathBuf),
    Text(String),
}

/// What the overlay offers besides reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    Nothing,
    /// A Running worker: open it in the parent's Claude Code (`Attach`).
    OpenInClaude {
        agent_id: String,
    },
}

/// The row's title for a tab or overlay: `acme#158 · name`, whichever exist.
pub fn title_of(click: &RowClick) -> String {
    match (click.key.is_empty(), click.name.is_empty()) {
        (false, false) if click.key != click.name => format!("{} · {}", click.key, click.name),
        (false, _) => click.key.clone(),
        (true, false) => click.name.clone(),
        (true, true) => click
            .agent_id
            .clone()
            .unwrap_or_else(|| "agent".to_string()),
    }
}

fn agent_id(click: &RowClick) -> Option<&str> {
    click.agent_id.as_deref().filter(|id| !id.is_empty())
}

/// Decide what a click does.
pub fn plan(click: &RowClick) -> Plan {
    let title = title_of(click);
    let open = click
        .open
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    match click.stage {
        Stage::Running | Stage::Done => {
            let live = click.stage == Stage::Running;
            if let Some(t) = &click.transcript {
                return Plan::Watch {
                    title,
                    transcript: t.clone(),
                    live,
                };
            }
            let id = agent_id(click);
            if live
                && id.is_none()
                && let Some(cmd) = open
            {
                return Plan::Run {
                    title,
                    command: cmd.to_string(),
                };
            }
            let mut why = match (id, live) {
                (Some(id), true) => format!(
                    "No transcript for worker {id} yet — Claude Code writes it once the \
                     worker's first turn lands. Click again in a moment."
                ),
                (Some(id), false) => {
                    format!("No transcript was found for worker {id}, so there is nothing to show.")
                }
                (None, _) if open.is_some() => {
                    "This row names no worker, so there is no transcript to show.".to_string()
                }
                (None, _) => "This row names no worker and no `open` command, so there is \
                              nothing to open."
                    .to_string(),
            };
            if let Some(cmd) = open.filter(|_| !live) {
                why.push_str(&format!("\n\nThe row's open command:\n  {cmd}"));
            }
            Plan::Show {
                title,
                body: Body::Text(note_then(click, &why)),
            }
        }
        Stage::Planned => {
            if let Some(b) = &click.brief {
                return Plan::Show {
                    title,
                    body: Body::File(b.clone()),
                };
            }
            Plan::Show {
                title,
                body: Body::Text(planned_without_brief(click)),
            }
        }
    }
}

/// A Next up row with no brief still says what it has: its
/// task, its title and its note, then how a dispatcher gives it a brief.
fn planned_without_brief(click: &RowClick) -> String {
    let some = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
    let mut parts: Vec<String> = Vec::new();
    if let Some(k) = some(&click.key) {
        parts.push(format!("Task: {k}"));
    }
    if let Some(t) = some(&click.name).filter(|t| *t != click.key.trim()) {
        parts.push(t);
    }
    if let Some(n) = click.note.as_deref().and_then(some) {
        parts.push(n);
    }
    parts.push(
        "No brief was given for this row. The dispatcher adds one with \
         `giverny-pass plan <task> --eta <min> --brief FILE` (or `start … --brief FILE`)."
            .to_string(),
    );
    parts.join("\n\n")
}

/// What the overlay for this click offers.
pub fn offer(click: &RowClick) -> Offer {
    match (click.stage, agent_id(click)) {
        (Stage::Running, Some(id)) => Offer::OpenInClaude {
            agent_id: id.to_string(),
        },
        _ => Offer::Nothing,
    }
}

/// The conversation a Done row's `open` command resumes, if it does: a
/// click on it goes to the tab running that conversation rather than
/// showing the overlay.
pub fn done_resumes(click: &RowClick) -> Option<String> {
    if click.stage != Stage::Done {
        return None;
    }
    resumed_session(click.open.as_deref()?)
}

fn note_then(click: &RowClick, text: &str) -> String {
    match click.note.as_deref().filter(|n| !n.trim().is_empty()) {
        Some(n) => format!("{n}\n\n{text}"),
        None => text.to_string(),
    }
}

/// Read a brief for the overlay: the file, capped, or a line saying why not.
pub fn read_brief(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() > BRIEF_MAX => {
            let head = String::from_utf8_lossy(&bytes[..BRIEF_MAX]);
            format!(
                "{head}\n\n… {} more bytes — open {} to read the rest.",
                bytes.len() - BRIEF_MAX,
                path.display()
            )
        }
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(err) => format!("Could not read the brief at {}: {err}", path.display()),
    }
}

/// The conversation an `open` command resumes, if it resumes one:
/// the session id after `--resume`/`-r`, or `--resume=<id>`.
pub fn resumed_session(command: &str) -> Option<String> {
    let is_sid = |s: &str| s.len() == 36 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    let unquote = |s: &str| s.trim_matches(|c| c == '\'' || c == '"').to_string();
    let words: Vec<&str> = command.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        if let Some(v) = w.strip_prefix("--resume=") {
            let v = unquote(v);
            if is_sid(&v) {
                return Some(v);
            }
        }
        if (*w == "--resume" || *w == "-r")
            && let Some(next) = words.get(i + 1)
        {
            let v = unquote(next);
            if is_sid(&v) {
                return Some(v);
            }
        }
    }
    None
}

// ------------------------------------------------------------ attach ----

/// Claude Code's key path to one subagent's interactive view: the one place
/// that depends on Claude Code's keybindings and screen, so a Claude Code
/// update that moves them is fixed here.
///
/// Established on Claude Code 2.1.281 with `"tui": "fullscreen"` by driving
/// a real `claude` in tmux with background subagents. Nothing is typed:
/// only arrow keys and
/// Enter, so nothing is left in the prompt and no dialog flickers open.
///
/// 1. Under the prompt, Claude Code draws its agent strip: `● main`, then a
///    row per agent, `◯ <agent type>  <description>  <time> · ↓ <tokens>`
///    (the filled dot is the view on screen). ↓ at the prompt moves the
///    focus down out of the prompt box — through the draft's lines, then
///    the `N shells` pill when background shells exist — into the strip,
///    which then shows `❯` on its selected row and an `↑/↓ to select` or
///    `Enter to view` hint. A one-line draft stays where it is.
/// 2. ↑/↓ step the `❯` along the strip onto the worker, read off the
///    screen one key at a time; its label is the Agent call's description.
/// 3. Enter opens that agent's view. The strip keeps the focus, and
///    anything typed next goes to the view's prompt — which messages the
///    worker. A draft in the main prompt comes along into it.
///
/// Every key is its own write. Never sent: Esc (at the prompt it cancels
/// the turn) and `x` (stops the selected agent).
pub mod cc_keys {
    use super::Keystroke;

    /// Moves the focus out of the prompt, and the strip's selection.
    pub const NEXT: Keystroke = Keystroke::Down;
    pub const PREVIOUS: Keystroke = Keystroke::Up;
    /// Opens the selected strip row's view.
    pub const VIEW: Keystroke = Keystroke::Enter;
    /// The strip's hint rows while it has the focus.
    pub const STRIP_HINTS: &[&str] = &["↑/↓ to select", "Enter to view"];

    /// The `/tasks` dialog's hint row in list mode, and the part only it
    /// has (the strip's hint also says "↑/↓ to select"). The dialog is not
    /// used any more; it is recognised so an attach never types into it.
    pub const LIST_HINT: &str = "↑/↓ to select";
    pub const LIST_HINT_CLOSE: &str = "Esc to close";
    /// The hint row of an agent's detail card.
    pub const DETAIL_HINT: &str = "← to go back";
    /// Between the agent type and its description on a detail card.
    pub const DETAIL_TITLE_SEP: &str = " › ";
    /// The prompt's first column.
    pub const PROMPT: char = '❯';
    /// The dialog's top border.
    pub const DIALOG_TOP: char = '▔';
    /// The footer strip's first row is the main session: `● main` while it
    /// is the one on screen, `◯ main` (`( ) main` without Unicode) while a
    /// subagent's or teammate's view is. Read off Claude Code 2.1.281's
    /// strip row (`[❯ |  ]<● or ◯> main`, then `↑ N more` right-aligned).
    pub const MAIN_UNVIEWED: &[&str] = &["◯ main", "( ) main"];
}

/// Whether Claude Code on this screen shows a worker's view rather than the
/// main session — entered by an attach, by `/tasks`, or by
/// hand, and left with the strip's `main` or ←. Read from the screen alone,
/// so it is right however the view was reached.
///
/// A strip row is the marker and nothing looser: the whole row is the
/// unviewed `main`, save the pointer before it and a `↑ N more` after it,
/// so a transcript line that merely mentions `◯ main` does not count.
pub fn viewing_worker(screen: &str) -> bool {
    screen.lines().any(|row| main_row(row) == Some(false))
}

/// A strip's `main` row: `Some(true)` while main is the view on screen
/// (`● main`), `Some(false)` while a worker's is (`◯ main`), `None` for any
/// other row. The whole row is the dot and `main`, save the pointer before
/// it and a `↑ N more` after it.
fn main_row(row: &str) -> Option<bool> {
    let row = row.trim();
    let row = row
        .strip_prefix(cc_keys::PROMPT)
        .map_or(row, str::trim_start);
    let rest = if let Some(r) = row.strip_prefix("( )").or_else(|| row.strip_prefix("(*)")) {
        r
    } else {
        let mut chars = row.chars();
        let dot = chars.next()?;
        if dot.is_alphanumeric() {
            return None;
        }
        chars.as_str()
    };
    let rest = rest.strip_prefix(" main")?;
    if !(rest.is_empty() || (rest.starts_with("  ") && rest.trim_start().starts_with('↑'))) {
        return None;
    }
    Some(!cc_keys::MAIN_UNVIEWED.iter().any(|m| row.starts_with(m)))
}

/// One key Giverny types into the parent's Claude Code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keystroke {
    Enter,
    Up,
    Down,
}

/// One item of the Background dialog's list, or a row of the agent strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub selected: bool,
    /// The agent id a strip row is tagged with, while the relay tags them
    /// (`hooks::tag_strip_row`).
    pub id: Option<String>,
}

/// What the parent's screen shows, as far as the attach cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    /// The prompt box; `draft` when it holds typed text.
    Prompt { draft: bool },
    /// The agent strip under the prompt, with the focus (a `❯` row).
    Strip(Vec<Item>),
    /// The `/tasks` dialog's list.
    List(Vec<Item>),
    /// An agent's detail card, titled with its description.
    Detail(String),
    /// Anything else: a permission question, another dialog, a shell.
    Other,
}

pub(crate) fn is_rule(line: &str) -> bool {
    line.trim_start().starts_with("───")
}

/// Read the parent's screen. `screen` is its text; `undimmed` the same with
/// dim cells blanked, which is how a draft is told from the placeholder.
pub fn read_view(screen: &str, undimmed: &str) -> View {
    use cc_keys::*;
    let rows: Vec<&str> = screen.lines().collect();

    let hint = |needle: &str, also: Option<&str>| {
        rows.iter()
            .rposition(|r| r.contains(needle) && also.is_none_or(|a| r.contains(a)))
    };
    let top_above = |at: usize| {
        rows[..at]
            .iter()
            .rposition(|r| r.trim_start().starts_with(DIALOG_TOP))
    };
    if let Some(h) = hint(LIST_HINT, Some(LIST_HINT_CLOSE))
        && let Some(top) = top_above(h)
    {
        let items = rows[top + 1..h]
            .iter()
            .filter_map(|r| parse_item(r))
            .collect();
        return View::List(items);
    }
    if let Some(h) = hint(DETAIL_HINT, None)
        && let Some(top) = top_above(h)
        && let Some(title) = rows[top + 1..h]
            .iter()
            .find_map(|r| r.split_once(DETAIL_TITLE_SEP).map(|(_, t)| t.trim()))
    {
        return View::Detail(title.to_string());
    }
    if let Some(items) = read_strip(&rows) {
        return View::Strip(items);
    }
    match prompt_box(screen, undimmed) {
        Some(b) => View::Prompt { draft: b.draft },
        None => View::Other,
    }
}

/// Claude Code's prompt box, as far as a send cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptBox {
    /// The text on the box's top rule: a worker's view carries the worker's
    /// description there (`─── eta worker ─`); the main view none.
    pub label: Option<String>,
    /// It holds typed text (not the dim placeholder).
    pub draft: bool,
    /// Its rows on screen, from the `❯` row to the one above its bottom
    /// rule.
    pub rows: std::ops::Range<usize>,
}

/// Read the prompt box off the screen, wherever the focus is: a rule,
/// `❯ …` in the first column, maybe more lines of draft, a rule. The last
/// one on screen; the transcript above has `❯ ` lines too, but never right
/// under a rule. `undimmed` tells a draft from the placeholder.
pub fn prompt_box(screen: &str, undimmed: &str) -> Option<PromptBox> {
    let rows: Vec<&str> = screen.lines().collect();
    let bright: Vec<&str> = undimmed.lines().collect();
    for i in (1..rows.len()).rev() {
        if !rows[i].starts_with(cc_keys::PROMPT) || !is_rule(rows[i - 1]) {
            continue;
        }
        let Some(end) = (i + 1..rows.len().min(i + 40)).find(|&j| is_rule(rows[j])) else {
            continue;
        };
        let typed = |j: usize| {
            let line = bright.get(j).copied().unwrap_or("");
            let line = if j == i {
                line.trim_start().trim_start_matches(cc_keys::PROMPT)
            } else {
                line
            };
            !line.trim().is_empty()
        };
        let label = rows[i - 1].trim().trim_matches('─').trim();
        return Some(PromptBox {
            label: (!label.is_empty()).then(|| label.to_string()),
            draft: (i..end).any(typed),
            rows: i..end,
        });
    }
    None
}

/// Whether Claude Code's agent strip is drawn under the prompt: its `main`
/// row is on screen. With the agents pane on, Giverny's relay hides the
/// strip until asked to show it.
pub fn strip_shown(screen: &str) -> bool {
    screen.lines().any(|row| main_row(row).is_some())
}

/// `   ❯ <label> (running) · Opus 5.5` → the label, and whether it is
/// selected. Section headings (`Local agents (3)`) and counts are not items.
fn parse_item(row: &str) -> Option<Item> {
    let row = row.trim();
    let (selected, rest) = match row.strip_prefix(cc_keys::PROMPT) {
        Some(r) => (true, r.trim_start()),
        None => (false, row),
    };
    // The last ` (<status>)` whose status is a word, followed by the end or
    // ` · <model>`.
    let mut at = rest.len();
    while let Some(open) = rest[..at].rfind(" (") {
        let after = &rest[open + 2..];
        if let Some(close) = after.find(')') {
            let status = &after[..close];
            let tail = &after[close + 1..];
            if !status.is_empty()
                && status.chars().all(|c| c.is_ascii_lowercase() || c == ' ')
                && (tail.is_empty() || tail.starts_with(" · "))
            {
                let label = rest[..open].trim();
                return (!label.is_empty()).then(|| Item {
                    label: label.to_string(),
                    selected,
                    id: None,
                });
            }
        }
        at = open;
    }
    None
}

/// The agent strip, when it has the focus: the rows under its hint row,
/// one of them selected. `None` when no strip row is selected.
fn read_strip(rows: &[&str]) -> Option<Vec<Item>> {
    let h = rows.iter().rposition(|r| {
        cc_keys::STRIP_HINTS.iter().any(|hint| r.contains(hint))
            && !r.contains(cc_keys::LIST_HINT_CLOSE)
    })?;
    let items: Vec<Item> = rows[h + 1..]
        .iter()
        .map(|r| r.trim())
        // Inside a worker's view a blank row sits under the hint.
        .skip_while(|r| r.is_empty())
        .take_while(|r| !r.is_empty())
        .filter_map(parse_strip_item)
        .collect();
    items.iter().any(|it| it.selected).then_some(items)
}

/// `❯ ◯ general-purpose  eta worker     8s · ↓ 27.2k tokens` → `eta worker`
/// (selected); `● main` → `main`; a row the relay tagged,
/// `◯ [a95d7f3452a3597df] Reading main.rs`, → `Reading main.rs` with that
/// id. `↑ N more` rows are not items.
fn parse_strip_item(row: &str) -> Option<Item> {
    let (selected, rest) = match row.strip_prefix(cc_keys::PROMPT) {
        Some(r) => (true, r.trim_start()),
        None => (false, row),
    };
    // The dot: one symbol (`●`, `◯`, a spinner), or `( )` / `(*)` without
    // Unicode.
    let rest = if rest.len() >= 3 && rest.starts_with('(') && rest[2..].starts_with(')') {
        &rest[3..]
    } else {
        let mut chars = rest.chars();
        let dot = chars.next()?;
        if dot.is_alphanumeric() || dot == '↑' || dot == '↓' {
            return None;
        }
        chars.as_str()
    };
    if !rest.starts_with(' ') {
        return None;
    }
    let rest = rest.trim();
    if let Some((id, label)) = giverny_claude::hooks::strip_row_tag(rest) {
        return Some(Item {
            label: label.to_string(),
            selected,
            id: Some(id.to_string()),
        });
    }
    // Columns are separated by runs of spaces: type, description, stats.
    let cols: Vec<&str> = rest
        .split("  ")
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect();
    let label = match cols.as_slice() {
        [] => return None,
        [only] => *only,
        [_, desc, ..] => *desc,
    };
    Some(Item {
        label: label.to_string(),
        selected,
        id: None,
    })
}

/// Rows of the prompt box on screen (its draft's lines), else 1.
fn prompt_rows(screen: &str) -> usize {
    let rows: Vec<&str> = screen.lines().collect();
    for i in (1..rows.len()).rev() {
        if !rows[i].starts_with(cc_keys::PROMPT) || !is_rule(rows[i - 1]) {
            continue;
        }
        if let Some(end) = (i + 1..rows.len().min(i + 40)).find(|&j| is_rule(rows[j])) {
            return end - i;
        }
    }
    1
}

/// Whether a label on Claude Code's screen names this description: equal
/// but for spacing, or cut short with `…`.
pub fn label_matches(label: &str, description: &str) -> bool {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let (label, description) = (norm(label), norm(description));
    if label == description {
        return true;
    }
    match label.strip_suffix('…') {
        Some(head) => !head.trim().is_empty() && description.starts_with(head.trim_end()),
        None => false,
    }
}

/// The bytes for one keystroke, `encode` being the terminal's key encoder
/// (`giverny_term::input::encode_key` in the tab's current mode), so an arrow
/// goes out as the running program asked for it.
pub fn keystroke_bytes(
    key: Keystroke,
    encode: impl Fn(egui::Key, egui::Modifiers) -> Option<Vec<u8>>,
) -> Vec<u8> {
    use egui::{Key, Modifiers};
    let special = |k: Key, m: Modifiers| encode(k, m).unwrap_or_default();
    match key {
        Keystroke::Enter => special(Key::Enter, Modifiers::NONE),
        Keystroke::Up => special(Key::ArrowUp, Modifiers::NONE),
        Keystroke::Down => special(Key::ArrowDown, Modifiers::NONE),
    }
}

/// Why an attach stopped short of the worker's view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stuck {
    /// Neither the prompt nor the dialog is on screen.
    NoPrompt,
    /// The dialog opened but does not list the worker.
    NotListed,
    /// More than one running agent carries this description.
    Ambiguous,
    /// The screen never got where the keys should have taken it.
    TimedOut,
    /// Claude Code's agent strip never came up under the prompt.
    NoStrip,
    /// Enter opened a view, but another worker's (its prompt's rule says
    /// which).
    WrongView(String),
}

impl Stuck {
    /// What the overlay tells the user.
    pub fn explain(&self, description: &str) -> String {
        match self {
            Stuck::NoPrompt => "This means typing into this tab's Claude Code, and its prompt \
                                is not on screen (a permission question, or another dialog, is \
                                up). Answer or close it, then try again."
                .to_string(),
            Stuck::NotListed => format!(
                "The agent list under Claude Code's prompt has no agent called \
                 \u{201c}{description}\u{201d}. It may have finished and left the list; its \
                 row still shows the transcript."
            ),
            Stuck::Ambiguous => format!(
                "More than one agent is called \u{201c}{description}\u{201d}, so Giverny \
                 cannot tell which is which. The agent list under Claude Code's prompt is \
                 left selected: pick it with ↑/↓ and press Enter."
            ),
            Stuck::TimedOut => "Claude Code did not answer the keys in time, so Giverny \
                                stopped. Try again, or press ↓ at this tab's prompt until the \
                                agent list is selected, then Enter on the worker."
                .to_string(),
            Stuck::NoStrip => format!(
                "Claude Code's agent list never came up under this tab's prompt, so there \
                 was no way to \u{201c}{description}\u{201d}. It may have just finished."
            ),
            Stuck::WrongView(other) => format!(
                "Giverny opened the view of \u{201c}{other}\u{201d} instead of \
                 \u{201c}{description}\u{201d}: Claude Code's agent list names busy agents by \
                 what they are doing, and two looked alike. Press Esc to go back to the \
                 orchestrator."
            ),
        }
    }
}

/// What to do this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tick {
    /// Write this key to the parent's pty.
    Send(Keystroke),
    /// Nothing yet; look again next frame.
    Wait,
    /// The foreground key is sent: the worker's view is open.
    Done,
    Stuck(Stuck),
}

/// The gap between two keys: each must be its own write.
pub const KEY_GAP: Duration = Duration::from_millis(40);
/// How long a key's effect is waited for before the screen is read afresh.
pub const SETTLE: Duration = Duration::from_millis(700);
/// How long a worker's view must hold still, prompt drawn but cursor not in
/// it, before the walk takes the footer's shells pill to have the focus.
/// Claude Code draws the frame and puts its cursor back in one write, so
/// this only has to outlast a frame split across reads; the full [`SETTLE`]
/// here cost every open ~0.7 s whenever a background shell was running.
pub const FOCUS_CALM: Duration = Duration::from_millis(150);
/// The whole attach, start to finish.
pub const DEADLINE: Duration = Duration::from_secs(5);

/// Walks the key path into a parent's Claude Code, reading its screen after
/// every step. Pure but for the clock and screen it is handed, so it is
/// driven the same way in tests as in the app.
#[derive(Debug, Clone)]
pub struct Attach {
    pub description: String,
    /// Other labels the strip may show the worker by: Claude Code swaps the
    /// description for the worker's live activity label once it is working.
    aliases: Vec<String>,
    /// With no row by any of its names, take the strip's only agent row:
    /// for a caller that checks the view it lands on ([`Walk`]).
    only_agent: bool,
    /// The worker's agent id: on a strip whose rows carry ids (the relay
    /// tags them while a walk asks for the strip) the row is
    /// found by it alone, whatever Claude Code labels it.
    id: Option<String>,
    /// ↓ has been sent out of the prompt.
    opened: bool,
    /// Enter on the worker is queued.
    finishing: bool,
    queue: VecDeque<Keystroke>,
    last_key: Option<Instant>,
    /// After the queue drains, wait until the view differs from this one,
    /// or the instant passes.
    settle: Option<(Instant, View)>,
    deadline: Instant,
}

impl Attach {
    pub fn new(description: impl Into<String>, now: Instant) -> Attach {
        Attach {
            description: description.into(),
            aliases: Vec::new(),
            only_agent: false,
            id: None,
            opened: false,
            finishing: false,
            queue: VecDeque::new(),
            last_key: None,
            settle: None,
            deadline: now + DEADLINE,
        }
    }

    /// An attach that also takes a strip row labelled with one of
    /// `aliases` (empty ones are dropped) for the worker.
    pub fn with_aliases(mut self, aliases: impl IntoIterator<Item = String>) -> Attach {
        self.aliases = aliases
            .into_iter()
            .filter(|a| !a.trim().is_empty())
            .collect();
        self
    }

    /// With no row by any name, go to the strip's only agent row, if it
    /// has exactly one. The caller must check the view it opens.
    pub fn or_only_agent(mut self) -> Attach {
        self.only_agent = true;
        self
    }

    /// Find the row by agent id `id` when the strip's rows carry ids.
    pub fn with_id(mut self, id: Option<String>) -> Attach {
        self.id = id.filter(|i| !i.is_empty());
        self
    }

    fn names(&self, label: &str) -> bool {
        label_matches(label, &self.description)
            || self.aliases.iter().any(|a| label_matches(label, a))
    }

    fn push(&mut self, keys: &[Keystroke], view: View, now: Instant) -> Tick {
        self.queue.extend(keys.iter().copied());
        self.settle = Some((now + SETTLE, view));
        self.send_next(now)
    }

    fn send_next(&mut self, now: Instant) -> Tick {
        if self.last_key.is_some_and(|t| now < t + KEY_GAP) {
            return Tick::Wait;
        }
        match self.queue.pop_front() {
            Some(k) => {
                self.last_key = Some(now);
                if let Some((until, _)) = &mut self.settle {
                    *until = now + SETTLE;
                }
                Tick::Send(k)
            }
            None => Tick::Wait,
        }
    }

    /// One frame: `screen` and `undimmed` are the parent's screen now.
    pub fn tick(&mut self, now: Instant, screen: &str, undimmed: &str) -> Tick {
        if !self.queue.is_empty() {
            return self.send_next(now);
        }
        if self.finishing {
            return Tick::Done;
        }
        if now >= self.deadline {
            return Tick::Stuck(Stuck::TimedOut);
        }
        let view = read_view(screen, undimmed);
        // ↓ went out and the prompt is still all there is: the keys are
        // still landing, or — once they have had time to — there is no
        // strip to reach, so no agent is listed. However the prompt box
        // redraws meanwhile (the placeholder, the footer) it is waited out.
        if self.opened && matches!(view, View::Prompt { .. }) {
            return match &self.settle {
                Some((until, _)) if now < *until => Tick::Wait,
                _ => Tick::Stuck(Stuck::NotListed),
            };
        }
        if let Some((until, before)) = &self.settle {
            if view == *before && now < *until {
                return Tick::Wait;
            }
            self.settle = None;
        }
        match &view {
            View::Strip(items) => {
                // Rows tagged with ids: the id alone says which is the
                // worker's. A busy worker's label is Claude
                // Code's summary of what it is doing, never its description.
                let by_id = self
                    .id
                    .as_deref()
                    .filter(|_| items.iter().any(|it| it.id.is_some()));
                let hits: Vec<usize> = items
                    .iter()
                    .enumerate()
                    .filter(|(_, it)| match by_id {
                        Some(id) => it.id.as_deref() == Some(id),
                        None => self.names(&it.label),
                    })
                    .map(|(i, _)| i)
                    .collect();
                // The strip's first row is `main`; the rest are agents.
                let only = (by_id.is_none()
                    && self.only_agent
                    && items.len() == 2
                    && items[0].label == "main")
                    .then_some(1);
                let target = match (hits.as_slice(), only) {
                    ([], Some(one)) => one,
                    ([], None) => return Tick::Stuck(Stuck::NotListed),
                    ([one], _) => *one,
                    _ => return Tick::Stuck(Stuck::Ambiguous),
                };
                let Some(at) = items.iter().position(|it| it.selected) else {
                    return Tick::Wait;
                };
                let key = match target.cmp(&at) {
                    std::cmp::Ordering::Equal => {
                        self.finishing = true;
                        cc_keys::VIEW
                    }
                    std::cmp::Ordering::Greater => cc_keys::NEXT,
                    std::cmp::Ordering::Less => cc_keys::PREVIOUS,
                };
                self.push(&[key], view, now)
            }
            View::Prompt { .. } => {
                self.opened = true;
                // Down through the draft's lines, past the shells pill,
                // into the strip. A Down too many only moves the strip's
                // selection, which the next look corrects.
                let downs = prompt_rows(screen) + 1;
                let keys = vec![cc_keys::NEXT; downs];
                self.push(&keys, view, now)
            }
            View::List(_) | View::Detail(_) | View::Other => Tick::Stuck(Stuck::NoPrompt),
        }
    }
}

// -------------------------------------------------------------- walk ----

/// The agent rows of Claude Code's strip on this screen, focused or not:
/// the rows under its `main` row. Empty while the strip is hidden, and
/// while it shows `main` alone (a worker's view with the agents pane on:
/// the relay hides every agent row, but Claude Code keeps `main` so the
/// way back is there).
pub fn strip_agents(screen: &str) -> Vec<Item> {
    let rows: Vec<&str> = screen.lines().collect();
    let Some(at) = rows.iter().rposition(|r| main_row(r).is_some()) else {
        return Vec::new();
    };
    rows[at + 1..]
        .iter()
        .map(|r| r.trim())
        .take_while(|r| !r.is_empty())
        .filter_map(parse_strip_item)
        .collect()
}

/// Whether Claude Code's prompt has the keyboard: the terminal cursor
/// (`cursor`, its screen row while it is shown) sits in the prompt box.
/// Claude Code shows the cursor only there; with the focus on the strip or
/// the footer's `N shells` pill it hides it (2.1.281), and
/// nothing else on screen tells the pill's focus from the prompt's.
pub fn prompt_focused(screen: &str, cursor: Option<usize>) -> bool {
    let Some(row) = cursor else {
        return false;
    };
    prompt_box(screen, screen).is_some_and(|p| p.rows.contains(&row))
}

/// What a [`Walk`] reads each frame.
#[derive(Debug, Clone, Copy)]
pub struct Look<'a> {
    /// The screen's text, row per line.
    pub screen: &'a str,
    /// The same with dim cells blanked (a draft told from the placeholder).
    pub undimmed: &'a str,
    /// The terminal cursor's screen row, while the cursor is shown.
    pub cursor: Option<usize>,
}

/// Where a [`Walk`] takes the tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Goal {
    /// A worker's view, found on the strip by its description or one of
    /// the other labels Claude Code may show it by (its live activity).
    Worker {
        description: String,
        aliases: Vec<String>,
        /// Its agent id, which the relay tags the strip's rows with while
        /// the strip is asked for: found by that first.
        agent_id: Option<String>,
    },
    /// The main session's view: the orchestrator.
    Main,
}

/// How long the strip is waited for: with the agents pane on, the relay
/// shows it only when asked (`hooks::show_strip`), and Claude Code runs the
/// relay about every five seconds.
pub const STRIP_WAIT: Duration = Duration::from_secs(8);
/// How long one step's effect is waited for on screen.
pub const STEP_WAIT: Duration = Duration::from_secs(3);
/// The whole walk, start to finish.
pub const WALK_DEADLINE: Duration = Duration::from_secs(20);
/// The most `↑`s sent to bring the focus back up into the prompt: one per
/// strip row above the selection, and one for the shells pill.
const MAX_UPS: u8 = 12;

#[derive(Debug, Clone)]
enum Phase {
    Start,
    /// Waiting for the strip's rows.
    Strip,
    Attach(Box<Attach>),
    /// Enter went on the row: waiting for the view it opens.
    Landing,
    /// On the view: the focus back into its prompt.
    Focus {
        pill_up: bool,
    },
}

/// What was on screen when the last key went.
type Seen = (View, Option<PromptBox>, Option<usize>);

/// What the focus walk waits to hold still: the view, the prompt box's
/// label and draft, and the cursor's row within the box — not where on
/// screen the box is, which moves when the relay hides the strip's rows
/// under it and says nothing about the focus.
type Held = (View, Option<(Option<String>, bool)>, Option<isize>);

fn held(seen: &Seen) -> Held {
    let (view, prompt, cursor) = seen;
    let at = |c: usize| {
        let top = prompt.as_ref().map_or(0, |p| p.rows.start);
        c as isize - top as isize
    };
    (
        view.clone(),
        prompt.as_ref().map(|p| (p.label.clone(), p.draft)),
        cursor.map(at),
    )
}

/// Moves a tab's Claude Code between the main view and a worker's view
/// through the agent strip: wait for the strip, step to
/// the row with [`Attach`], Enter, check the view that opened, then put the
/// focus back in that view's prompt — so what the user types next goes to it,
/// and an Enter never lands on the strip. Reads the screen before every
/// key and never types text; `/tasks` is never opened.
///
/// Pure but for the clock and screen it is handed, like [`Attach`].
#[derive(Debug, Clone)]
pub struct Walk {
    pub goal: Goal,
    phase: Phase,
    /// When the current phase began.
    since: Instant,
    /// The screen when the last key went, and when: the next look waits
    /// until it changes, or `SETTLE` passes.
    hold: Option<(Instant, Seen)>,
    /// The screen, and since when it has looked like that.
    calm: Option<(Instant, Held)>,
    ups: u8,
    /// In the worker's view with the focus off the strip: its rows may go.
    off_strip: bool,
    deadline: Instant,
}

impl Walk {
    /// To worker `description` (and `aliases`, the strip's other names
    /// for it).
    pub fn open(description: impl Into<String>, aliases: Vec<String>, now: Instant) -> Walk {
        Walk::new(
            Goal::Worker {
                description: description.into(),
                aliases,
                agent_id: None,
            },
            now,
        )
    }

    /// Find the worker's strip row by its agent id `id` wherever the rows
    /// carry ids; its labels stay the way in where they do not.
    pub fn by_id(mut self, id: impl Into<String>) -> Walk {
        if let Goal::Worker { agent_id, .. } = &mut self.goal {
            *agent_id = Some(id.into()).filter(|i: &String| !i.is_empty());
        }
        self
    }

    /// Back to the main view.
    pub fn home(now: Instant) -> Walk {
        Walk::new(Goal::Main, now)
    }

    fn new(goal: Goal, now: Instant) -> Walk {
        Walk {
            goal,
            phase: Phase::Start,
            since: now,
            hold: None,
            calm: None,
            ups: 0,
            off_strip: false,
            deadline: now + WALK_DEADLINE,
        }
    }

    /// The worker's description, or `main`.
    pub fn target(&self) -> &str {
        match &self.goal {
            Goal::Worker { description, .. } => description,
            Goal::Main => "main",
        }
    }

    /// Whether the walk needs Claude Code's strip drawn: into a worker's
    /// view until the focus is off the strip — its rows going away under a
    /// focused strip would move its selection. From there the
    /// relay can hide them again while the focus walk finishes, so its
    /// answer is not waited for only after it. Going home
    /// needs no help: a worker's view keeps `main` on the strip whatever
    /// the relay hides.
    pub fn wants_strip(&self) -> bool {
        matches!(self.goal, Goal::Worker { .. }) && !self.off_strip
    }

    /// The phase the walk is in, for the timing log.
    pub fn phase_name(&self) -> &'static str {
        match self.phase {
            Phase::Start => "start",
            Phase::Strip => "strip",
            Phase::Attach(_) => "attach",
            Phase::Landing => "landing",
            Phase::Focus { .. } => "focus",
        }
    }

    /// Whether the walk is still on its way to the goal's Enter: waiting
    /// for the strip, or stepping along it.
    pub fn entering(&self) -> bool {
        matches!(self.phase, Phase::Start | Phase::Strip | Phase::Attach(_))
    }

    fn go(&mut self, phase: Phase, now: Instant) {
        self.phase = phase;
        self.since = now;
    }

    fn key(&mut self, k: Keystroke, seen: Seen, now: Instant) -> Tick {
        self.hold = Some((now, seen));
        Tick::Send(k)
    }

    /// The view on screen is the goal's.
    fn arrived(&self, screen: &str, label: Option<&str>) -> bool {
        match &self.goal {
            Goal::Main => !viewing_worker(screen) && label.is_none(),
            Goal::Worker { description, .. } => {
                viewing_worker(screen) && label.is_some_and(|l| label_matches(l, description))
            }
        }
    }

    /// One frame.
    pub fn tick(&mut self, now: Instant, look: Look) -> Tick {
        let (screen, undimmed) = (look.screen, look.undimmed);
        let view = read_view(screen, undimmed);
        let prompt = prompt_box(screen, undimmed);
        let seen: Seen = (view.clone(), prompt.clone(), look.cursor);
        if let Some((at, before)) = &self.hold {
            if now < *at + KEY_GAP || (seen == *before && now < *at + SETTLE) {
                return Tick::Wait;
            }
            self.hold = None;
        }
        if now >= self.deadline {
            return Tick::Stuck(Stuck::TimedOut);
        }
        let waited = now.duration_since(self.since);
        let label = prompt.as_ref().and_then(|p| p.label.clone());
        match &mut self.phase {
            Phase::Start => {
                if prompt.is_none() || matches!(view, View::List(_) | View::Detail(_) | View::Other)
                {
                    return Tick::Stuck(Stuck::NoPrompt);
                }
                if self.arrived(screen, label.as_deref()) {
                    // Already there: only the focus to see to.
                    self.go(Phase::Focus { pill_up: false }, now);
                } else {
                    self.go(Phase::Strip, now);
                }
                self.tick(now, look)
            }
            Phase::Strip => {
                let ready = match &self.goal {
                    Goal::Main => strip_shown(screen),
                    Goal::Worker { .. } => !strip_agents(screen).is_empty(),
                };
                if !ready {
                    return if waited < STRIP_WAIT {
                        Tick::Wait
                    } else {
                        Tick::Stuck(Stuck::NoStrip)
                    };
                }
                let attach = match &self.goal {
                    Goal::Main => Attach::new("main", now),
                    Goal::Worker {
                        description,
                        aliases,
                        agent_id,
                    } => Attach::new(description.clone(), now)
                        .with_aliases(aliases.clone())
                        .with_id(agent_id.clone())
                        .or_only_agent(),
                };
                self.go(Phase::Attach(Box::new(attach)), now);
                self.tick(now, look)
            }
            Phase::Attach(a) => match a.tick(now, screen, undimmed) {
                Tick::Done => {
                    self.go(Phase::Landing, now);
                    Tick::Wait
                }
                other => other,
            },
            Phase::Landing => {
                if self.arrived(screen, label.as_deref()) {
                    self.go(Phase::Focus { pill_up: false }, now);
                    return self.tick(now, look);
                }
                if let (Goal::Worker { .. }, Some(other)) = (&self.goal, &label)
                    && viewing_worker(screen)
                    && waited >= SETTLE
                {
                    return Tick::Stuck(Stuck::WrongView(other.clone()));
                }
                if waited < STEP_WAIT {
                    Tick::Wait
                } else {
                    Tick::Stuck(Stuck::TimedOut)
                }
            }
            Phase::Focus { pill_up } => {
                let pill_up = *pill_up;
                // The screen held still: a key's effect has landed, cursor
                // and all.
                let still = held(&seen);
                let calm = match &self.calm {
                    Some((since, before)) if *before == still => now >= *since + FOCUS_CALM,
                    _ => {
                        self.calm = Some((now, still));
                        false
                    }
                };
                match &view {
                    // The strip still has the focus: ↑ walks the selection
                    // up it, and past its top out of it. It never changes
                    // the view — only Enter does.
                    View::Strip(_) if self.ups < MAX_UPS => {
                        self.ups += 1;
                        self.key(cc_keys::PREVIOUS, seen, now)
                    }
                    View::Prompt { .. } if !self.off_strip && viewing_worker(screen) => {
                        self.off_strip = true;
                        self.tick(now, look)
                    }
                    View::Prompt { .. } if prompt_focused(screen, look.cursor) => Tick::Done,
                    // The prompt is drawn but has not got the cursor, and
                    // the screen has settled: the shells pill has the focus,
                    // and one ↑ leaves it. Only one: in the prompt, ↑ would
                    // recall history.
                    View::Prompt { .. } if !pill_up && calm => {
                        self.phase = Phase::Focus { pill_up: true };
                        self.calm = None;
                        self.key(cc_keys::PREVIOUS, seen, now)
                    }
                    // A terminal that never shows the cursor: the view is
                    // right, which is what counts.
                    View::Prompt { .. } if pill_up && calm => Tick::Done,
                    View::Prompt { .. } => Tick::Wait,
                    _ if waited < STEP_WAIT => Tick::Wait,
                    _ => Tick::Stuck(Stuck::NoPrompt),
                }
            }
        }
    }
}

// ----------------------------------------------------------- settle ----

/// How long the final screen must hold before it is shown: Claude Code
/// writes a frame in more than one read, and half of one is not it.
pub const SETTLE_CALM: Duration = Duration::from_millis(60);
/// The longest a finished walk waits for its final screen.
pub const SETTLE_MAX: Duration = Duration::from_millis(2500);

/// After a [`Walk`] lands: waits for the screen to be the view's final
/// one, so the terminal can go from the view before to it in one frame.
/// Into a worker's view with the agents pane on, final is
/// the strip's agent rows gone again (only its `main` row stays, and that
/// is not drawn); back on main, it is main's prompt.
#[derive(Debug, Clone)]
pub struct Settle {
    main: bool,
    pane_on: bool,
    since: Instant,
    good_since: Option<Instant>,
}

impl Settle {
    pub fn new(goal: &Goal, pane_on: bool, now: Instant) -> Settle {
        Settle {
            main: matches!(goal, Goal::Main),
            pane_on,
            since: now,
            good_since: None,
        }
    }

    /// Whether `screen` is the view's final screen.
    pub fn is_final(&self, screen: &str) -> bool {
        if self.main {
            !viewing_worker(screen)
        } else {
            viewing_worker(screen) && (!self.pane_on || strip_agents(screen).is_empty())
        }
    }

    /// One frame: true once the final screen has held for [`SETTLE_CALM`],
    /// or [`SETTLE_MAX`] has gone by regardless.
    pub fn done(&mut self, now: Instant, screen: &str) -> bool {
        if now >= self.since + SETTLE_MAX {
            return true;
        }
        if !self.is_final(screen) {
            self.good_since = None;
            return false;
        }
        let since = *self.good_since.get_or_insert(now);
        now >= since + SETTLE_CALM
    }
}

/// How long the pty is left one column narrow: long enough for Claude Code
/// to take the new width in before the real one comes back.
pub const NUDGE_NARROW: Duration = Duration::from_millis(90);
/// How long after a nudge the next is sent, while still waited on. Claude
/// Code answers a nudge about 0.4 s after the width comes back; one sent
/// while its relay run is in flight throws that run's answer away, so the
/// next waits well past it.
pub const NUDGE_AGAIN: Duration = Duration::from_millis(800);
/// The longest a [`Nudge::ask_held`] keeps the pty narrow waiting for the
/// answer: long enough that it never cuts off a relay run in flight.
pub const NUDGE_HOLD: Duration = Duration::from_millis(1200);
/// The most nudges one wait sends; after that the relay's own tick.
const NUDGE_MAX: u8 = 5;
/// Claude Code runs the relay this long after the last width change
/// (`subagentStatusLine`'s 300 ms), less a little for the relay to start
/// and read the ask: a new ask made before then is answered by that run.
const CC_TICK_AFTER_RESIZE: Duration = Duration::from_millis(280);

/// What a [`Nudge`] wants done to the pty's width this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    Narrow,
    Restore,
}

/// Brings Claude Code's next `subagentStatusLine` run forward.
/// Claude Code runs it every five seconds, and 300 ms after
/// the terminal's width changes — so a width one column short for a moment
/// makes the relay's answer to a new strip ask land within about half a
/// second instead of up to five. The pty alone is narrowed, never the grid,
/// and only while the tab's picture is held, so nothing of it is seen.
///
/// Claude Code's timer starts over at every width change, so the answer
/// comes about 300 ms after the *last* one. [`Nudge::ask_held`] leaves the
/// pty narrow until the answer is on screen, which starts that timer at the
/// narrowing rather than at the restore; it is for a wait whose screens are
/// read but never shown as final (the strip on the way in), since Claude
/// Code draws them one column short until the width is back.
#[derive(Debug, Clone, Default)]
pub struct Nudge {
    narrowed: Option<Instant>,
    last: Option<Instant>,
    sent: u8,
    /// Keep the pty narrow until the answer is in ([`Nudge::ask_held`]).
    held: bool,
}

impl Nudge {
    /// Start over: a new ask for the relay to answer. When the last nudge
    /// came back so recently that Claude Code's run is still to come, that
    /// run answers it, and nudging now would only put it off: the next
    /// nudge waits its usual [`NUDGE_AGAIN`] after that one.
    pub fn ask(&mut self, now: Instant) {
        if self
            .last
            .is_some_and(|back| now >= back + CC_TICK_AFTER_RESIZE)
        {
            self.last = None;
        }
        self.sent = 0;
        self.held = false;
    }

    /// Start over, keeping the pty narrow until the answer is on screen
    /// (or [`NUDGE_HOLD`] passes).
    pub fn ask_held(&mut self) {
        self.last = None;
        self.sent = 0;
        self.held = true;
    }

    /// One frame. `waiting`: the relay's answer is still not on screen.
    pub fn tick(&mut self, now: Instant, waiting: bool) -> Option<Width> {
        if let Some(at) = self.narrowed {
            let back = if self.held {
                !waiting || now >= at + NUDGE_HOLD
            } else {
                now >= at + NUDGE_NARROW
            };
            if back {
                self.narrowed = None;
                self.last = Some(now);
                return Some(Width::Restore);
            }
            return None;
        }
        let due = self.last.is_none_or(|at| now >= at + NUDGE_AGAIN);
        if waiting && due && self.sent < NUDGE_MAX {
            self.sent += 1;
            self.narrowed = Some(now);
            return Some(Width::Narrow);
        }
        None
    }

    /// The pty is narrow right now and must be put back.
    pub fn is_narrow(&self) -> bool {
        self.narrowed.is_some()
    }
}

// ------------------------------------------------------------ marks ----

/// What Giverny paints over a Claude Code tab's grid while the agents pane
/// stands in for Claude Code's own agent strip:
///
/// * the strip's `main` row is not drawn — the pane shows which worker the
///   tab is on, and the way back is Giverny's button;
/// * while a worker's view is on screen, a "back to orchestrator" button
///   sits where that `◯ main` was, left-aligned under the terminal's other
///   text; with no such row, at the right end of the status
///   line under the prompt (the row with the session's token counts);
/// * and Esc presses that button rather than reaching Claude Code, while
///   the prompt or the strip has the keyboard (not over a dialog, whose Esc
///   is its own).
pub fn row_marks(screen: &str) -> giverny_term::widget::RowMarks {
    let rows: Vec<&str> = screen.lines().collect();
    let as_row = |i: usize| u16::try_from(i).ok();
    let hidden = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| main_row(r).is_some())
        .filter_map(|(i, _)| as_row(i))
        .collect();
    let mut marks = giverny_term::widget::RowMarks {
        hidden,
        ..Default::default()
    };
    if !viewing_worker(screen) {
        return marks;
    }
    marks.escape = matches!(
        read_view(screen, screen),
        View::Prompt { .. } | View::Strip(_)
    );
    // Where the strip's `◯ main` was: its dot's column, pointer skipped.
    if let Some(i) = rows.iter().rposition(|r| main_row(r) == Some(false)) {
        let row = rows[i];
        let lead = row.chars().take_while(|c| c.is_whitespace()).count();
        let rest = row.trim_start();
        let dot = match rest.strip_prefix(cc_keys::PROMPT) {
            Some(after) => {
                let gap = after.chars().take_while(|c| c.is_whitespace()).count();
                lead + 1 + gap
            }
            None => lead,
        };
        marks.button = as_row(i).map(|r| (r, 0));
        marks.button_left = as_row(dot);
        return marks;
    }
    let Some(prompt) = prompt_box(screen, screen) else {
        return marks;
    };
    // The status line: under the prompt's bottom rule, the row with the
    // token counts (Giverny's `… · session: … · subagents: … · total: …`);
    // else the first row there with anything on it.
    let below = prompt.rows.end + 1;
    let near = below..rows.len().min(below + 3);
    let status = near
        .clone()
        .find(|&i| rows[i].contains("total:") || rows[i].contains("session:"))
        .or_else(|| near.clone().find(|&i| !rows[i].trim().is_empty()));
    marks.button = status.and_then(|i| Some((as_row(i)?, as_row(rows[i].chars().count())?)));
    marks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(stage: Stage) -> RowClick {
        RowClick {
            stage,
            key: "acme#158".into(),
            agent_id: Some("a93".into()),
            name: "Wren".into(),
            transcript: None,
            open: None,
            brief: None,
            note: None,
            review: None,
            facts: Vec::new(),
        }
    }

    #[test]
    fn running_and_done_watch_the_transcript() {
        let mut c = click(Stage::Running);
        c.transcript = Some("/t/agent-a93.jsonl".into());
        c.open = Some("claude --resume x".into());
        assert_eq!(
            plan(&c),
            Plan::Watch {
                title: "acme#158 · Wren".into(),
                transcript: "/t/agent-a93.jsonl".into(),
                live: true,
            }
        );
        assert_eq!(
            offer(&c),
            Offer::OpenInClaude {
                agent_id: "a93".into()
            }
        );
        // Done: the same overlay, not live, and never a new tab — even
        // with an `open` command.
        c.stage = Stage::Done;
        assert_eq!(
            plan(&c),
            Plan::Watch {
                title: "acme#158 · Wren".into(),
                transcript: "/t/agent-a93.jsonl".into(),
                live: false,
            }
        );
        assert_eq!(offer(&c), Offer::Nothing);
        c.stage = Stage::Planned;
        assert_eq!(offer(&c), Offer::Nothing);
    }

    #[test]
    fn done_without_a_transcript_says_so_and_opens_no_tab() {
        let mut c = click(Stage::Done);
        c.open = Some("less /tmp/log".into());
        let Plan::Show {
            body: Body::Text(t),
            ..
        } = plan(&c)
        else {
            panic!("expected text");
        };
        assert!(t.contains("No transcript was found for worker a93"));
        assert!(t.contains("less /tmp/log"));
        c.agent_id = None;
        assert!(matches!(plan(&c), Plan::Show { .. }));
    }

    #[test]
    fn a_done_row_resuming_a_conversation_goes_to_it() {
        let sid = "0b7c1c3e-8d7f-4c1e-9a55-2f6a1b0c9d11";
        let mut c = click(Stage::Done);
        c.open = Some(format!("claude --resume {sid}"));
        assert_eq!(done_resumes(&c), Some(sid.to_string()));
        c.stage = Stage::Running;
        assert_eq!(done_resumes(&c), None);
    }

    #[test]
    fn running_without_a_worker_id_runs_open() {
        let mut c = click(Stage::Running);
        c.agent_id = None;
        c.open = Some("claude --resume x".into());
        assert!(matches!(plan(&c), Plan::Run { .. }));
        c.agent_id = Some(String::new());
        assert!(matches!(plan(&c), Plan::Run { .. }));
        c.open = Some("   ".into());
        assert!(matches!(plan(&c), Plan::Show { .. }));
    }

    #[test]
    fn running_without_a_transcript_says_so() {
        let c = click(Stage::Running);
        let Plan::Show {
            body: Body::Text(t),
            ..
        } = plan(&c)
        else {
            panic!("expected text");
        };
        assert!(t.contains("No transcript for worker a93"));
    }

    #[test]
    fn planned_shows_brief_then_note() {
        let mut c = click(Stage::Planned);
        c.transcript = Some("/t/a.jsonl".into());
        c.note = Some("lane 2".into());
        let Plan::Show {
            title,
            body: Body::Text(t),
        } = plan(&c)
        else {
            panic!("expected text");
        };
        assert_eq!(title, "acme#158 · Wren");
        assert!(t.starts_with("Task: acme#158\n\nWren\n\nlane 2\n\n"), "{t}");
        assert!(t.contains("--brief FILE"), "{t}");
        c.brief = Some("/b/brief.md".into());
        assert_eq!(
            plan(&c),
            Plan::Show {
                title: "acme#158 · Wren".into(),
                body: Body::File("/b/brief.md".into())
            }
        );
        // No brief and no note: the task and its title, and how to add one.
        c.brief = None;
        c.note = None;
        c.name = "acme#158".into();
        let Plan::Show {
            body: Body::Text(t),
            ..
        } = plan(&c)
        else {
            panic!("expected text");
        };
        assert!(t.starts_with("Task: acme#158\n\nNo brief was given"), "{t}");
    }

    #[test]
    fn titles() {
        let mut c = click(Stage::Planned);
        c.name = "acme#158".into();
        assert_eq!(title_of(&c), "acme#158");
        c.key.clear();
        assert_eq!(title_of(&c), "acme#158");
        c.name.clear();
        assert_eq!(title_of(&c), "a93");
    }

    #[test]
    fn finds_the_resumed_session() {
        let sid = "0b7c1c3e-8d7f-4c1e-9a55-2f6a1b0c9d11";
        assert_eq!(
            resumed_session(&format!("cd /x && claude --resume {sid}")),
            Some(sid.into())
        );
        assert_eq!(
            resumed_session(&format!("claude -r '{sid}'")),
            Some(sid.into())
        );
        assert_eq!(
            resumed_session(&format!("claude --resume={sid} --fork")),
            Some(sid.into())
        );
        assert_eq!(resumed_session("claude --resume"), None);
        assert_eq!(resumed_session("less /tmp/log"), None);
    }

    #[test]
    fn brief_is_read_and_capped() {
        let dir = std::env::temp_dir().join(format!("giverny-brief-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small.md");
        std::fs::write(&small, "# Brief\nDo it.").unwrap();
        assert_eq!(read_brief(&small), "# Brief\nDo it.");
        let big = dir.join("big.md");
        std::fs::write(&big, vec![b'x'; BRIEF_MAX + 10]).unwrap();
        assert!(read_brief(&big).contains("… 10 more bytes"));
        // A long brief is read whole.
        let long = dir.join("long.md");
        std::fs::write(&long, "y".repeat(300 * 1024)).unwrap();
        assert_eq!(read_brief(&long).len(), 300 * 1024);
        assert!(read_brief(&dir.join("none.md")).starts_with("Could not read the brief"));
        std::fs::remove_dir_all(&dir).ok();
    }

    // ------------------------------------------------------ attach ----
    //
    // The screens below are Claude Code 2.1.280 (fullscreen tui), captured
    // from a real `claude` in a pty while building the attach.

    const TRANSCRIPT: &str = "\
❯ Spawn three general-purpose subagents in parallel
● 2 background agents launched (↓ to manage)
   ├ eta worker
   └ theta worker
✻ Waiting for 3 background agents to finish
";

    fn prompt_screen(text: &str) -> String {
        format!(
            "{TRANSCRIPT}──────────────────────────────\n❯ {text}\n──────────────────────────────\n  \
             Haiku 4.5  ·  5h 20%  ·  wk 86%\n  ⏸ manual mode on · ← 2 agents\n"
        )
    }

    fn list_screen(selected: usize) -> String {
        let rows = [
            "sleep 300",
            "wait for background sleep 240 to finish",
            "iota worker",
            "theta worker",
            "eta worker",
        ];
        let mut s = format!(
            "{TRANSCRIPT}▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔\n   Background\n   2 active shells · 3 active agents\n     Shells (2)\n"
        );
        for (i, r) in rows.iter().enumerate() {
            if i == 2 {
                s.push_str("     Local agents (3)\n");
            }
            let mark = if i == selected { "   ❯ " } else { "     " };
            let tail = if i < 2 { "" } else { " · Opus 5.5" };
            s.push_str(&format!("{mark}{r} (running){tail}\n"));
        }
        s.push_str(
            "   ↑/↓ to select · Enter to view · f to foreground · x to stop · \
             ctrl+x ctrl+k to stop all agents · Esc to close\n",
        );
        s
    }

    const DETAIL: &str = "\
▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔
   general-purpose › theta worker
   58s · 24.7k tokens · 1 tool · Opus 5.5
   Progress
   › Bash(python3 -c 'import time; time.sleep(240)')
   ← to go back · Esc/Enter/Space to close · x to stop · f to foreground · ctrl+x ctrl+k to stop all agents
";

    const PERMISSION: &str = "\
✻ Waiting for 2 background agents to finish
──────────────────────────────
 Bash command · from the general-purpose agent
   python3 -c 'import time; time.sleep(240)'
 Do you want to proceed?
 ❯ 1. Yes
   2. No
 Esc to cancel · Tab to amend
";

    #[test]
    fn reads_the_prompt_and_tells_a_draft_from_the_placeholder() {
        // The placeholder is dim: blank once dim cells are dropped.
        let shown = prompt_screen("Message @general-purpose…");
        let undimmed = prompt_screen("");
        assert_eq!(read_view(&shown, &undimmed), View::Prompt { draft: false });
        let draft = prompt_screen("half a thought");
        assert_eq!(read_view(&draft, &draft), View::Prompt { draft: true });
        // A draft on a second line counts too.
        let two = "──────\n❯ \n  second line\n──────\n";
        assert_eq!(read_view(two, two), View::Prompt { draft: true });
    }

    #[test]
    fn a_transcript_line_is_not_the_prompt() {
        assert_eq!(read_view(TRANSCRIPT, TRANSCRIPT), View::Other);
        assert_eq!(read_view(PERMISSION, PERMISSION), View::Other);
    }

    #[test]
    fn reads_the_background_list() {
        let s = list_screen(3);
        let View::List(items) = read_view(&s, &s) else {
            panic!("expected the list");
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "sleep 300",
                "wait for background sleep 240 to finish",
                "iota worker",
                "theta worker",
                "eta worker"
            ]
        );
        assert_eq!(items.iter().position(|i| i.selected), Some(3));
    }

    /// The footer strip under a worker's view, as the attach saw it (`─── <desc> ─`,
    /// the `Message @…` placeholder, `◯ main`), and the same strip on main.
    fn strip_screen(main: &str, pointer: bool) -> String {
        let p = if pointer { "❯ " } else { "  " };
        format!(
            "{TRANSCRIPT}─── theta worker ──────────────\n❯ Message @general-purpose…\n\
             ──────────────────────────────\n  {p}{main} main                 ↑ 1 more\n  \
             ◯ theta worker (running)\n"
        )
    }

    #[test]
    fn tells_a_worker_view_from_main() {
        assert!(viewing_worker(&strip_screen("◯", false)));
        assert!(viewing_worker(&strip_screen("◯", true)));
        assert!(viewing_worker(&strip_screen("( )", false)));
        assert!(!viewing_worker(&strip_screen("●", false)));
        assert!(!viewing_worker(&strip_screen("⏺", true)));
        // Main with the strip folded away, and the Background dialog.
        assert!(!viewing_worker(&prompt_screen("")));
        assert!(!viewing_worker(&list_screen(2)));
        assert!(!viewing_worker(DETAIL));
        // A transcript line that talks about the strip is not the strip.
        assert!(!viewing_worker(
            "● the worker view has a ◯ main strip to go back\n  ◯ main is how\n"
        ));
    }

    #[test]
    fn reads_a_detail_card() {
        assert_eq!(
            read_view(DETAIL, DETAIL),
            View::Detail("theta worker".into())
        );
    }

    #[test]
    fn items_keep_parentheses_of_their_own() {
        assert_eq!(
            parse_item("   ❯ fix (the) pane (running) · Opus 5.5"),
            Some(Item {
                label: "fix (the) pane".into(),
                selected: true,
                id: None,
            })
        );
        assert_eq!(parse_item("     Local agents (3)"), None);
        assert_eq!(parse_item("   3 active agents"), None);
    }

    #[test]
    fn labels_match_through_spacing_and_truncation() {
        assert!(label_matches("theta  worker", "theta worker"));
        assert!(label_matches(
            "demo#23 click a Run…",
            "demo#23 click a Running row"
        ));
        assert!(!label_matches("theta worker", "eta worker"));
        assert!(!label_matches("…", "eta worker"));
    }

    /// Drive an attach against a scripted screen until it stops.
    fn drive(a: &mut Attach, t: &mut Instant, screen: &str) -> Tick {
        loop {
            let tick = a.tick(*t, screen, screen);
            *t += Duration::from_millis(20);
            if tick != Tick::Wait {
                return tick;
            }
        }
    }

    // Claude Code 2.1.281 (fullscreen tui), captured from a real `claude`
    // in tmux while testing the attach: two background agents, two
    // background shells.

    const STRIP_TOP: &str = "\
● Theta worker is now also running its sleep 400 in the background.
✻ Crunched for 18s · done 1:02 AM · 2 shells still running
──────────────────────────────────────────────────────────────────
";

    fn strip_rows(selected: Option<usize>, hint: &str) -> String {
        let rows = [
            "● main",
            "◯ general-purpose  eta worker                          8s · ↓ 27.2k tokens",
            "◯ general-purpose  theta worker                        8s · ↓ 27.2k tokens",
        ];
        let mut out = format!("  {hint}\n");
        for (i, r) in rows.iter().enumerate() {
            let lead = if Some(i) == selected { "❯ " } else { "  " };
            out.push_str(&format!("{lead}{r}\n"));
        }
        out
    }

    /// The main prompt with the strip under it, unfocused.
    fn at_prompt(draft: &str) -> String {
        format!(
            "{STRIP_TOP}❯ {draft}\n──────────────────────────────────────────────────────────────────\n  \
             Haiku 4.5  ·  5h 11%  ·  wk 94%  ·  session: 36.2k  ·  subagents: 53.8k  ·  total: 90k\n{}",
            strip_rows(None, "⏸ manual mode on · 2 shells · ← 2 agents")
        )
    }

    /// The strip with the focus on row `at`.
    fn in_strip(at: usize) -> String {
        let hint = if at == 0 {
            "↑/↓ to select"
        } else {
            "Enter to view · x to stop · ctrl+x ctrl+k to stop all agents"
        };
        format!(
            "{STRIP_TOP}❯ \n──────────────────────────────────────────────────────────────────\n  \
             Haiku 4.5  ·  5h 11%  ·  wk 94%  ·  session: 36.2k  ·  subagents: 53.8k  ·  total: 90k\n{}",
            strip_rows(Some(at), hint)
        )
    }

    #[test]
    fn reads_the_agent_strip() {
        assert_eq!(
            read_view(&at_prompt(""), &at_prompt("")),
            View::Prompt { draft: false }
        );
        let View::Strip(items) = read_view(&in_strip(1), &in_strip(1)) else {
            panic!("expected the strip");
        };
        let labels: Vec<(&str, bool)> = items
            .iter()
            .map(|i| (i.label.as_str(), i.selected))
            .collect();
        assert_eq!(
            labels,
            vec![
                ("main", false),
                ("eta worker", true),
                ("theta worker", false)
            ]
        );
        // Without Unicode, and with a `↑ N more` row.
        assert_eq!(
            parse_strip_item("❯ ( ) general-purpose  iota worker   3s"),
            Some(Item {
                label: "iota worker".into(),
                selected: true,
                id: None,
            })
        );
        assert_eq!(parse_strip_item("↑ 2 more"), None);
        assert_eq!(parse_strip_item("Haiku 4.5  ·  5h 11%"), None);
    }

    /// A worker's view with the strip focused on `main`, verbatim.
    const WORKER_VIEW_STRIP: &str = "\
● The 400-second sleep is running in the background.
───────────────────────────────────────────────────── eta worker ─
❯ Message @general-purpose…
──────────────────────────────────────────────────────────────────
  Haiku 4.5  ·  5h 12%  ·  wk 94%  ·  session: 37.6k  ·  subagents: 80.3k  ·  total: 117.9k
  ↑/↓ to select · Enter to view

❯ ◯ main
  ● general-purpose  eta worker                          8s · ↓ 27.2k tokens
  ◯ general-purpose  theta worker                        8s · ↓ 27.2k tokens
";

    #[test]
    fn reads_the_strip_under_a_worker_view() {
        let View::Strip(items) = read_view(WORKER_VIEW_STRIP, WORKER_VIEW_STRIP) else {
            panic!("expected the strip");
        };
        assert_eq!(items.len(), 3);
        assert!(items[0].selected && items[0].label == "main");
        assert!(viewing_worker(WORKER_VIEW_STRIP));
    }

    #[test]
    fn attach_steps_down_the_strip_and_views_the_worker_typing_nothing() {
        let mut t = Instant::now();
        let mut a = Attach::new("theta worker", t);
        let prompt = at_prompt("");
        // ↓ out of the prompt box (one row) and past the shells pill.
        assert_eq!(drive(&mut a, &mut t, &prompt), Tick::Send(Keystroke::Down));
        assert_eq!(drive(&mut a, &mut t, &prompt), Tick::Send(Keystroke::Down));
        // The strip has the focus on `main`; the worker is two rows down.
        assert_eq!(
            drive(&mut a, &mut t, &in_strip(0)),
            Tick::Send(Keystroke::Down)
        );
        assert_eq!(
            drive(&mut a, &mut t, &in_strip(1)),
            Tick::Send(Keystroke::Down)
        );
        assert_eq!(
            drive(&mut a, &mut t, &in_strip(2)),
            Tick::Send(Keystroke::Enter)
        );
        assert_eq!(drive(&mut a, &mut t, &in_strip(2)), Tick::Done);
        // A Down too many is walked back.
        let mut b = Attach::new("eta worker", t);
        b.opened = true;
        assert_eq!(
            drive(&mut b, &mut t, &in_strip(2)),
            Tick::Send(Keystroke::Up)
        );
    }

    #[test]
    fn attach_goes_through_a_draft() {
        let mut t = Instant::now();
        let mut a = Attach::new("eta worker", t);
        let draft = format!(
            "{STRIP_TOP}❯ first line\n  second line\n  third\n──────────────────────────────\n{}",
            strip_rows(None, "⏸ manual mode on")
        );
        let mut downs = 0;
        loop {
            match drive(&mut a, &mut t, &draft) {
                Tick::Send(Keystroke::Down) => downs += 1,
                other => {
                    assert_eq!(other, Tick::Stuck(Stuck::NotListed));
                    break;
                }
            }
        }
        // Three draft rows, then one more for the shells pill; never a
        // stash, never text.
        assert_eq!(downs, 4);
    }

    #[test]
    fn attach_waits_for_a_key_to_land_before_the_next() {
        let mut t = Instant::now();
        let mut a = Attach::new("theta worker", t);
        a.opened = true;
        assert_eq!(a.tick(t, &in_strip(0), ""), Tick::Send(Keystroke::Down));
        // Same screen: the Down has not landed yet, so no second Down.
        t += Duration::from_millis(100);
        assert_eq!(a.tick(t, &in_strip(0), ""), Tick::Wait);
        // Keys go out at least KEY_GAP apart.
        let mut b = Attach::new("x", t);
        let prompt = at_prompt("");
        assert_eq!(b.tick(t, &prompt, &prompt), Tick::Send(Keystroke::Down));
        assert_eq!(
            b.tick(t + Duration::from_millis(5), &prompt, &prompt),
            Tick::Wait
        );
        assert_eq!(
            b.tick(t + KEY_GAP, &prompt, &prompt),
            Tick::Send(Keystroke::Down)
        );
    }

    #[test]
    fn attach_stops_rather_than_type_blind() {
        let mut t = Instant::now();
        let mut a = Attach::new("eta worker", t);
        assert_eq!(
            drive(&mut a, &mut t, PERMISSION),
            Tick::Stuck(Stuck::NoPrompt)
        );
        // The /tasks dialog is up: not the prompt, nothing is typed.
        let mut l = Attach::new("eta worker", t);
        assert_eq!(
            drive(&mut l, &mut t, &list_screen(0)),
            Tick::Stuck(Stuck::NoPrompt)
        );
        let mut b = Attach::new("kappa worker", t);
        assert_eq!(
            drive(&mut b, &mut t, &in_strip(0)),
            Tick::Stuck(Stuck::NotListed)
        );
        let twice = in_strip(0).replace("theta worker", "eta worker");
        let mut c = Attach::new("eta worker", t);
        assert_eq!(drive(&mut c, &mut t, &twice), Tick::Stuck(Stuck::Ambiguous));
        // ↓ went out and no strip came: no agent to view.
        let mut d = Attach::new("eta worker", t);
        let bare = prompt_screen("");
        assert_eq!(drive(&mut d, &mut t, &bare), Tick::Send(Keystroke::Down));
        assert_eq!(drive(&mut d, &mut t, &bare), Tick::Send(Keystroke::Down));
        assert_eq!(drive(&mut d, &mut t, &bare), Tick::Stuck(Stuck::NotListed));
    }

    // ------------------------------------------------------- walk ----
    //
    // A fake Claude Code 2.1.281 that draws the screens seen in tmux during
    // the strip and back-to-main work (main view, a worker's view, the strip focused or
    // not, the shells pill focused, the agents pane's relay hiding rows)
    // and answers keys the way that one did.

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Focus {
        Prompt,
        Pill,
        /// On the strip, at this visible row.
        Strip(usize),
    }

    struct Fake {
        /// 0 is main; 1.. the agents.
        view: usize,
        focus: Focus,
        /// The agents pane is on: the relay hides every agent row.
        pane: bool,
        /// The relay has been asked to show the strip anyway.
        asked: bool,
        /// The relay tags each row it shows on an ask with the agent's id:
        /// `◯ [<id>] <label>`.
        tagged: bool,
        /// A background shell: the footer's `1 shell` pill.
        shells: bool,
        /// (description, strip label) per agent.
        agents: Vec<(&'static str, &'static str)>,
        /// What an ↑ at the prompt recalled, if one ever did.
        recalled: bool,
        /// The Background dialog (`/tasks`'s): opened by ↓ on the pill
        /// with no strip under it.
        tasks_dialog: bool,
        permission: bool,
        keys: usize,
    }

    impl Fake {
        fn new() -> Fake {
            Fake {
                view: 0,
                focus: Focus::Prompt,
                pane: true,
                asked: false,
                tagged: false,
                shells: true,
                agents: vec![
                    ("eta worker", "Starting Python sleep"),
                    ("theta worker", "theta worker"),
                ],
                recalled: false,
                tasks_dialog: false,
                permission: false,
                keys: 0,
            }
        }

        /// The strip rows drawn now, as views (0 = main).
        fn rows(&self) -> Vec<usize> {
            if !self.pane || self.asked {
                (0..=self.agents.len()).collect()
            } else if self.view != 0 {
                // Claude Code keeps `main` while a worker's view is up.
                vec![0]
            } else {
                vec![]
            }
        }

        /// A focus the strip can no longer hold falls back the way Claude
        /// Code's does: to the pill, else the prompt.
        fn settle(&mut self) {
            if let Focus::Strip(at) = self.focus
                && at >= self.rows().len()
            {
                self.focus = if self.shells {
                    Focus::Pill
                } else {
                    Focus::Prompt
                };
            }
        }

        /// The screen, the same with dim cells blanked, and the cursor row.
        fn look(&mut self) -> (String, String, Option<usize>) {
            self.settle();
            if self.permission {
                return (PERMISSION.into(), PERMISSION.into(), None);
            }
            if self.tasks_dialog {
                let s = list_screen(0);
                return (s.clone(), s, None);
            }
            let rule = "─".repeat(60);
            let top = match self.view {
                0 => rule.clone(),
                v => format!("{rule} {} ─", self.agents[v - 1].0),
            };
            let placeholder = if self.view == 0 {
                ""
            } else {
                "Message @general-purpose…"
            };
            let prompt = if self.recalled {
                "an old prompt"
            } else {
                placeholder
            };
            let bright = if self.recalled { "an old prompt" } else { "" };
            let pill = if self.shells { " · 1 shell" } else { "" };
            let hint = match self.focus {
                Focus::Strip(_) => "↑/↓ to select · Enter to view".to_string(),
                Focus::Pill => format!("⏸ manual mode on{pill}"),
                Focus::Prompt => format!("⏸ manual mode on{pill} · ← 2 agents"),
            };
            let mut strip = String::new();
            let rows = self.rows();
            if !rows.is_empty() {
                strip.push('\n');
            }
            for (i, &v) in rows.iter().enumerate() {
                let ptr = if self.focus == Focus::Strip(i) {
                    "❯ "
                } else {
                    "  "
                };
                let dot = if self.view == v { "●" } else { "◯" };
                if v == 0 {
                    strip.push_str(&format!("{ptr}{dot} main\n"));
                } else if self.tagged && self.pane && self.asked {
                    let label = self.agents[v - 1].1;
                    let id = fake_id(v);
                    strip.push_str(&format!("{ptr}{dot} [{id}] {label}\n"));
                } else {
                    let label = self.agents[v - 1].1;
                    strip.push_str(&format!(
                        "{ptr}{dot} general-purpose  {label}          8s · ↓ 2k tokens\n"
                    ));
                }
            }
            let head = "● Launched.\n✻ Waiting for 2 background agents to finish\n";
            let foot = format!(
                "{rule}\n  Haiku 4.5  ·  5h 84%  ·  session: 36.2k  ·  subagents: 53.8k  ·  total: 90k\n  {hint}\n{strip}"
            );
            let screen = format!("{head}{top}\n❯ {prompt}\n{foot}");
            let undimmed = format!("{head}{top}\n❯ {bright}\n{foot}");
            let cursor = (self.focus == Focus::Prompt).then_some(3);
            (screen, undimmed, cursor)
        }

        fn key(&mut self, k: Keystroke) {
            self.keys += 1;
            self.settle();
            let n = self.rows().len();
            self.focus = match (k, self.focus) {
                (Keystroke::Down, Focus::Prompt) if self.shells => Focus::Pill,
                (Keystroke::Down, Focus::Prompt | Focus::Pill) if n > 0 => Focus::Strip(0),
                (Keystroke::Down, Focus::Pill) => {
                    self.tasks_dialog = true;
                    Focus::Pill
                }
                (Keystroke::Down, f @ Focus::Prompt) => f,
                (Keystroke::Down, Focus::Strip(at)) => Focus::Strip((at + 1).min(n - 1)),
                (Keystroke::Up, Focus::Strip(0)) if self.shells => Focus::Pill,
                (Keystroke::Up, Focus::Strip(0)) => Focus::Prompt,
                (Keystroke::Up, Focus::Strip(at)) => Focus::Strip(at - 1),
                (Keystroke::Up, Focus::Pill) => Focus::Prompt,
                (Keystroke::Up, Focus::Prompt) => {
                    self.recalled = true;
                    Focus::Prompt
                }
                (Keystroke::Enter, Focus::Strip(at)) => {
                    self.view = self.rows()[at];
                    Focus::Strip(at)
                }
                (Keystroke::Enter, Focus::Pill) => {
                    self.tasks_dialog = true;
                    Focus::Pill
                }
                (Keystroke::Enter, f @ Focus::Prompt) => f,
            };
            self.settle();
        }
    }

    /// The fake's agent id for view `v`.
    fn fake_id(v: usize) -> String {
        format!("a{v:016x}")
    }

    /// Drive a walk against the fake until it stops; `each` runs before
    /// every frame (to change the fake mid-walk).
    fn run_walk(w: &mut Walk, fake: &mut Fake, mut each: impl FnMut(&mut Fake, Duration)) -> Tick {
        let start = Instant::now();
        let mut t = start;
        loop {
            each(fake, t - start);
            let (screen, undimmed, cursor) = fake.look();
            let look = Look {
                screen: &screen,
                undimmed: &undimmed,
                cursor,
            };
            match w.tick(t, look) {
                Tick::Send(k) => fake.key(k),
                Tick::Wait => {}
                other => return other,
            }
            t += Duration::from_millis(20);
            assert!(t - start < Duration::from_secs(60), "the walk never ended");
        }
    }

    fn open_eta() -> Walk {
        Walk::open(
            "eta worker",
            vec!["Starting Python sleep".into()],
            Instant::now(),
        )
    }

    /// Where every walk must leave the fake: typing goes to the prompt,
    /// and nothing was recalled or opened on the way.
    fn assert_clean(fake: &Fake) {
        assert_eq!(fake.focus, Focus::Prompt, "the prompt has the keyboard");
        assert!(!fake.recalled, "an ↑ reached the prompt");
        assert!(!fake.tasks_dialog, "the Background dialog was opened");
    }

    #[test]
    fn open_waits_for_the_asked_strip_then_lands_in_the_workers_prompt() {
        let mut fake = Fake::new();
        let mut w = open_eta();
        assert!(w.wants_strip());
        // The relay answers the ask on its next run, four seconds in.
        let done = run_walk(&mut w, &mut fake, |f, at| {
            if at >= Duration::from_secs(4) {
                f.asked = true;
            }
        });
        assert_eq!(done, Tick::Done);
        assert_eq!(fake.view, 1, "on eta worker's view");
        assert!(!w.wants_strip(), "let go once the focus is off the strip");
        assert!(!w.entering());
        assert_clean(&fake);
    }

    #[test]
    fn the_strip_goes_as_soon_as_the_focus_is_off_it_and_the_pill_costs_no_settle() {
        // The relay answers every ask at once, both ways: the rows go the
        // frame the walk lets them, and nothing is typed into a strip
        // that is going away.
        for shells in [true, false] {
            let mut fake = Fake::new();
            fake.shells = shells;
            fake.tagged = true;
            let mut w = open_eta().by_id(fake_id(1));
            let start = Instant::now();
            let mut t = start;
            let done = loop {
                fake.asked = w.wants_strip();
                let (screen, undimmed, cursor) = fake.look();
                let look = Look {
                    screen: &screen,
                    undimmed: &undimmed,
                    cursor,
                };
                match w.tick(t, look) {
                    Tick::Send(k) => {
                        assert!(
                            !(k == Keystroke::Up
                                && !w.wants_strip()
                                && fake.focus == Focus::Prompt),
                            "an ↑ went to the prompt"
                        );
                        fake.key(k);
                    }
                    Tick::Wait => {}
                    other => break other,
                }
                t += Duration::from_millis(20);
                assert!(t - start < Duration::from_secs(20), "the walk never ended");
            };
            assert_eq!(done, Tick::Done, "shells: {shells}");
            assert_eq!(fake.view, 1);
            assert_clean(&fake);
            assert!(!w.wants_strip());
            // The shells pill is left after FOCUS_CALM, not a full SETTLE.
            assert!(t - start < SETTLE, "shells: {shells}: took {:?}", t - start);
        }
    }

    #[test]
    fn settle_waits_for_the_strips_rows_to_go_then_for_calm() {
        let t = Instant::now();
        let goal = Goal::Worker {
            description: "eta worker".into(),
            aliases: vec![],
            agent_id: None,
        };
        let mut s = Settle::new(&goal, true, t);
        // In the view, the strip still showing its agent rows.
        assert!(!s.done(t, WORKER_VIEW_STRIP));
        assert!(!s.done(t + SETTLE_CALM * 3, WORKER_VIEW_STRIP));
        // The relay hid them: only `main` is left.
        let hidden: String = WORKER_VIEW_STRIP
            .lines()
            .filter(|l| !l.contains("general-purpose  "))
            .map(|l| format!("{l}\n"))
            .collect();
        let at = t + Duration::from_millis(400);
        assert!(!s.done(at, &hidden), "not before it holds");
        assert!(s.done(at + SETTLE_CALM, &hidden));
        // With the pane off the strip is Claude Code's: the view is enough.
        let mut off = Settle::new(&goal, false, t);
        assert!(!off.done(t, WORKER_VIEW_STRIP));
        assert!(off.done(t + SETTLE_CALM, WORKER_VIEW_STRIP));
        // Never longer than SETTLE_MAX.
        let mut late = Settle::new(&goal, true, t);
        assert!(late.done(t + SETTLE_MAX, WORKER_VIEW_STRIP));
    }

    #[test]
    fn settle_home_is_main_on_screen() {
        let t = Instant::now();
        let mut s = Settle::new(&Goal::Main, true, t);
        assert!(!s.done(t, WORKER_VIEW_STRIP));
        assert!(!s.done(t, &at_prompt("")));
        assert!(s.done(t + SETTLE_CALM, &at_prompt("")));
    }

    #[test]
    fn a_nudge_narrows_then_restores_and_gives_up_after_nudge_max() {
        let t = Instant::now();
        let mut n = Nudge::default();
        assert_eq!(n.tick(t, false), None, "nothing waited on");
        assert_eq!(n.tick(t, true), Some(Width::Narrow));
        assert!(n.is_narrow());
        assert_eq!(n.tick(t + NUDGE_NARROW / 2, true), None);
        let back = t + NUDGE_NARROW;
        assert_eq!(n.tick(back, true), Some(Width::Restore));
        assert!(!n.is_narrow());
        // Still waited on: again, but not at once.
        assert_eq!(n.tick(back + NUDGE_AGAIN / 2, true), None);
        let mut at = back + NUDGE_AGAIN;
        for _ in 1..NUDGE_MAX {
            assert_eq!(n.tick(at, true), Some(Width::Narrow));
            at += NUDGE_NARROW;
            assert_eq!(n.tick(at, true), Some(Width::Restore));
            at += NUDGE_AGAIN;
        }
        assert_eq!(n.tick(at, true), None, "NUDGE_MAX is the most");
        // A new ask starts over.
        n.ask(at);
        assert_eq!(n.tick(at, true), Some(Width::Narrow));
        // A narrow pty always comes back, waited on or not.
        assert_eq!(n.tick(at + NUDGE_NARROW, false), Some(Width::Restore));
    }

    #[test]
    fn a_held_nudge_stays_narrow_until_the_answer_is_in() {
        let t = Instant::now();
        let mut n = Nudge::default();
        n.ask_held();
        assert_eq!(n.tick(t, true), Some(Width::Narrow));
        assert_eq!(n.tick(t + NUDGE_NARROW * 3, true), None, "still narrow");
        let answer = t + Duration::from_millis(350);
        assert_eq!(n.tick(answer, false), Some(Width::Restore));
        assert!(!n.is_narrow());
        // No answer at all: it comes back after NUDGE_HOLD, and tries again.
        n.ask_held();
        assert_eq!(n.tick(t, true), Some(Width::Narrow));
        assert_eq!(n.tick(t + NUDGE_HOLD, true), Some(Width::Restore));
        let again = t + NUDGE_HOLD + NUDGE_AGAIN;
        assert_eq!(n.tick(again, true), Some(Width::Narrow));
        // A plain ask is back to the short narrowing.
        n.ask(again);
        assert_eq!(n.tick(again + NUDGE_NARROW, true), Some(Width::Restore));
    }

    #[test]
    fn an_ask_right_after_a_nudge_is_left_to_the_run_already_coming() {
        let t = Instant::now();
        let mut n = Nudge::default();
        n.ask_held();
        assert_eq!(n.tick(t, true), Some(Width::Narrow));
        let back = t + Duration::from_millis(340);
        assert_eq!(n.tick(back, false), Some(Width::Restore));
        // Asked again 0.2 s later: Claude Code's run after that restore
        // has not happened yet, and will read the new ask.
        let soon = back + Duration::from_millis(200);
        n.ask(soon);
        assert_eq!(n.tick(soon, true), None);
        assert_eq!(
            n.tick(back + NUDGE_AGAIN, true),
            Some(Width::Narrow),
            "unless it misses"
        );
        // Asked once that run is past: nudged at once.
        let mut m = Nudge::default();
        assert_eq!(m.tick(t, true), Some(Width::Narrow));
        assert_eq!(m.tick(t + NUDGE_NARROW, false), Some(Width::Restore));
        let late = t + NUDGE_NARROW + CC_TICK_AFTER_RESIZE;
        m.ask(late);
        assert_eq!(m.tick(late, true), Some(Width::Narrow));
    }

    #[test]
    fn open_types_nothing_while_the_strip_is_hidden() {
        let mut fake = Fake::new();
        let mut w = open_eta();
        let stuck = run_walk(&mut w, &mut fake, |_, _| {});
        assert_eq!(stuck, Tick::Stuck(Stuck::NoStrip));
        assert_eq!(fake.keys, 0, "↓ on the pill would open /tasks");
        assert_clean(&fake);
    }

    #[test]
    fn open_with_the_pane_off_and_no_shells() {
        let mut fake = Fake::new();
        fake.pane = false;
        fake.shells = false;
        let mut w = Walk::open("theta worker", vec![], Instant::now());
        assert_eq!(run_walk(&mut w, &mut fake, |_, _| {}), Tick::Done);
        assert_eq!(fake.view, 2);
        assert_clean(&fake);
    }

    #[test]
    fn open_on_the_workers_own_view_types_nothing() {
        let mut fake = Fake::new();
        fake.view = 1;
        let mut w = open_eta();
        assert_eq!(run_walk(&mut w, &mut fake, |_, _| {}), Tick::Done);
        assert_eq!(fake.keys, 0);
        assert_clean(&fake);
    }

    #[test]
    fn open_reports_the_wrong_view_and_stays() {
        let mut fake = Fake::new();
        fake.asked = true;
        // The tracker's label for eta is theta's strip label.
        let mut w = Walk::open("eta worker", vec!["theta worker".into()], Instant::now());
        assert_eq!(
            run_walk(&mut w, &mut fake, |_, _| {}),
            Tick::Stuck(Stuck::WrongView("theta worker".into()))
        );
        assert_eq!(fake.view, 2);
    }

    /// The strip in a parent whose workers are busy, one of them resumed
    /// with SendMessage, as Claude Code 2.1.283 drew it in tmux:
    /// each row is labelled with the worker's progress
    /// summary, never its description (`gamma busy worker`, `beta resumed
    /// worker`), and the summary moves on every thirty seconds.
    const STRIP_SUMMARIES: &str = "\
✻ Waiting for 2 background agents to finish
────────────────────────────────────────────────────────────
❯ 
────────────────────────────────────────────────────────────
  Haiku 4.5  ·  5h 8%  ·  wk 39%  ·  session 39.3k  ·  total: 106.2k
  Enter to view · x to stop · ctrl+x ctrl+k to stop all agents

  ● main
  ◯ general-purpose  Running foreground timeout command                          6m 24s · ↓ 22.7k tokens
❯ ◯ general-purpose  Running hold-beta timeout commands                          6m 24s · ↓ 22.7k tokens
";

    #[test]
    fn a_busy_or_resumed_workers_row_is_its_summary_and_no_name_finds_it() {
        let View::Strip(items) = read_view(STRIP_SUMMARIES, STRIP_SUMMARIES) else {
            panic!("expected the strip");
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "main",
                "Running foreground timeout command",
                "Running hold-beta timeout commands"
            ]
        );
        assert!(items.iter().all(|i| i.id.is_none()));
        // What a real session hit: by description (and the pane's own activity) the
        // worker is not on the strip, and with two rows no guess is made.
        let t = Instant::now();
        let mut a = Attach::new("beta resumed worker", t)
            .with_aliases(["Bash: Hold beta for thirty seconds".to_string()])
            .or_only_agent()
            .with_id(Some("a2d06f3ac3b6e7671".into()));
        assert_eq!(
            a.tick(t, STRIP_SUMMARIES, STRIP_SUMMARIES),
            Tick::Stuck(Stuck::NotListed)
        );
    }

    /// The same strip once the relay tags its rows.
    const STRIP_TAGGED: &str = "\
✻ Waiting for 2 background agents to finish
────────────────────────────────────────────────────────────
❯ 
────────────────────────────────────────────────────────────
  Haiku 4.5  ·  5h 8%  ·  wk 39%  ·  session 39.3k  ·  total: 106.2k
  Enter to view · x to stop · ctrl+x ctrl+k to stop all agents

  ● main
❯ ◯ [a95d7f3452a3597df] Running foreground timeout command
  ◯ [a2d06f3ac3b6e7671] Running hold-beta timeout commands
";

    #[test]
    fn a_tagged_strip_is_walked_by_agent_id() {
        let View::Strip(items) = read_view(STRIP_TAGGED, STRIP_TAGGED) else {
            panic!("expected the strip");
        };
        let ids: Vec<Option<&str>> = items.iter().map(|i| i.id.as_deref()).collect();
        assert_eq!(
            ids,
            [None, Some("a95d7f3452a3597df"), Some("a2d06f3ac3b6e7671")]
        );
        assert_eq!(items[2].label, "Running hold-beta timeout commands");
        assert_eq!(strip_agents(STRIP_TAGGED).len(), 2);
        let t = Instant::now();
        let mut a = Attach::new("beta resumed worker", t)
            .or_only_agent()
            .with_id(Some("a2d06f3ac3b6e7671".into()));
        assert_eq!(
            a.tick(t, STRIP_TAGGED, STRIP_TAGGED),
            Tick::Send(Keystroke::Down)
        );
        // An id the strip does not carry is not there, whatever the labels.
        let mut gone = Attach::new("Running hold-beta timeout commands", t)
            .or_only_agent()
            .with_id(Some("a0000000000000000".into()));
        assert_eq!(
            gone.tick(t, STRIP_TAGGED, STRIP_TAGGED),
            Tick::Stuck(Stuck::NotListed)
        );
    }

    #[test]
    fn open_finds_a_busy_worker_by_id_on_the_tagged_strip() {
        let mut fake = Fake::new();
        fake.tagged = true;
        fake.agents = vec![
            ("eta worker", "Reading agent_open.rs"),
            ("theta worker", "Running cargo test"),
            ("iota worker", "Reading agent_open.rs"),
        ];
        for (v, name) in [(2, "theta worker"), (3, "iota worker"), (1, "eta worker")] {
            fake.view = 0;
            fake.asked = false;
            let mut w = Walk::open(name, vec![], Instant::now()).by_id(fake_id(v));
            let done = run_walk(&mut w, &mut fake, |f, at| {
                if at >= Duration::from_secs(1) {
                    f.asked = true;
                }
            });
            assert_eq!(done, Tick::Done, "{name}");
            assert_eq!(fake.view, v, "on {name}'s view");
            assert_clean(&fake);
        }
        // An untagged strip (an older relay, or the pane off): the labels
        // are still the way in.
        let mut fake = Fake::new();
        fake.pane = false;
        let mut w = Walk::open("theta worker", vec![], Instant::now()).by_id(fake_id(2));
        assert_eq!(run_walk(&mut w, &mut fake, |_, _| {}), Tick::Done);
        assert_eq!(fake.view, 2);
    }

    #[test]
    fn a_walk_never_types_over_a_dialog() {
        let mut fake = Fake::new();
        fake.permission = true;
        for mut w in [open_eta(), Walk::home(Instant::now())] {
            assert_eq!(
                run_walk(&mut w, &mut fake, |_, _| {}),
                Tick::Stuck(Stuck::NoPrompt)
            );
        }
        assert_eq!(fake.keys, 0);
    }

    #[test]
    fn home_from_a_workers_view_under_the_pane() {
        // The relay hides the agent rows; `main` alone is left.
        let mut fake = Fake::new();
        fake.view = 1;
        let mut w = Walk::home(Instant::now());
        assert!(!w.wants_strip(), "going home needs no ask");
        assert_eq!(run_walk(&mut w, &mut fake, |_, _| {}), Tick::Done);
        assert_eq!(fake.view, 0);
        // Enter on `main` left the focus on the shells pill; one ↑ took it
        // back to the prompt.
        assert_clean(&fake);
    }

    #[test]
    fn home_with_the_whole_strip_up_walks_the_selection_back_out() {
        let mut fake = Fake::new();
        fake.asked = true;
        fake.shells = false;
        fake.view = 2;
        fake.focus = Focus::Strip(2);
        let mut w = Walk::home(Instant::now());
        assert_eq!(run_walk(&mut w, &mut fake, |_, _| {}), Tick::Done);
        assert_eq!(fake.view, 0);
        assert_clean(&fake);
    }

    #[test]
    fn home_on_main_does_nothing() {
        let mut fake = Fake::new();
        let mut w = Walk::home(Instant::now());
        assert_eq!(run_walk(&mut w, &mut fake, |_, _| {}), Tick::Done);
        assert_eq!(fake.keys, 0);
        assert_clean(&fake);
    }

    #[test]
    fn open_then_home_round_trip() {
        let mut fake = Fake::new();
        fake.asked = true;
        let mut w = open_eta();
        assert_eq!(run_walk(&mut w, &mut fake, |_, _| {}), Tick::Done);
        assert_eq!(fake.view, 1);
        // The ask is dropped once the view is open.
        fake.asked = false;
        let mut h = Walk::home(Instant::now());
        assert_eq!(run_walk(&mut h, &mut fake, |_, _| {}), Tick::Done);
        assert_eq!(fake.view, 0);
        assert_clean(&fake);
    }

    #[test]
    fn reads_the_strip_agents_and_the_prompts_focus() {
        let mut fake = Fake::new();
        let (screen, _, cursor) = fake.look();
        assert!(strip_agents(&screen).is_empty(), "hidden on main");
        assert!(prompt_focused(&screen, cursor));
        assert!(!prompt_focused(&screen, None));
        assert!(!prompt_focused(&screen, Some(0)), "not the prompt's row");
        fake.view = 1;
        let (screen, _, _) = fake.look();
        assert!(strip_shown(&screen) && strip_agents(&screen).is_empty());
        fake.asked = true;
        let (screen, _, _) = fake.look();
        let labels: Vec<String> = strip_agents(&screen).into_iter().map(|i| i.label).collect();
        assert_eq!(labels, ["Starting Python sleep", "theta worker"]);
    }

    #[test]
    fn marks_hide_main_and_put_the_way_back_where_it_was() {
        let marks = row_marks(WORKER_VIEW_STRIP);
        let rows: Vec<&str> = WORKER_VIEW_STRIP.lines().collect();
        assert_eq!(marks.hidden, vec![7], "the `❯ ◯ main` row");
        assert!(rows[7].contains("◯ main"));
        let (row, used) = marks.button.expect("a worker's view has the button");
        assert_eq!((row, used), (7, 0), "on the hidden `main` row");
        assert_eq!(marks.button_left, Some(2), "at the dot, under the text");
        assert!(marks.escape);
        // Not pointed at: the dot is where it is all the same.
        let unpointed = WORKER_VIEW_STRIP.replace("❯ ◯ main", "  ◯ main");
        let marks = row_marks(&unpointed);
        assert_eq!((marks.button, marks.button_left), (Some((7, 0)), Some(2)));
        // On main: `main` still hidden, no button, Esc is Claude Code's.
        let main = at_prompt("");
        let marks = row_marks(&main);
        let rows: Vec<&str> = main.lines().collect();
        assert_eq!(marks.hidden.len(), 1);
        assert!(rows[marks.hidden[0] as usize].contains("● main"));
        assert_eq!(marks.button, None);
        assert!(!marks.escape);
        // A question up in a worker's view: its Esc is its own.
        let asking = format!("{PERMISSION}──────\n◯ main\n");
        assert!(!row_marks(&asking).escape);
        // No strip at all: nothing to paint.
        assert_eq!(row_marks(&prompt_screen("")), Default::default());
    }

    #[test]
    fn reads_the_prompt_box_rule_and_the_main_row() {
        let (screen, undimmed, _) = Fake::new().look();
        assert_eq!(
            prompt_box(&screen, &undimmed),
            Some(PromptBox {
                label: None,
                draft: false,
                rows: 3..4,
            })
        );
        assert!(!viewing_worker(&screen));
        assert_eq!(
            prompt_box(WORKER_VIEW_STRIP, WORKER_VIEW_STRIP)
                .unwrap()
                .label
                .as_deref(),
            Some("eta worker")
        );
        assert!(strip_shown(WORKER_VIEW_STRIP));
        assert!(!strip_shown(&prompt_screen("")));
    }

    #[test]
    fn keystrokes_encode_like_typed_keys() {
        let enc = |k: egui::Key, m: egui::Modifiers| -> Option<Vec<u8>> {
            Some(format!("{k:?}{}", if m.ctrl { "+ctrl" } else { "" }).into_bytes())
        };
        assert_eq!(keystroke_bytes(Keystroke::Down, enc), b"ArrowDown");
        assert_eq!(keystroke_bytes(Keystroke::Enter, enc), b"Enter");
        assert!(keystroke_bytes(Keystroke::Up, |_, _| None).is_empty());
    }

    /// The attach keys against a real Claude Code in tmux:
    /// `GIVERNY_TMUX=<socket>:<target>`, a `claude` there with the agent
    /// `GIVERNY_TMUX_AGENT` (a description) in its strip.
    /// Run by hand: `cargo test -p giverny live_tmux -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_tmux() {
        use std::process::Command;
        let spec = std::env::var("GIVERNY_TMUX").expect("GIVERNY_TMUX=<socket>:<target>");
        let (sock, target) = spec.split_once(':').unwrap();
        let tmux = |args: &[&str]| {
            let out = Command::new("tmux")
                .args(["-L", sock])
                .args(args)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let screen = || tmux(&["capture-pane", "-p", "-t", target]);
        let send = |bytes: &[u8]| {
            let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let mut args = vec!["send-keys", "-t", target, "-H"];
            args.extend(hex.iter().map(String::as_str));
            tmux(&args);
        };
        let enc = |k: egui::Key, _m: egui::Modifiers| -> Option<Vec<u8>> {
            Some(
                match k {
                    egui::Key::ArrowUp => "\x1b[A",
                    egui::Key::ArrowDown => "\x1b[B",
                    egui::Key::Enter => "\r",
                    _ => return None,
                }
                .as_bytes()
                .to_vec(),
            )
        };
        let run = |tick: &mut dyn FnMut(Instant, &str) -> Tick| {
            let started = Instant::now();
            let mut sent = Vec::new();
            loop {
                let s = screen();
                match tick(Instant::now(), &s) {
                    Tick::Send(k) => {
                        sent.push(format!("{k:?}"));
                        send(&keystroke_bytes(k, enc));
                    }
                    Tick::Wait => {}
                    other => {
                        println!("{other:?} after {:?}: {sent:?}", started.elapsed());
                        return other;
                    }
                }
                std::thread::sleep(Duration::from_millis(16));
            }
        };
        let agent = std::env::var("GIVERNY_TMUX_AGENT").unwrap();
        let mut a = Attach::new(agent, Instant::now());
        let done = run(&mut |now, s| a.tick(now, s, s));
        println!("{}", screen());
        assert_eq!(done, Tick::Done);
    }

    /// `tmux capture-pane -e` → (the text, the text with dim cells blank).
    fn undim(ansi: &str) -> (String, String) {
        let (mut plain, mut bright) = (String::new(), String::new());
        let mut dim = false;
        let mut chars = ansi.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                plain.push(c);
                bright.push(if dim && c != '\n' { ' ' } else { c });
                continue;
            }
            match chars.next() {
                Some('[') => {
                    let mut params = String::new();
                    for p in chars.by_ref() {
                        if p.is_ascii_alphabetic() {
                            if p == 'm' {
                                let mut it = params.split(';');
                                while let Some(n) = it.next() {
                                    match n {
                                        "" | "0" | "22" => dim = false,
                                        "2" => dim = true,
                                        "38" | "48" | "58" => {
                                            if it.next() == Some("2") {
                                                it.nth(2);
                                            } else {
                                                it.next();
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            break;
                        }
                        params.push(p);
                    }
                }
                // OSC (hyperlinks): to BEL or ESC \.
                Some(']') => {
                    while let Some(p) = chars.next() {
                        if p == '\x07' || (p == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        (plain, bright)
    }

    /// Into a worker's view and back against a real Claude Code in tmux:
    /// `GIVERNY_TMUX=<socket>:<target>`, a `claude` there
    /// with the running agent `GIVERNY_TMUX_AGENT` (its description; the
    /// strip's label for it, if different, in `GIVERNY_TMUX_ALIAS`). With
    /// the agents pane on, `GIVERNY_TMUX_ASK` names the relay's ask file
    /// (`<state>/show-strip/<tab id>`): it is written for the way in and
    /// removed once the view is open, as the app does; `GIVERNY_TMUX_ID`
    /// is the worker's agent id. Run by hand:
    /// `cargo test -p giverny live_tmux_walk -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_tmux_walk() {
        use std::process::Command;
        let spec = std::env::var("GIVERNY_TMUX").expect("GIVERNY_TMUX=<socket>:<target>");
        let (sock, target) = spec.split_once(':').unwrap();
        let tmux = |args: &[&str]| {
            let out = Command::new("tmux")
                .args(["-L", sock])
                .args(args)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let send = |bytes: &[u8]| {
            let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let mut args = vec!["send-keys", "-t", target, "-H"];
            args.extend(hex.iter().map(String::as_str));
            tmux(&args);
        };
        let enc = |k: egui::Key, _m: egui::Modifiers| -> Option<Vec<u8>> {
            Some(
                match k {
                    egui::Key::ArrowUp => "\x1b[A",
                    egui::Key::ArrowDown => "\x1b[B",
                    egui::Key::Enter => "\r",
                    _ => return None,
                }
                .as_bytes()
                .to_vec(),
            )
        };
        let ask = std::env::var("GIVERNY_TMUX_ASK").ok().map(PathBuf::from);
        let run = |w: &mut Walk| {
            let started = Instant::now();
            let mut log = Vec::new();
            let tick = loop {
                if let Some(ask) = &ask {
                    if w.wants_strip() {
                        std::fs::create_dir_all(ask.parent().unwrap()).unwrap();
                        std::fs::write(ask, b"").unwrap();
                    } else {
                        let _ = std::fs::remove_file(ask);
                    }
                }
                let (screen, undimmed) = undim(&tmux(&["capture-pane", "-p", "-e", "-t", target]));
                let cur = tmux(&["display", "-p", "-t", target, "#{cursor_y} #{cursor_flag}"]);
                let mut cur = cur.split_whitespace();
                let (y, shown) = (cur.next().unwrap_or("0"), cur.next() == Some("1"));
                let cursor = shown.then(|| y.parse().unwrap_or(0));
                let look = Look {
                    screen: &screen,
                    undimmed: &undimmed,
                    cursor,
                };
                match w.tick(Instant::now(), look) {
                    Tick::Send(k) => {
                        log.push(format!("{:?} {k:?}", started.elapsed()));
                        send(&keystroke_bytes(k, enc));
                    }
                    Tick::Wait => {}
                    other => break other,
                }
                std::thread::sleep(Duration::from_millis(16));
            };
            println!("{tick:?} after {:?}\n{}", started.elapsed(), log.join("\n"));
            println!("{}", tmux(&["capture-pane", "-p", "-t", target]));
            tick
        };
        let agent = std::env::var("GIVERNY_TMUX_AGENT").unwrap();
        let aliases = std::env::var("GIVERNY_TMUX_ALIAS").into_iter().collect();
        let mut open = Walk::open(agent, aliases, Instant::now());
        // Its agent id (`GIVERNY_TMUX_ID`), which a tagging relay's rows carry.
        if let Ok(id) = std::env::var("GIVERNY_TMUX_ID") {
            open = open.by_id(id);
        }
        assert_eq!(run(&mut open), Tick::Done);
        if let Some(ask) = &ask {
            let _ = std::fs::remove_file(ask);
        }
        // Give the relay a tick to hide the rows again, as it will in use.
        std::thread::sleep(Duration::from_secs(6));
        let mut home = Walk::home(Instant::now());
        assert_eq!(run(&mut home), Tick::Done);
    }
}

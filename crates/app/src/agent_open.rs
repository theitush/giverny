//! What a click on an agents-pane row opens (build task D).
//!
//! The pane hands the app a [`RowClick`]; this module decides, without
//! touching the app, what that click means — so the decision is tested here
//! and `main.rs::apply` only carries it out:
//!
//! * **Running** and **Done** workers open an overlay over the current
//!   terminal (giverny#41, #44) showing the worker's transcript, rendered as
//!   `giverny transcript` renders it: a Running one live, following it as it
//!   grows; a Done one opened at its end, on the final report. Nothing is
//!   typed into Claude Code and no tab is opened.
//! * From that overlay a Running worker can be *attached* (the overlay's
//!   **Open in Claude Code**, giverny#23): Giverny types into the parent's
//!   own terminal the keys that put Claude Code in that subagent's
//!   interactive view. The keys, and what they were established from, are
//!   [`cc_keys`]; the typing is driven by [`Attach`], which reads the
//!   parent's screen after every key rather than typing blind.
//!   With the agents pane on, the relay is asked to show Claude Code's
//!   agent strip while the overlay is up and the walk runs
//!   (`hooks::show_strip`), since the strip is the only keyboard path to a
//!   worker's view; `/tasks` is never used.
//! * While a tab shows a worker's view, Esc and the terminal's "back to
//!   orchestrator" button walk it back to the main view ([`Walk::home`],
//!   giverny#75), and the strip's own `main` row is not drawn
//!   ([`row_marks`]).
//! * A Done worker's overlay only reads: nothing is offered. Claude Code
//!   keeps no view of a finished subagent (it leaves the agent strip and
//!   `/tasks`, and its transcript is not a resumable session), and the one
//!   way back in — the parent's `SendMessage` — speaks for Ita, so the old
//!   **Revive** that typed a line at the parent's prompt is gone (giverny#61).
//! * **Planned** shows the row's brief, or its note, in the same overlay.
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

/// The row's title for a tab or overlay: `coo#158 · name`, whichever exist.
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
            let note = click
                .note
                .as_deref()
                .filter(|n| !n.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| "No brief for this row yet.".to_string());
            Plan::Show {
                title,
                body: Body::Text(note),
            }
        }
    }
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
/// showing the overlay (giverny#41).
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
/// a real `claude` in tmux with background subagents (giverny#44; #23 has
/// the earlier `/tasks` findings). Nothing is typed: only arrow keys and
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
/// main session (giverny#17) — entered by an attach, by `/tasks`, or by
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

/// One item of the Background dialog's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub selected: bool,
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

fn is_rule(line: &str) -> bool {
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
/// strip (giverny#3) until asked to show it (giverny#71).
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
/// (selected); `● main` → `main`. `↑ N more` rows are not items.
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
    /// What the overlay tells Ita.
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
                let hits: Vec<usize> = items
                    .iter()
                    .enumerate()
                    .filter(|(_, it)| self.names(&it.label))
                    .map(|(i, _)| i)
                    .collect();
                // The strip's first row is `main`; the rest are agents.
                let only =
                    (self.only_agent && items.len() == 2 && items[0].label == "main").then_some(1);
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
/// the footer's `N shells` pill it hides it (2.1.281, giverny#75), and
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

/// Moves a tab's Claude Code between the main view and a worker's view
/// through the agent strip (giverny#23, #75): wait for the strip, step to
/// the row with [`Attach`], Enter, check the view that opened, then put the
/// focus back in that view's prompt — so what Ita types next goes to it,
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
    calm: Option<(Instant, Seen)>,
    ups: u8,
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
            },
            now,
        )
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

    /// Whether the walk still needs Claude Code's strip drawn: until the
    /// row is entered. Going home needs no help: a worker's view keeps
    /// `main` on the strip whatever the relay hides.
    pub fn wants_strip(&self) -> bool {
        matches!(self.goal, Goal::Worker { .. })
            && matches!(self.phase, Phase::Start | Phase::Strip | Phase::Attach(_))
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
                    } => Attach::new(description.clone(), now)
                        .with_aliases(aliases.clone())
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
                // The screen held still for SETTLE: a key's effect has
                // landed, cursor and all.
                let calm = match &self.calm {
                    Some((since, before)) if *before == seen => now >= *since + SETTLE,
                    _ => {
                        self.calm = Some((now, seen.clone()));
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

// ------------------------------------------------------------ marks ----

/// What Giverny paints over a Claude Code tab's grid while the agents pane
/// stands in for Claude Code's own agent strip (giverny#75):
///
/// * the strip's `main` row is not drawn — the pane shows which worker the
///   tab is on, and the way back is Giverny's button;
/// * while a worker's view is on screen, a "back to orchestrator" button
///   sits at the right end of the status line under the prompt (the row
///   with the session's token counts), on its blank cells;
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
    let Some(prompt) = prompt_box(screen, screen) else {
        return marks;
    };
    // The status line: under the prompt's bottom rule, the row with the
    // token counts (Giverny's `… · session … · total: …`); else the first
    // row there with anything on it.
    let below = prompt.rows.end + 1;
    let near = below..rows.len().min(below + 3);
    let status = near
        .clone()
        .find(|&i| rows[i].contains("total:") || rows[i].contains("session "))
        .or_else(|| near.clone().find(|&i| !rows[i].trim().is_empty()));
    marks.button = status.and_then(|i| Some((as_row(i)?, as_row(rows[i].chars().count())?)));
    marks.escape = matches!(
        read_view(screen, screen),
        View::Prompt { .. } | View::Strip(_)
    );
    marks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(stage: Stage) -> RowClick {
        RowClick {
            stage,
            key: "coo#158".into(),
            agent_id: Some("a93".into()),
            name: "Wren".into(),
            transcript: None,
            open: None,
            brief: None,
            note: None,
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
                title: "coo#158 · Wren".into(),
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
                title: "coo#158 · Wren".into(),
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
        assert_eq!(
            plan(&c),
            Plan::Show {
                title: "coo#158 · Wren".into(),
                body: Body::Text("lane 2".into())
            }
        );
        c.brief = Some("/b/brief.md".into());
        assert_eq!(
            plan(&c),
            Plan::Show {
                title: "coo#158 · Wren".into(),
                body: Body::File("/b/brief.md".into())
            }
        );
        c.brief = None;
        c.note = None;
        assert!(
            matches!(plan(&c), Plan::Show { body: Body::Text(t), .. } if t == "No brief for this row yet.")
        );
    }

    #[test]
    fn titles() {
        let mut c = click(Stage::Planned);
        c.name = "coo#158".into();
        assert_eq!(title_of(&c), "coo#158");
        c.key.clear();
        assert_eq!(title_of(&c), "coo#158");
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
    // from a real `claude` in a pty during the giverny#23 spike.

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

    /// The footer strip under a worker's view, as #23 saw it (`─── <desc> ─`,
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
                selected: true
            })
        );
        assert_eq!(parse_item("     Local agents (3)"), None);
        assert_eq!(parse_item("   3 active agents"), None);
    }

    #[test]
    fn labels_match_through_spacing_and_truncation() {
        assert!(label_matches("theta  worker", "theta worker"));
        assert!(label_matches(
            "giverny#23 click a Run…",
            "giverny#23 click a Running row"
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
    // in tmux during the giverny#44 check: two background agents, two
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
             Haiku 4.5  ·  5h 11%  ·  wk 94%  ·  session 36.2k  ·  total: 90k\n{}",
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
             Haiku 4.5  ·  5h 11%  ·  wk 94%  ·  session 36.2k  ·  total: 90k\n{}",
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
                selected: true
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
  Haiku 4.5  ·  5h 12%  ·  wk 94%  ·  session 37.6k  ·  total: 117.9k
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
    // giverny#71 and #75 (main view, a worker's view, the strip focused or
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
                } else {
                    let label = self.agents[v - 1].1;
                    strip.push_str(&format!(
                        "{ptr}{dot} general-purpose  {label}          8s · ↓ 2k tokens\n"
                    ));
                }
            }
            let head = "● Launched.\n✻ Waiting for 2 background agents to finish\n";
            let foot = format!(
                "{rule}\n  Haiku 4.5  ·  5h 84%  ·  session 36.2k  ·  total: 90k\n  {hint}\n{strip}"
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
        assert!(!w.wants_strip());
        assert_clean(&fake);
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
    fn marks_hide_main_and_put_the_way_back_on_the_status_line() {
        let marks = row_marks(WORKER_VIEW_STRIP);
        let rows: Vec<&str> = WORKER_VIEW_STRIP.lines().collect();
        assert_eq!(marks.hidden, vec![7], "the `❯ ◯ main` row");
        assert!(rows[7].contains("◯ main"));
        let (row, used) = marks.button.expect("a worker's view has the button");
        assert!(rows[row as usize].contains("total: 117.9k"));
        assert_eq!(used as usize, rows[row as usize].chars().count());
        assert!(marks.escape);
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

    /// The attach keys against a real Claude Code in tmux (giverny#44):
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

    /// Into a worker's view and back against a real Claude Code in tmux
    /// (giverny#75): `GIVERNY_TMUX=<socket>:<target>`, a `claude` there
    /// with the running agent `GIVERNY_TMUX_AGENT` (its description; the
    /// strip's label for it, if different, in `GIVERNY_TMUX_ALIAS`). With
    /// the agents pane on, `GIVERNY_TMUX_ASK` names the relay's ask file
    /// (`<state>/show-strip/<tab id>`): it is written for the way in and
    /// removed once the view is open, as the app does. Run by hand:
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

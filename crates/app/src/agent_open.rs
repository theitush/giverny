//! What a click on an agents-pane row opens (build task D).
//!
//! The pane hands the app a [`RowClick`]; this module decides, without
//! touching the app, what that click means — so the decision is tested here
//! and `main.rs::apply` only carries it out:
//!
//! * **Running** workers of this tab's Claude Code are *attached*
//!   (giverny#23): Giverny types into the parent's own terminal the keys that
//!   put Claude Code in that subagent's interactive view — its transcript
//!   live, and a prompt that messages it. A subagent lives inside its parent
//!   process, so no second `claude` can reach it, but the parent's pty is
//!   Giverny's. The keys, and what they were established from, are
//!   [`cc_keys`]; the typing is driven by [`Attach`], which reads the parent's
//!   screen after every key rather than typing blind.
//! * **Done**, and a Running row that cannot be attached (no worker id), open
//!   the old way: a feed row's `open` command runs in a new tab — unless it
//!   resumes a conversation something is already running, which two claudes
//!   on one transcript would interleave. With no `open`, the worker's
//!   transcript is followed read-only in a new tab
//!   (`giverny transcript --follow`): a finished worker cannot be resumed.
//! * **Planned** shows the row's brief, or its note, in an overlay.
//!
//! A row with none of these still answers the click, with an overlay saying
//! what is missing, rather than doing nothing.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use giverny_claude::feed::Stage;

use crate::agents_pane::RowClick;

/// The largest brief shown whole; past it the overlay shows the head and
/// says so.
pub const BRIEF_MAX: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Put the parent tab's Claude Code in this running subagent's view.
    Attach { title: String, agent_id: String },
    /// Run the feed's `open` command in a new tab titled `title`.
    Run { title: String, command: String },
    /// Follow a worker's transcript in a new tab.
    Follow { title: String, transcript: PathBuf },
    /// Show text over the terminal.
    Show { title: String, body: Body },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// A brief on disk, read when the overlay opens.
    File(PathBuf),
    Text(String),
}

/// The row's title for a tab or overlay: `coo#158 · name`, whichever exist.
fn title_of(click: &RowClick) -> String {
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

/// Decide what a click does.
pub fn plan(click: &RowClick) -> Plan {
    if click.stage == Stage::Running
        && let Some(id) = click.agent_id.as_deref().filter(|id| !id.is_empty())
    {
        return Plan::Attach {
            title: title_of(click),
            agent_id: id.to_string(),
        };
    }
    plan_open(click)
}

/// What a click does when the worker is not attached: what a Done row always
/// does, and a Running one falls back to when the attach cannot start (the
/// parent tab runs no Claude Code, or the worker's description is unknown).
pub fn plan_open(click: &RowClick) -> Plan {
    let title = title_of(click);
    let open = click
        .open
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    match click.stage {
        Stage::Running | Stage::Done => {
            if let Some(cmd) = open {
                return Plan::Run {
                    title,
                    command: cmd.to_string(),
                };
            }
            if let Some(t) = &click.transcript {
                return Plan::Follow {
                    title,
                    transcript: t.clone(),
                };
            }
            let why = match &click.agent_id {
                Some(id) => format!(
                    "No transcript for worker {id} yet — Claude Code writes it once the \
                     worker's first turn lands. Click again in a moment."
                ),
                None => "This row names no worker and no `open` command, so there is \
                         nothing to open."
                    .to_string(),
            };
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

/// The shell line that follows `transcript` with this very binary.
pub fn follow_command(exe: &Path, transcript: &Path) -> String {
    use giverny_term::input::quote_path;
    format!(
        "{} transcript --follow {}",
        quote_path(&exe.to_string_lossy()),
        quote_path(&transcript.to_string_lossy())
    )
}

// ------------------------------------------------------------ attach ----

/// Claude Code's key path to one subagent's interactive view: the one place
/// that depends on Claude Code's keybindings and screen, so a Claude Code
/// update that moves them is fixed here.
///
/// Established on Claude Code 2.1.280 with `"tui": "fullscreen"` by driving
/// a real `claude` in a pty with live background subagents (giverny#23 has
/// the full findings):
///
/// 1. At the prompt, `/tasks` + Enter opens the *Background* dialog. It is an
///    immediate command, so it opens mid-turn too, and from inside a subagent
///    view. A draft in the prompt would be sent with it, so a draft is first
///    stashed with Ctrl+S (`chat:stash`), which Claude Code restores once the
///    command is submitted; on an *empty* prompt Ctrl+S pops an older stash
///    instead, so it is sent only when the screen shows a draft.
/// 2. The dialog lists `❯ <description> (running) · <model>`, the label
///    being the Agent call's `description`. Shells and monitors come first
///    and their number is invisible from here, so the list is read off the
///    screen and ↑/↓ step the `❯` onto the worker, one key at a time.
/// 3. `f` (foreground) on it opens the worker's interactive view. With one
///    agent alone the dialog may open on its detail card, where `f` works
///    too and ← goes back to the list.
///
/// Every key is its own write: sent as one burst, Claude Code took Ctrl+S,
/// `/tasks` and CR as typed text and sent it as a message. Never sent: Esc
/// (at the prompt it cancels the turn) and `x` (stops the selected agent).
pub mod cc_keys {
    use super::Keystroke;

    /// Opens the Background dialog from the prompt.
    pub const OPEN_LIST: &[Keystroke] = &[Keystroke::Text("/tasks"), Keystroke::Enter];
    /// `chat:stash`: sets a draft aside; restored after the next submit.
    pub const STASH_DRAFT: Keystroke = Keystroke::Ctrl('s');
    /// Moves the dialog's selection.
    pub const NEXT: Keystroke = Keystroke::Down;
    pub const PREVIOUS: Keystroke = Keystroke::Up;
    /// From an agent's detail card back to the list.
    pub const BACK: Keystroke = Keystroke::Left;
    /// Foreground: the selected agent's interactive view.
    pub const FOREGROUND: Keystroke = Keystroke::Text("f");

    /// The dialog's hint row in list mode, and the part only it has (the
    /// footer's agent strip also says "↑/↓ to select").
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
    screen.lines().any(|row| {
        let row = row.trim();
        let row = row
            .strip_prefix(cc_keys::PROMPT)
            .map_or(row, str::trim_start);
        cc_keys::MAIN_UNVIEWED.iter().any(|m| {
            row.strip_prefix(m).is_some_and(|rest| {
                rest.is_empty() || (rest.starts_with("  ") && rest.trim_start().starts_with('↑'))
            })
        })
    })
}

/// One key Giverny types into the parent's Claude Code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keystroke {
    Text(&'static str),
    Ctrl(char),
    Enter,
    Up,
    Down,
    Left,
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
    /// The Background dialog's list.
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
    let bright: Vec<&str> = undimmed.lines().collect();

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
    // The prompt box: a rule, `❯ …` in the first column, maybe more lines
    // of draft, a rule. The last one on screen; the transcript above has
    // `❯ ` lines too, but never right under a rule.
    for i in (1..rows.len()).rev() {
        if !rows[i].starts_with(PROMPT) || !is_rule(rows[i - 1]) {
            continue;
        }
        let Some(end) = (i + 1..rows.len().min(i + 40)).find(|&j| is_rule(rows[j])) else {
            continue;
        };
        let typed = |j: usize| {
            let line = bright.get(j).copied().unwrap_or("");
            let line = if j == i {
                line.trim_start().trim_start_matches(PROMPT)
            } else {
                line
            };
            !line.trim().is_empty()
        };
        return View::Prompt {
            draft: (i..end).any(typed),
        };
    }
    View::Other
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
        Keystroke::Text(t) => t.as_bytes().to_vec(),
        Keystroke::Ctrl(c) => Key::from_name(&c.to_ascii_uppercase().to_string())
            .and_then(|k| encode(k, Modifiers::CTRL))
            .unwrap_or_else(|| vec![c as u8 & 0x1f]),
        Keystroke::Enter => special(Key::Enter, Modifiers::NONE),
        Keystroke::Up => special(Key::ArrowUp, Modifiers::NONE),
        Keystroke::Down => special(Key::ArrowDown, Modifiers::NONE),
        Keystroke::Left => special(Key::ArrowLeft, Modifiers::NONE),
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
}

impl Stuck {
    /// What the overlay tells Ita.
    pub fn explain(&self, description: &str) -> String {
        match self {
            Stuck::NoPrompt => "Opening a worker means typing into this tab's Claude Code, and \
                                its prompt is not on screen (a permission question, or another \
                                dialog, is up). Answer or close it, then click the row again."
                .to_string(),
            Stuck::NotListed => format!(
                "Claude Code's background list (left open under this note) has no agent \
                 called \u{201c}{description}\u{201d}. It may have just finished; its Done row \
                 opens the transcript."
            ),
            Stuck::Ambiguous => format!(
                "More than one running agent is called \u{201c}{description}\u{201d}, so \
                 Giverny cannot tell which is which. Claude Code's background list is left \
                 open under this note: pick it with ↑/↓ and press f."
            ),
            Stuck::TimedOut => "Claude Code did not answer the keys in time, so Giverny \
                                stopped typing. Click the row again, or type /tasks in this \
                                tab and press f on the worker."
                .to_string(),
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

/// Types the key path into a parent's Claude Code, reading its screen after
/// every step. Pure but for the clock and screen it is handed, so it is
/// driven the same way in tests as in the app.
#[derive(Debug, Clone)]
pub struct Attach {
    pub description: String,
    /// `/tasks` has been typed.
    opened: bool,
    /// The foreground key is queued.
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
            opened: false,
            finishing: false,
            queue: VecDeque::new(),
            last_key: None,
            settle: None,
            deadline: now + DEADLINE,
        }
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
        if let Some((until, before)) = &self.settle {
            if view == *before && now < *until {
                return Tick::Wait;
            }
            self.settle = None;
        }
        match &view {
            View::List(items) => {
                let hits: Vec<usize> = items
                    .iter()
                    .enumerate()
                    .filter(|(_, it)| label_matches(&it.label, &self.description))
                    .map(|(i, _)| i)
                    .collect();
                let target = match hits.as_slice() {
                    [] => return Tick::Stuck(Stuck::NotListed),
                    [one] => *one,
                    _ => return Tick::Stuck(Stuck::Ambiguous),
                };
                let Some(at) = items.iter().position(|it| it.selected) else {
                    // A frame drawn between two selections: look again.
                    return Tick::Wait;
                };
                let key = match target.cmp(&at) {
                    std::cmp::Ordering::Equal => {
                        self.finishing = true;
                        cc_keys::FOREGROUND
                    }
                    std::cmp::Ordering::Greater => cc_keys::NEXT,
                    std::cmp::Ordering::Less => cc_keys::PREVIOUS,
                };
                self.push(&[key], view, now)
            }
            View::Detail(title) => {
                if label_matches(title, &self.description) {
                    self.finishing = true;
                    self.push(&[cc_keys::FOREGROUND], view, now)
                } else {
                    self.push(&[cc_keys::BACK], view, now)
                }
            }
            // `/tasks` is typed and the dialog is on its way (the prompt
            // clears a frame before it draws); DEADLINE bounds the wait.
            View::Prompt { .. } | View::Other if self.opened => Tick::Wait,
            View::Prompt { draft } => {
                self.opened = true;
                let mut keys = Vec::new();
                if *draft {
                    keys.push(cc_keys::STASH_DRAFT);
                }
                keys.extend_from_slice(cc_keys::OPEN_LIST);
                self.push(&keys, view, now)
            }
            View::Other => Tick::Stuck(Stuck::NoPrompt),
        }
    }
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
        }
    }

    #[test]
    fn running_attaches_and_done_opens() {
        let mut c = click(Stage::Running);
        c.transcript = Some("/t/agent-a93.jsonl".into());
        c.open = Some("claude --resume x".into());
        assert_eq!(
            plan(&c),
            Plan::Attach {
                title: "coo#158 · Wren".into(),
                agent_id: "a93".into()
            }
        );
        // The fallback, and Done, open the old way: `open`, else the
        // transcript.
        for stage in [Stage::Running, Stage::Done] {
            c.stage = stage;
            c.open = None;
            assert_eq!(
                plan_open(&c),
                Plan::Follow {
                    title: "coo#158 · Wren".into(),
                    transcript: "/t/agent-a93.jsonl".into()
                }
            );
            c.open = Some("claude --resume x".into());
            assert!(
                matches!(plan_open(&c), Plan::Run { command, .. } if command == "claude --resume x")
            );
        }
        c.stage = Stage::Done;
        assert!(matches!(plan(&c), Plan::Run { .. }));
    }

    #[test]
    fn running_without_a_worker_id_opens_the_old_way() {
        let mut c = click(Stage::Running);
        c.agent_id = None;
        c.open = Some("claude --resume x".into());
        assert!(matches!(plan(&c), Plan::Run { .. }));
        c.agent_id = Some(String::new());
        assert!(matches!(plan(&c), Plan::Run { .. }));
    }

    #[test]
    fn a_blank_open_falls_through_to_the_transcript() {
        let mut c = click(Stage::Done);
        c.open = Some("   ".into());
        c.transcript = Some("/t/a.jsonl".into());
        assert!(matches!(plan(&c), Plan::Follow { .. }));
    }

    #[test]
    fn running_without_a_transcript_says_so() {
        let c = click(Stage::Running);
        let Plan::Show {
            body: Body::Text(t),
            ..
        } = plan_open(&c)
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
        assert!(read_brief(&dir.join("none.md")).starts_with("Could not read the brief"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(not(windows))]
    fn follow_command_quotes() {
        assert_eq!(
            follow_command(Path::new("/opt/giverny"), Path::new("/a b/agent-x.jsonl")),
            "/opt/giverny transcript --follow '/a b/agent-x.jsonl'"
        );
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

    #[test]
    fn attach_opens_the_list_steps_to_the_worker_and_foregrounds_it() {
        let mut t = Instant::now();
        let mut a = Attach::new("eta worker", t);
        let empty = prompt_screen("");
        assert_eq!(
            drive(&mut a, &mut t, &empty),
            Tick::Send(Keystroke::Text("/tasks"))
        );
        assert_eq!(drive(&mut a, &mut t, &empty), Tick::Send(Keystroke::Enter));
        // The list opens on its first item; the worker is the last.
        for at in 0..4 {
            assert_eq!(
                drive(&mut a, &mut t, &list_screen(at)),
                Tick::Send(Keystroke::Down),
                "from item {at}"
            );
        }
        assert_eq!(
            drive(&mut a, &mut t, &list_screen(4)),
            Tick::Send(Keystroke::Text("f"))
        );
        assert_eq!(drive(&mut a, &mut t, &list_screen(4)), Tick::Done);
    }

    #[test]
    fn attach_waits_for_a_key_to_land_before_the_next() {
        let mut t = Instant::now();
        let mut a = Attach::new("eta worker", t);
        a.opened = true;
        assert_eq!(a.tick(t, &list_screen(2), ""), Tick::Send(Keystroke::Down));
        // Same screen: the Down has not landed yet, so no second Down.
        t += Duration::from_millis(100);
        assert_eq!(a.tick(t, &list_screen(2), ""), Tick::Wait);
        // Keys go out at least KEY_GAP apart.
        let mut b = Attach::new("x", t);
        let empty = prompt_screen("");
        assert_eq!(
            b.tick(t, &empty, &empty),
            Tick::Send(Keystroke::Text("/tasks"))
        );
        assert_eq!(
            b.tick(t + Duration::from_millis(5), &empty, &empty),
            Tick::Wait
        );
        assert_eq!(
            b.tick(t + KEY_GAP, &empty, &empty),
            Tick::Send(Keystroke::Enter)
        );
    }

    #[test]
    fn attach_stashes_a_draft_first() {
        let mut t = Instant::now();
        let mut a = Attach::new("eta worker", t);
        let draft = prompt_screen("half a thought");
        assert_eq!(
            drive(&mut a, &mut t, &draft),
            Tick::Send(Keystroke::Ctrl('s'))
        );
        assert_eq!(
            drive(&mut a, &mut t, &draft),
            Tick::Send(Keystroke::Text("/tasks"))
        );
        assert_eq!(drive(&mut a, &mut t, &draft), Tick::Send(Keystroke::Enter));
    }

    #[test]
    fn attach_moves_up_and_backs_out_of_the_wrong_card() {
        let mut t = Instant::now();
        let mut a = Attach::new("iota worker", t);
        assert_eq!(
            drive(&mut a, &mut t, &list_screen(4)),
            Tick::Send(Keystroke::Up)
        );
        let mut b = Attach::new("eta worker", t);
        assert_eq!(drive(&mut b, &mut t, DETAIL), Tick::Send(Keystroke::Left));
        let mut c = Attach::new("theta worker", t);
        assert_eq!(
            drive(&mut c, &mut t, DETAIL),
            Tick::Send(Keystroke::Text("f"))
        );
        assert_eq!(drive(&mut c, &mut t, DETAIL), Tick::Done);
    }

    #[test]
    fn attach_stops_rather_than_type_blind() {
        let mut t = Instant::now();
        let mut a = Attach::new("eta worker", t);
        assert_eq!(
            drive(&mut a, &mut t, PERMISSION),
            Tick::Stuck(Stuck::NoPrompt)
        );
        let mut b = Attach::new("kappa worker", t);
        assert_eq!(
            drive(&mut b, &mut t, &list_screen(0)),
            Tick::Stuck(Stuck::NotListed)
        );
        let twice = list_screen(0).replace("iota worker", "eta worker");
        let mut c = Attach::new("eta worker", t);
        assert_eq!(drive(&mut c, &mut t, &twice), Tick::Stuck(Stuck::Ambiguous));
        // Typed `/tasks`, and the dialog never came.
        let mut d = Attach::new("eta worker", t);
        let empty = prompt_screen("");
        assert_eq!(
            drive(&mut d, &mut t, &empty),
            Tick::Send(Keystroke::Text("/tasks"))
        );
        assert_eq!(drive(&mut d, &mut t, &empty), Tick::Send(Keystroke::Enter));
        assert_eq!(drive(&mut d, &mut t, &empty), Tick::Stuck(Stuck::TimedOut));
    }

    #[test]
    fn keystrokes_encode_like_typed_keys() {
        let enc = |k: egui::Key, m: egui::Modifiers| -> Option<Vec<u8>> {
            Some(format!("{k:?}{}", if m.ctrl { "+ctrl" } else { "" }).into_bytes())
        };
        assert_eq!(keystroke_bytes(Keystroke::Text("/tasks"), enc), b"/tasks");
        assert_eq!(keystroke_bytes(Keystroke::Ctrl('s'), enc), b"S+ctrl");
        assert_eq!(keystroke_bytes(Keystroke::Down, enc), b"ArrowDown");
        // With no encoder answer, Ctrl+S is still the control byte.
        assert_eq!(keystroke_bytes(Keystroke::Ctrl('s'), |_, _| None), [0x13]);
    }
}

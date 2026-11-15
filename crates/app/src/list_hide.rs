//! Claude Code's own agent list, kept off a tab whose management panel is
//! on (giverny#526).
//!
//! Until Claude Code 2.1.322 the relay answered `subagentStatusLine` with an
//! empty row per worker, and Claude Code then drew no list at all. 2.1.323
//! removed that setting and nothing inside Claude Code hides the list now,
//! so Giverny does, in what it draws; the grid keeps every row, which is
//! what a walk to a worker's view reads.
//!
//! Claude Code 2.1.325 draws the list as the last rows of its screen, under
//! the prompt's footer: a blank row, `● main` (`◯ main` while a worker's
//! view is on screen, `❯` before it while the list has the keyboard), then
//! one row per worker:
//!
//! ```text
//!   ⏵⏵ auto mode on · 3 shells · ← 2 agents
//!
//!   ● main
//!   ◯ general-purpose  Alpha: sleep 45 then reply done      8s · ↓ 26.7k tokens
//!   ◯ general-purpose  Beta: sleep 45 then reply done      10s · ↓ 29.8k tokens
//! ```
//!
//! It comes with the first worker and goes with the last. The footer's
//! `← N agents` is not part of it: it is there with no worker at all, and
//! the relay never hid it.
//!
//! * **Fullscreen** (the alternate screen), the list pushes everything above
//!   it up by its height. Those rows are given to Claude Code past the
//!   bottom of the tab ([`tail_rows`], `giverny_term::tail`): it lays out the
//!   rest exactly as it would with no list, and the picture ends where the
//!   tab does. A worker's view keeps its blank row and `◯ main`, where the
//!   "back to manager" button is drawn ([`agent_open::row_marks`]), as it
//!   did while the relay hid the list; only its agent rows go.
//! * **Outside fullscreen** Claude Code draws on the main screen, whose
//!   scrollback a resize would rewrite (it reprints its transcript, and
//!   lines come back changed), so nothing is resized: the list's rows are
//!   drawn blank ([`marks`]) and Claude Code's own scroll stands.

use giverny_term::widget::RowMarks;

use crate::agent_open;

/// The list's rows on screen: the index of its `main` row, and whether that
/// row is a worker view's (`◯ main`). The last `main` row on screen.
fn main_row(rows: &[&str]) -> Option<(usize, bool)> {
    let at = rows.iter().rposition(|r| agent_open::strip_shown(r))?;
    Some((at, agent_open::viewing_worker(rows[at])))
}

/// How many rows at the bottom of a fullscreen Claude Code screen to keep
/// out of sight: the agent list, when it is the bottom of the screen. From
/// the blank row above `● main` to the end on the main view; only the rows
/// under `◯ main` on a worker's view. 0 with no list, or one that does not
/// reach the bottom (some other program's screen).
pub fn tail_rows(screen: &str) -> u16 {
    let rows: Vec<&str> = screen.lines().collect();
    let Some((at, viewing)) = main_row(&rows) else {
        return 0;
    };
    // The list runs unbroken to the last row.
    if rows[at + 1..].iter().any(|r| r.trim().is_empty()) {
        return 0;
    }
    let start = if viewing {
        at + 1
    } else if at > 0 && rows[at - 1].trim().is_empty() {
        at - 1
    } else {
        at
    };
    u16::try_from(rows.len() - start).unwrap_or(u16::MAX)
}

/// What is painted over the tab's own Claude Code while the panel is on:
/// [`agent_open::row_marks`], and the agent rows under `● main` drawn
/// blank, which is all that hides the list outside fullscreen. In
/// fullscreen those rows are past the picture already.
pub fn marks(screen: &str) -> RowMarks {
    let mut marks = agent_open::row_marks(screen);
    let rows: Vec<&str> = screen.lines().collect();
    if let Some((at, false)) = main_row(&rows) {
        let agents = rows[at + 1..]
            .iter()
            .take_while(|r| !r.trim().is_empty())
            .count();
        marks
            .hidden
            .extend((at + 1..at + 1 + agents).filter_map(|i| u16::try_from(i).ok()));
    }
    marks
}

/// Whether a tab's foreground command (`Tab::foreground`, the shell's
/// newest child) is Claude Code itself: the tab's own session, not a
/// `claude` inside a multiplexer or a nested shell, whose list is left as
/// Claude Code draws it.
pub fn own_claude(foreground: Option<&str>) -> bool {
    let Some(command) = foreground else {
        return false;
    };
    let program = giverny_core::procs::program_name(command);
    if giverny_claude::lineage::is_claude_name(program) {
        return true;
    }
    // Claude Code run as a script: `node …/@anthropic-ai/claude-code/cli.js`.
    program == "node"
        && command
            .split_whitespace()
            .nth(1)
            .is_some_and(|s| s.contains("claude-code") && s.ends_with(".js"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: &str = "────────────────────────────────────────────────────────────";

    /// The bottom of Claude Code 2.1.325's fullscreen screen, as captured
    /// with three workers running (tmux, 120 columns).
    fn screen(list: &[&str]) -> String {
        let mut s = String::from(
            "  asked. Their handbacks were routine.\n\n✻ Waiting for 3 background agents to finish\n\n",
        );
        s.push_str(&format!("{RULE}\n❯ \n{RULE}\n"));
        s.push_str("  Haiku 5.5  ·  session: 39.1k  ·  subagents: 82k  ·  total: 121k\n");
        s.push_str("  ⏵⏵ auto mode on · 3 shells · ← 2 agents\n");
        for row in list {
            s.push_str(row);
            s.push('\n');
        }
        s
    }

    const ALPHA: &str =
        "  ◯ general-purpose  Alpha: sleep 45 then reply done            8s · ↓ 26.7k tokens";
    const BETA: &str =
        "  ◯ general-purpose  Beta: sleep 45 then reply done            10s · ↓ 29.8k tokens";
    const GAMMA: &str =
        "  ◯ general-purpose  Gamma: sleep 45 then reply done            9s · ↓ 26.7k tokens";

    #[test]
    fn the_whole_list_goes_on_the_main_view() {
        let s = screen(&["", "  ● main", ALPHA, BETA, GAMMA]);
        assert_eq!(tail_rows(&s), 5);
        let s = screen(&["", "  ● main", BETA]);
        assert_eq!(tail_rows(&s), 3);
        // With the keyboard, the pointer before `main`.
        let s = screen(&["", "❯ ● main", ALPHA, BETA]);
        assert_eq!(tail_rows(&s), 4);
    }

    #[test]
    fn no_list_no_rows() {
        assert_eq!(tail_rows(&screen(&[])), 0);
        assert_eq!(tail_rows(""), 0);
    }

    #[test]
    fn a_worker_view_keeps_its_main_row_for_the_back_button() {
        let viewed =
            "  ● general-purpose  Delta: sleep 90 then reply done      16s · ↓ 30.4k tokens";
        let s = screen(&["", "  ◯ main", viewed, BETA]);
        assert_eq!(tail_rows(&s), 2);
        // And the button is still where `◯ main` is, on screen.
        let marks = marks(&s);
        let main = s.lines().position(|l| l == "  ◯ main").unwrap() as u16;
        assert_eq!(marks.button.map(|(row, _)| row), Some(main));
    }

    #[test]
    fn a_list_that_does_not_reach_the_bottom_is_not_taken_off_the_grid() {
        // A grid just grown, before Claude Code redrew it: blank rows under
        // the list. Or another program's screen with a `● main` in it.
        let s = screen(&["", "  ● main", ALPHA, BETA, "", ""]);
        assert_eq!(tail_rows(&s), 0);
    }

    #[test]
    fn outside_fullscreen_the_list_rows_are_drawn_blank() {
        // On the main screen the list sits under the footer, with the rows
        // below it empty while the screen is not full.
        let s = screen(&["  ● main", ALPHA, BETA, "", ""]);
        let main = s.lines().position(|l| l == "  ● main").unwrap() as u16;
        let mut hidden = marks(&s).hidden;
        hidden.sort_unstable();
        assert_eq!(hidden, vec![main, main + 1, main + 2]);
        // Nothing else on the screen is.
        let none = marks(&screen(&[]));
        assert!(none.hidden.is_empty());
    }

    #[test]
    fn only_claude_itself_in_front_is_the_tabs_own_session() {
        assert!(own_claude(Some("claude --model haiku")));
        assert!(own_claude(Some(
            "/home/ita/.nvm/versions/node/v22.22.1/bin/claude --resume 0db89496"
        )));
        assert!(own_claude(Some("claude.exe")));
        assert!(own_claude(Some(
            "node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"
        )));
        assert!(!own_claude(None));
        assert!(!own_claude(Some("tmux new -s work")));
        assert!(!own_claude(Some("bash")));
        assert!(!own_claude(Some("vim claude.md")));
    }
}

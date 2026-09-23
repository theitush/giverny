//! What a click on an agents-pane row opens (build task D).
//!
//! The pane hands the app a [`RowClick`]; this module decides, without
//! touching the app, what that click means — so the decision is tested here
//! and `main.rs::apply` only carries it out:
//!
//! * **Running and Done** open the worker, the same way for both (Ita, on
//!   coo#158: *"doen should just open it"*). A feed row's `open` command runs
//!   in a new tab — unless it resumes a conversation something is already
//!   running, which two claudes on one transcript would interleave. With no
//!   `open`, the worker's transcript is followed read-only in a new tab
//!   (`giverny transcript --follow`), since Claude Code cannot open a worker
//!   by id.
//! * **Planned** shows the row's brief, or its note, in an overlay.
//!
//! A row with none of these still answers the click, with an overlay saying
//! what is missing, rather than doing nothing.

use std::path::{Path, PathBuf};

use giverny_claude::feed::Stage;

use crate::agents_pane::RowClick;

/// The largest brief shown whole; past it the overlay shows the head and
/// says so.
pub const BRIEF_MAX: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
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
    fn done_opens_exactly_like_running() {
        for stage in [Stage::Running, Stage::Done] {
            let mut c = click(stage);
            c.transcript = Some("/t/agent-a93.jsonl".into());
            assert_eq!(
                plan(&c),
                Plan::Follow {
                    title: "coo#158 · Wren".into(),
                    transcript: "/t/agent-a93.jsonl".into()
                }
            );
            c.open = Some("claude --resume x".into());
            assert!(
                matches!(plan(&c), Plan::Run { command, .. } if command == "claude --resume x")
            );
        }
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
}

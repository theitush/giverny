//! A worker's transcript, rendered for a person: `giverny transcript`.
//!
//! Claude Code cannot open a subagent by id, so a click on a Running or Done
//! row of the agents pane opens a tab that follows the worker's own
//! `agent-<id>.jsonl` instead — read-only, live, and compact enough to watch:
//! what it said, each tool it called (one line, the argument that says what
//! it is doing), and the first lines of what came back. Thinking, attachments
//! and bookkeeping lines are left out.
//!
//! [`render_line`] is the pure half — one JSONL line in, zero or more
//! terminal lines out — so it is tested without a file. [`follow`] is the
//! `tail -f` around it: it reads from the start, then polls for growth,
//! holds back a half-written last line until its newline arrives, and starts
//! over if the file is truncated or replaced.
//!
//! As everywhere in this crate, the lines are Claude Code's private format:
//! fields are pulled out one at a time, and a line that does not parse is
//! skipped rather than stopping the view.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::subagents::describe_tool_use;

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const RED: &str = "\x1b[31m";
const MAGENTA: &str = "\x1b[35m";
const RESET: &str = "\x1b[0m";

/// Lines of a tool result shown before it is folded.
const RESULT_LINES: usize = 4;
/// Lines of a prompt (the brief, or a message sent to the worker) shown.
const PROMPT_LINES: usize = 12;
/// Widest single line we print from a result or prompt, in characters.
const LINE_MAX: usize = 200;

/// Options for one render.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    /// ANSI colour. Off for tests and for a pipe.
    pub color: bool,
}

impl Style {
    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("{code}{text}{RESET}")
        } else {
            text.to_string()
        }
    }
}

/// `HH:MM:SS` in local time from the line's `timestamp`, if it has one.
fn clock(v: &Value) -> Option<String> {
    let ts = v
        .get("timestamp")
        .and_then(Value::as_str)?
        .parse::<jiff::Timestamp>()
        .ok()?;
    Some(
        ts.to_zoned(jiff::tz::TimeZone::system())
            .strftime("%H:%M:%S")
            .to_string(),
    )
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max - 1).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

/// The first `keep` non-empty lines of `text`, each clipped, and a
/// `… +N lines` tail when some were dropped.
fn folded(text: &str, keep: usize, indent: &str, style: Style, code: &str) -> Vec<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let mut out: Vec<String> = lines
        .iter()
        .take(keep)
        .map(|l| format!("{indent}{}", style.paint(code, &clip(l, LINE_MAX))))
        .collect();
    if lines.len() > keep {
        out.push(format!(
            "{indent}{}",
            style.paint(DIM, &format!("… +{} lines", lines.len() - keep))
        ));
    }
    out
}

/// A tool result's `content`: a string, or a list of `{type:"text"}` blocks
/// (anything else — an image — is named rather than shown).
fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|p| match p.get("type").and_then(Value::as_str) {
                Some("text") => p
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                Some(other) => format!("[{other}]"),
                None => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Render one JSONL line. Unknown or uninteresting lines render as nothing.
pub fn render_line(line: &str, style: Style) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let at = clock(&v)
        .map(|c| style.paint(DIM, &c) + " ")
        .unwrap_or_default();
    let mut out = Vec::new();
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            let Some(parts) = v.pointer("/message/content").and_then(Value::as_array) else {
                return out;
            };
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                        let mut lines = text.lines().map(str::trim_end);
                        let Some(first) = lines.next() else { continue };
                        out.push(format!("{at}{}", style.paint(BOLD, "◆")));
                        out.push(format!("  {first}"));
                        out.extend(lines.map(|l| format!("  {l}")));
                    }
                    Some("tool_use") => {
                        out.push(format!(
                            "{at}{}",
                            style.paint(CYAN, &format!("● {}", describe_tool_use(part)))
                        ));
                    }
                    // Thinking is private and often redacted; the tool calls
                    // around it say what it decided.
                    _ => {}
                }
            }
        }
        Some("user") => match v.pointer("/message/content") {
            Some(Value::String(s)) => {
                if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
                    return out;
                }
                out.push(format!("{at}{}", style.paint(MAGENTA, "▸ prompt")));
                out.extend(folded(s, PROMPT_LINES, "  ", style, ""));
            }
            Some(Value::Array(parts)) => {
                for part in parts {
                    match part.get("type").and_then(Value::as_str) {
                        Some("tool_result") => {
                            let error = part.get("is_error").and_then(Value::as_bool) == Some(true);
                            let text = result_text(part.get("content").unwrap_or(&Value::Null));
                            let code = if error { RED } else { DIM };
                            let mut body = folded(&text, RESULT_LINES, "    ", style, code);
                            if body.is_empty() {
                                body.push(format!("    {}", style.paint(DIM, "(no output)")));
                            }
                            // The first line hangs off the ⎿, the rest indent under it.
                            let first = body.remove(0);
                            out.push(format!(
                                "  {}{}",
                                style.paint(code, "⎿ "),
                                first.trim_start()
                            ));
                            out.extend(body);
                        }
                        Some("text") => {
                            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                            out.push(format!("{at}{}", style.paint(MAGENTA, "▸ message")));
                            out.extend(folded(text, PROMPT_LINES, "  ", style, ""));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        },
        _ => {}
    }
    out
}

/// A header for the view: who the worker is, from `agent-<id>.meta.json`
/// beside the transcript when it is there.
pub fn header(path: &Path, style: Style) -> Vec<String> {
    let meta = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".jsonl"))
        .map(|stem| path.with_file_name(format!("{stem}.meta.json")))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null);
    let field = |k: &str| meta.get(k).and_then(Value::as_str).map(str::to_string);
    let mut out = Vec::new();
    let title = field("description").unwrap_or_else(|| "worker".into());
    out.push(style.paint(BOLD, &title));
    let facts: Vec<String> = [field("agentType"), field("model")]
        .into_iter()
        .flatten()
        .collect();
    if !facts.is_empty() {
        out.push(style.paint(DIM, &facts.join(" · ")));
    }
    out.push(style.paint(DIM, &path.display().to_string()));
    out.push(style.paint(DIM, "read-only · Ctrl+C stops"));
    out.push(String::new());
    out
}

/// Print `path` rendered, and with `follow` keep printing what is appended
/// until the writer (the terminal) goes away. Returns when the output is
/// closed, or — without `follow` — at the end of the file.
pub fn follow(path: &Path, follow: bool, out: &mut impl Write) -> std::io::Result<()> {
    let style = Style { color: true };
    for l in header(path, style) {
        writeln!(out, "{l}")?;
    }
    let mut waited = false;
    let mut offset: u64 = 0;
    let mut pending = String::new();
    loop {
        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(_) if follow => {
                if !waited {
                    writeln!(out, "{}", style.paint(DIM, "waiting for the transcript…"))?;
                    out.flush()?;
                    waited = true;
                }
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
            Err(e) => return Err(e),
        };
        let len = file.metadata()?.len();
        if len < offset {
            // Truncated or replaced: start over rather than read garbage.
            writeln!(out, "{}", style.paint(DIM, "── transcript restarted ──"))?;
            offset = 0;
            pending.clear();
        }
        if len > offset {
            file.seek(SeekFrom::Start(offset))?;
            let mut bytes = Vec::new();
            file.take(len - offset).read_to_end(&mut bytes)?;
            offset = len;
            pending.push_str(&String::from_utf8_lossy(&bytes));
            // Only whole lines: the last one may still be being written.
            if let Some(cut) = pending.rfind('\n') {
                let complete: String = pending.drain(..=cut).collect();
                for line in complete.lines() {
                    for l in render_line(line, style) {
                        writeln!(out, "{l}")?;
                    }
                }
            }
            out.flush()?;
        }
        if !follow {
            if !pending.trim().is_empty() {
                for l in render_line(&pending, style) {
                    writeln!(out, "{l}")?;
                }
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Style = Style { color: false };

    fn strip_clock(lines: Vec<String>) -> Vec<String> {
        // The clock is local time; drop the `HH:MM:SS ` prefix.
        lines
            .into_iter()
            .map(|l| {
                if l.len() > 9 && l.as_bytes()[2] == b':' && l.as_bytes()[5] == b':' {
                    l[9..].to_string()
                } else {
                    l
                }
            })
            .collect()
    }

    #[test]
    fn tool_use_is_one_line_with_its_argument() {
        let line = r#"{"type":"assistant","timestamp":"2026-09-23T10:00:00Z","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test","description":"Run tests"}}]}}"#;
        assert_eq!(
            strip_clock(render_line(line, PLAIN)),
            vec!["● Bash: Run tests"]
        );
    }

    #[test]
    fn text_is_shown_whole_and_thinking_is_not() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"Done.\nAll green."}]}}"#;
        assert_eq!(
            render_line(line, PLAIN),
            vec!["◆", "  Done.", "  All green."]
        );
    }

    #[test]
    fn results_fold_after_four_lines() {
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"a\nb\nc\nd\ne\nf"}]}}"#;
        assert_eq!(
            render_line(line, PLAIN),
            vec!["  ⎿ a", "    b", "    c", "    d", "    … +2 lines"]
        );
    }

    #[test]
    fn block_results_and_empty_results() {
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":[{"type":"text","text":"boom"}]},{"type":"tool_result","content":""}]}}"#;
        assert_eq!(
            render_line(line, PLAIN),
            vec!["  ⎿ boom", "  ⎿ (no output)"]
        );
    }

    #[test]
    fn prompts_show_and_meta_and_attachments_do_not() {
        let prompt = r#"{"type":"user","message":{"content":"Work task 5.\nRead the issue."}}"#;
        assert_eq!(
            render_line(prompt, PLAIN),
            vec!["▸ prompt", "  Work task 5.", "  Read the issue."]
        );
        let meta = r#"{"type":"user","isMeta":true,"message":{"content":"<system>"}}"#;
        assert!(render_line(meta, PLAIN).is_empty());
        let att = r#"{"type":"attachment","attachment":{"type":"deferred_tools_delta"}}"#;
        assert!(render_line(att, PLAIN).is_empty());
        assert!(render_line("not json", PLAIN).is_empty());
    }

    #[test]
    fn follow_without_follow_renders_the_file_and_a_half_line() {
        let dir = std::env::temp_dir().join(format!("giverny-transcript-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-x.jsonl");
        std::fs::write(
            dir.join("agent-x.meta.json"),
            r#"{"description":"Work giverny#5","agentType":"general-purpose","model":"opus"}"#,
        )
        .unwrap();
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#,
                "\n",
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"tail"}]}}"#
            ),
        )
        .unwrap();
        let mut buf = Vec::new();
        follow(&path, false, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("Work giverny#5"));
        assert!(text.contains("general-purpose · opus"));
        assert!(text.contains("  hi"));
        assert!(text.contains("  tail"));
        std::fs::remove_dir_all(&dir).ok();
    }
}

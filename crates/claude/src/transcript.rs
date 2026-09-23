//! A worker's transcript, rendered for a person.
//!
//! Claude Code cannot open a subagent by id, so Giverny reads the worker's
//! own `agent-<id>.jsonl` instead: what it said, each tool it called (one
//! line, the argument that says what it is doing), and the first lines of
//! what came back. Thinking, attachments and bookkeeping lines are left out.
//! Two views use it: the overlay a Running or Done row of the agents pane
//! opens (giverny#41, #44), which keeps the task text whole, and
//! `giverny transcript [--follow]`, which folds it to watch in a terminal.
//!
//! [`render_rows`] is the pure half — one JSONL line in, zero or more rows
//! out — so it is tested without a file; [`render_line`] paints those rows
//! for a terminal. [`Tail`] is the `tail -f` under both views: it reads a
//! whole line at a time, holds back a half-written last line until its
//! newline arrives, and notices when the file is truncated or replaced.
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

/// How much of the long parts a render keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    /// Lines of a prompt or message kept; `None` keeps it whole, unclipped.
    pub prompt_lines: Option<usize>,
    /// Lines of a tool result kept.
    pub result_lines: usize,
}

impl Fold {
    /// The follower tab's: compact enough to watch scroll by.
    pub const TERMINAL: Fold = Fold {
        prompt_lines: Some(PROMPT_LINES),
        result_lines: RESULT_LINES,
    };
    /// Giverny's overlay (giverny#41): the task text whole, tool output
    /// still folded — it is the noise, not the task.
    pub const OVERLAY: Fold = Fold {
        prompt_lines: None,
        result_lines: RESULT_LINES,
    };
}

/// What a rendered row is, which is how it is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// The worker's own words, and a prompt's text.
    Plain,
    /// `◆`: a reply starts.
    Reply,
    /// `● Tool: what`.
    Tool,
    /// `▸ prompt` / `▸ message`.
    Prompt,
    /// Tool output, fold marks.
    Dim,
    /// Tool output that is an error.
    Error,
}

/// One rendered row: `indent`, then the clock (for a row that starts an
/// entry), then `text` in `tone`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub indent: &'static str,
    pub clock: Option<String>,
    pub tone: Tone,
    pub text: String,
}

impl Row {
    fn new(indent: &'static str, tone: Tone, text: impl Into<String>) -> Row {
        Row {
            indent,
            clock: None,
            tone,
            text: text.into(),
        }
    }

    /// The row as a terminal line.
    pub fn to_ansi(&self, style: Style) -> String {
        let at = self
            .clock
            .as_deref()
            .map(|c| style.paint(DIM, c) + " ")
            .unwrap_or_default();
        let code = match self.tone {
            Tone::Plain => "",
            Tone::Reply => BOLD,
            Tone::Tool => CYAN,
            Tone::Prompt => MAGENTA,
            Tone::Dim => DIM,
            Tone::Error => RED,
        };
        let text = if code.is_empty() {
            self.text.clone()
        } else {
            style.paint(code, &self.text)
        };
        format!("{}{at}{text}", self.indent)
    }

    /// The row as plain text.
    pub fn plain(&self) -> String {
        self.to_ansi(Style { color: false })
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

/// The first `keep` non-empty lines of `text` (all of them, unclipped, when
/// `keep` is `None`), and a `… +N lines` tail when some were dropped.
fn folded(text: &str, keep: Option<usize>, indent: &'static str, tone: Tone) -> Vec<Row> {
    let lines: Vec<&str> = match keep {
        Some(_) => text
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.trim().is_empty())
            .collect(),
        // Whole: keep its blank lines too, only not the trailing ones.
        None => text.trim_end().lines().map(str::trim_end).collect(),
    };
    let keep_n = keep.unwrap_or(usize::MAX);
    let mut out: Vec<Row> = lines
        .iter()
        .take(keep_n)
        .map(|l| {
            let text = if keep.is_some() {
                clip(l, LINE_MAX)
            } else {
                l.to_string()
            };
            Row::new(indent, tone, text)
        })
        .collect();
    if lines.len() > keep_n {
        out.push(Row::new(
            indent,
            Tone::Dim,
            format!("… +{} lines", lines.len() - keep_n),
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

/// Render one JSONL line as terminal lines. Unknown or uninteresting lines
/// render as nothing.
pub fn render_line(line: &str, style: Style) -> Vec<String> {
    render_rows(line, Fold::TERMINAL)
        .iter()
        .map(|r| r.to_ansi(style))
        .collect()
}

/// Render one JSONL line as rows: the pure half of both views.
pub fn render_rows(line: &str, fold: Fold) -> Vec<Row> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let at = clock(&v);
    let head = |tone: Tone, text: String| Row {
        indent: "",
        clock: at.clone(),
        tone,
        text,
    };
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
                        out.push(head(Tone::Reply, "◆".into()));
                        out.push(Row::new("  ", Tone::Plain, first));
                        out.extend(lines.map(|l| Row::new("  ", Tone::Plain, l)));
                    }
                    Some("tool_use") => {
                        out.push(head(Tone::Tool, format!("● {}", describe_tool_use(part))));
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
                out.push(head(Tone::Prompt, "▸ prompt".into()));
                out.extend(folded(s, fold.prompt_lines, "  ", Tone::Plain));
            }
            Some(Value::Array(parts)) => {
                for part in parts {
                    match part.get("type").and_then(Value::as_str) {
                        Some("tool_result") => {
                            let error = part.get("is_error").and_then(Value::as_bool) == Some(true);
                            let text = result_text(part.get("content").unwrap_or(&Value::Null));
                            let tone = if error { Tone::Error } else { Tone::Dim };
                            let mut body = folded(&text, Some(fold.result_lines), "    ", tone);
                            if body.is_empty() {
                                body.push(Row::new("    ", Tone::Dim, "(no output)"));
                            }
                            // The first line hangs off the ⎿, the rest indent under it.
                            let first = body.remove(0);
                            out.push(Row::new("  ", tone, format!("⎿ {}", first.text)));
                            out.extend(body);
                        }
                        Some("text") => {
                            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                            out.push(head(Tone::Prompt, "▸ message".into()));
                            out.extend(folded(text, fold.prompt_lines, "  ", Tone::Plain));
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

/// What [`Tail::read`] found since the last read.
#[derive(Debug, Default)]
pub struct Update {
    /// The file shrank or was replaced: what was read before is void.
    pub restarted: bool,
    /// Whole JSONL lines appended since the last read.
    pub lines: Vec<String>,
}

/// Reads a growing JSONL file a whole line at a time: the `tail -f` under
/// both the follower tab and the overlay's live view.
#[derive(Debug, Default)]
pub struct Tail {
    offset: u64,
    pending: String,
}

impl Tail {
    /// Read what was appended since the last call. A half-written last line
    /// is held back until its newline arrives.
    pub fn read(&mut self, path: &Path) -> std::io::Result<Update> {
        let mut file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        let mut update = Update::default();
        if len < self.offset {
            update.restarted = true;
            self.offset = 0;
            self.pending.clear();
        }
        if len > self.offset {
            file.seek(SeekFrom::Start(self.offset))?;
            let mut bytes = Vec::new();
            (&mut file)
                .take(len - self.offset)
                .read_to_end(&mut bytes)?;
            self.offset += bytes.len() as u64;
            self.pending.push_str(&String::from_utf8_lossy(&bytes));
            if let Some(cut) = self.pending.rfind('\n') {
                let complete: String = self.pending.drain(..=cut).collect();
                update.lines = complete.lines().map(str::to_string).collect();
            }
        }
        Ok(update)
    }

    /// The held-back last line, if the file ends without a newline — for a
    /// read that will not wait for it.
    pub fn rest(&self) -> Option<&str> {
        Some(self.pending.as_str()).filter(|p| !p.trim().is_empty())
    }
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
    let mut tail = Tail::default();
    loop {
        let update = match tail.read(path) {
            Ok(u) => u,
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
        if update.restarted {
            // Truncated or replaced: start over rather than read garbage.
            writeln!(out, "{}", style.paint(DIM, "── transcript restarted ──"))?;
        }
        for line in &update.lines {
            for l in render_line(line, style) {
                writeln!(out, "{l}")?;
            }
        }
        out.flush()?;
        if !follow {
            if let Some(rest) = tail.rest() {
                for l in render_line(rest, style) {
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

    #[test]
    fn the_overlay_keeps_a_prompt_whole() {
        let body: String = (1..=20).map(|i| format!("line {i}\n\n")).collect();
        let long = "x".repeat(300);
        let line = serde_json::json!({
            "type": "user",
            "message": {"content": format!("{body}{long}")},
        })
        .to_string();
        let rows = render_rows(&line, Fold::OVERLAY);
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts[0], "▸ prompt");
        assert!(texts.contains(&"line 20"), "{texts:?}");
        assert!(texts.contains(&""), "blank lines are kept");
        assert_eq!(texts.last().unwrap().chars().count(), 300, "not clipped");
        assert!(!texts.iter().any(|t| t.starts_with("… +")));
        // The terminal view folds the same prompt.
        let folded = render_rows(&line, Fold::TERMINAL);
        assert!(folded.last().unwrap().text.starts_with("… +"));
    }

    #[test]
    fn rows_carry_tone_and_clock() {
        let line = r#"{"type":"user","timestamp":"2026-09-23T10:00:00Z","message":{"content":[{"type":"tool_result","is_error":true,"content":"boom"}]}}"#;
        let rows = render_rows(line, Fold::OVERLAY);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tone, Tone::Error);
        assert_eq!(rows[0].plain(), "  ⎿ boom");
        let line = r#"{"type":"assistant","timestamp":"2026-09-23T10:00:00Z","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/a/b.rs"}}]}}"#;
        let rows = render_rows(line, Fold::OVERLAY);
        assert_eq!(rows[0].tone, Tone::Tool);
        assert!(rows[0].clock.is_some());
    }

    #[test]
    fn tail_reads_whole_lines_and_notices_a_restart() {
        let dir = std::env::temp_dir().join(format!("giverny-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-t.jsonl");
        std::fs::write(&path, "a\nb").unwrap();
        let mut tail = Tail::default();
        let u = tail.read(&path).unwrap();
        assert_eq!(u.lines, vec!["a"]);
        assert_eq!(tail.rest(), Some("b"));
        std::fs::write(&path, "a\nbc\nd\n").unwrap();
        let u = tail.read(&path).unwrap();
        assert!(!u.restarted);
        assert_eq!(u.lines, vec!["bc", "d"]);
        assert!(tail.read(&path).unwrap().lines.is_empty());
        std::fs::write(&path, "z\n").unwrap();
        let u = tail.read(&path).unwrap();
        assert!(u.restarted);
        assert_eq!(u.lines, vec!["z"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}

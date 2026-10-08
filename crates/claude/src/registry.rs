//! Claude Code's live session registry: `$CONFIG_DIR/sessions/<pid>.json`.
//!
//! Written by Claude Code itself (verified against 2.1.220): gives per-session
//! status with zero hook setup. Stale files are never cleaned up upstream, so
//! PID liveness gating is mandatory.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct SessionEntry {
    pub pid: u32,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(default)]
    pub cwd: PathBuf,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(rename = "statusUpdatedAt", default)]
    pub status_updated_at: u64,
    /// Session start, ms since epoch. Compared against `settings.json`'s
    /// mtime to tell whether this session loaded Giverny's hooks.
    #[serde(rename = "startedAt", default)]
    pub started_at_ms: u64,
    /// The background job this interactive session handed its conversation
    /// to and now shows (`parkedJobId`, Claude Code 2.1.292): the tab is the
    /// job's, and so are its hooks (giverny#242).
    #[serde(rename = "parkedJobId", default)]
    pub parked_job_id: Option<String>,
    /// `bg` for a background job's worker, which the daemon runs and no
    /// tab does (`interactive` otherwise).
    #[serde(default)]
    pub kind: String,
    /// The background job a worker runs (`jobId`).
    #[serde(rename = "jobId", default)]
    pub job_id: Option<String>,
}

impl SessionEntry {
    /// A background job's worker: the daemon's process, never a tab's. A
    /// tab attached to the job holds the same conversation, and must not
    /// take the worker for its own claude by it (giverny#243).
    pub fn job_worker(&self) -> bool {
        self.kind == "bg" || self.job_id.is_some()
    }

    /// The agent is working: thinking, or running a tool call. Measured, not
    /// assumed — a session stays `busy` through minutes of back-to-back Bash
    /// calls.
    pub fn busy(&self) -> bool {
        self.status == "busy"
    }

    /// A background shell is alive while the agent itself is at its prompt —
    /// a `run_in_background` command, or one that outlived its timeout and
    /// was moved to the background.
    ///
    /// Claude Code's own session list counts this as "working", which is a
    /// fair answer to "is anything happening in there" and the wrong one for
    /// a spinner: the agent is waiting on *you*, often with a question, while
    /// something polls in the background. Marked, not animated.
    pub fn background_shell(&self) -> bool {
        self.status == "shell"
    }

    /// Claude is blocked on the user. Same bucket Claude Code puts it in, and
    /// the reason a session waiting on a permission prompt used to read as
    /// idle here when no hook reported it.
    pub fn waiting(&self) -> bool {
        self.status == "waiting"
    }
}

#[derive(Debug, Clone)]
pub struct LiveSession {
    pub entry: SessionEntry,
    pub config_dir: PathBuf,
}

/// Is the session behind this entry still running?
///
/// A pid only means something on the machine that issued it. An account
/// inside WSL, read from Windows, hands us Linux pids: checking those against
/// the Windows process table is not a weaker answer, it is an unrelated one.
/// The distribution is asked instead, by the sweep that already walks its
/// `/proc`, and this reads what that sweep left behind.
fn entry_is_live(config_dir: &Path, entry: &SessionEntry) -> bool {
    if crate::wsl::is_wsl_path(config_dir) {
        // The pids in there are the distribution's, and the last sweep asked
        // it which ones exist. Not knowing means no: an unanswered question
        // used to read as "still running", which is the answer that refuses
        // to bring a conversation back — and it was wrong for the two minutes
        // after a restart, which is precisely when the app restarts.
        return crate::wsl::pid_alive_in(config_dir, entry.pid).unwrap_or(false);
    }
    pid_alive(entry.pid)
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // macOS/BSD: signal 0 probes existence without delivering anything.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
            true,
            ProcessRefreshKind::nothing(),
        );
        sys.process(Pid::from_u32(pid)).is_some()
    }
}

/// Scan the registries of every config dir for live sessions.
pub fn scan(config_dirs: impl IntoIterator<Item = PathBuf>) -> Vec<LiveSession> {
    let mut out = Vec::new();
    for dir in config_dirs {
        let sessions = dir.join("sessions");
        let Ok(entries) = std::fs::read_dir(&sessions) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(entry) = serde_json::from_slice::<SessionEntry>(&bytes) else {
                continue;
            };
            if entry_is_live(&dir, &entry) {
                out.push(LiveSession {
                    entry,
                    config_dir: dir.clone(),
                });
            }
        }
    }
    out
}

/// Is this claude session currently live in ANY of the given config dirs?
/// (Resuming it twice would interleave two writers into one transcript.)
pub fn session_is_live(config_dirs: impl IntoIterator<Item = PathBuf>, session_id: &str) -> bool {
    scan(config_dirs)
        .iter()
        .any(|s| s.entry.session_id == session_id)
}

/// Locate a session's transcript inside one config dir:
/// `projects/<munged-cwd>/<session_id>.jsonl`. The munging is lossy, so we
/// scan project dirs instead of reconstructing it.
pub fn find_transcript(config_dir: &Path, session_id: &str) -> Option<PathBuf> {
    let projects = config_dir.join("projects");
    for entry in std::fs::read_dir(projects).ok()?.flatten() {
        let candidate = entry.path().join(format!("{session_id}.jsonl"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The working directory a transcript's conversation ran in — the *only*
/// directory `claude --resume` will find it from. Early lines carry a `cwd`
/// field (format is internal; we scan a bounded prefix and tolerate misses).
pub fn transcript_cwd(path: &Path) -> Option<PathBuf> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    for line in reader.lines().take(50) {
        let Ok(line) = line else { break };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(cwd) = value.get("cwd").and_then(|c| c.as_str())
            && !cwd.is_empty()
        {
            return Some(PathBuf::from(cwd));
        }
    }
    None
}

/// Claude's project-dir name for a cwd: every non-alphanumeric byte becomes
/// `-`, case preserved (verified against 2.1.220 layouts).
pub fn munge_cwd(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// A past conversation in some project dir, for the resume picker.
#[derive(Debug, Clone)]
pub struct PastSession {
    pub id: String,
    /// AI title / last prompt (best-effort from the transcript tail).
    pub title: String,
    pub path: PathBuf,
    pub config_dir: PathBuf,
    pub modified: Option<std::time::SystemTime>,
    /// Currently open in some terminal — resuming would corrupt it.
    pub live: bool,
}

/// List past sessions for `cwd` across config dirs, newest first (capped).
pub fn list_sessions(config_dirs: &[PathBuf], cwd: &Path) -> Vec<PastSession> {
    use std::collections::HashSet;
    let live_ids: HashSet<String> = scan(config_dirs.iter().cloned())
        .into_iter()
        .map(|s| s.entry.session_id)
        .collect();
    let munged = munge_cwd(cwd);
    let mut out: Vec<PastSession> = Vec::new();
    for dir in config_dirs {
        let proj = dir.join("projects").join(&munged);
        let Ok(entries) = std::fs::read_dir(&proj) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(id) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            if id.len() != 36 {
                continue;
            }
            let modified = e.metadata().ok().and_then(|m| m.modified().ok());
            out.push(PastSession {
                live: live_ids.contains(&id),
                id,
                title: String::new(),
                path,
                config_dir: dir.clone(),
                modified,
            });
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.modified));
    out.truncate(15);
    for s in &mut out {
        s.title = tail_title(&s.path).unwrap_or_else(|| s.id[..8].to_string());
    }
    out
}

/// Best-effort session title from the transcript's tail: the last `aiTitle`
/// line, else the last `lastPrompt` (truncated).
fn tail_title(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 128 * 1024;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = String::new();
    file.take(TAIL).read_to_string(&mut buf).ok()?;

    let mut title: Option<String> = None;
    let mut prompt: Option<String> = None;
    for line in buf.lines() {
        if !line.contains("\"aiTitle\"") && !line.contains("\"lastPrompt\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(t) = v.get("aiTitle").and_then(|t| t.as_str()) {
            title = Some(t.to_string());
        } else if let Some(p) = v.get("lastPrompt").and_then(|p| p.as_str()) {
            prompt = Some(p.to_string());
        }
    }
    let mut best = title.or(prompt)?;
    best = best.replace(['\n', '\r'], " ");
    if best.chars().count() > 60 {
        best = best.chars().take(59).collect::<String>() + "…";
    }
    (!best.is_empty()).then_some(best)
}

/// The last prompt the user sent in a transcript: the last of
/// [`prompt_history`].
pub fn last_prompt(path: &Path) -> Option<String> {
    prompt_history(path).pop()
}

/// The prompts the user sent in a transcript, oldest first, for a session
/// whose prompts no hook reported: one adopted mid-way, or resumed after a
/// restart. Read from the tail, so a very long session gives its later turns.
///
/// The prompts are the user messages that are not tool results, notes Claude
/// Code adds (`isMeta`, compaction summaries) or things it wraps in a tag (a
/// command, a notification, `!` shell input).
///
/// The last one is checked against Claude Code's own `lastPrompt` marker,
/// which is cut at 200 characters with an ellipsis and has its line breaks
/// flattened. The message the marker agrees with is taken in full; when none
/// does, the marker itself ends the list: it is the one that says which
/// message was typed (a `!` command's, say), and a cut prompt beats none.
pub fn prompt_history(path: &Path) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    // Long answers with tool output in them push the messages that started
    // their turns a long way back.
    const TAIL: u64 = 2 * 1024 * 1024;
    let Some(buf) = (|| {
        let mut file = std::fs::File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
        let mut bytes = Vec::new();
        file.take(TAIL).read_to_end(&mut bytes).ok()?;
        // The seek can land inside a character; only the first line is cut.
        Some(String::from_utf8_lossy(&bytes).into_owned())
    })() else {
        return Vec::new();
    };

    let flat = |s: &str| s.replace(['\n', '\r'], " ");
    let flag =
        |v: &serde_json::Value, name: &str| v.get(name).and_then(|m| m.as_bool()) == Some(true);
    let mut marker: Option<String> = None;
    let mut typed: Vec<String> = Vec::new();
    for line in buf.lines() {
        let has_marker = line.contains("\"lastPrompt\"");
        if !has_marker && !line.contains("\"user\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if has_marker {
            if let Some(p) = v.get("lastPrompt").and_then(|p| p.as_str()) {
                marker = Some(p.to_string());
            }
            continue;
        }
        if v.get("type").and_then(|t| t.as_str()) != Some("user")
            || flag(&v, "isMeta")
            || flag(&v, "isCompactSummary")
        {
            continue;
        }
        if let Some(text) = v.get("message").and_then(|m| user_text(m.get("content")?)) {
            let text = text.trim().to_string();
            if !text.is_empty() {
                typed.push(text);
            }
        }
    }
    // The last prompt: the message the marker names, in full.
    let last = marker.map(|m| {
        let stem = m.strip_suffix('…').filter(|_| m.chars().count() > 200);
        typed
            .iter()
            .rev()
            .find(|t| {
                let t = flat(t);
                t == m || stem.is_some_and(|stem| t.starts_with(stem))
            })
            .cloned()
            .unwrap_or_else(|| m.trim().to_string())
    });
    let mut prompts: Vec<String> = typed.into_iter().filter(|t| !t.starts_with('<')).collect();
    // A marker no listed message agrees with names one Claude Code wrapped
    // (`!` shell input): it is the latest prompt.
    if let Some(last) = last.filter(|l| !l.is_empty())
        && !prompts.contains(&last)
    {
        prompts.push(last);
    }
    prompts
}

/// The text of a user message, or `None` when it is a tool result.
fn user_text(content: &serde_json::Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let items = content.as_array()?;
    let mut parts = Vec::new();
    for item in items {
        match item.get("type").and_then(|t| t.as_str()) {
            Some("text") => parts.push(item.get("text")?.as_str()?.to_string()),
            Some("tool_result") => return None,
            _ => {}
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// Walk `/proc/<pid>/stat` parent links; true when `ancestor` is in the chain.
/// Maps a claude process to the Giverny tab whose shell spawned it.
#[cfg(target_os = "linux")]
pub fn has_ancestor(mut pid: u32, ancestor: u32) -> bool {
    for _ in 0..64 {
        if pid == ancestor {
            return true;
        }
        if pid <= 1 {
            return false;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        // Field 4 (ppid) comes after the parenthesized comm, which may itself
        // contain spaces/parens — split after the LAST ')'.
        let Some((_, rest)) = stat.rsplit_once(')') else {
            return false;
        };
        let mut fields = rest.split_whitespace();
        let _state = fields.next();
        let Some(ppid) = fields.next().and_then(|p| p.parse::<u32>().ok()) else {
            return false;
        };
        pid = ppid;
    }
    false
}

/// Same walk on non-Linux platforms, via `sysinfo`'s parent links.
#[cfg(not(target_os = "linux"))]
pub fn has_ancestor(pid: u32, ancestor: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let mut current = Pid::from_u32(pid);
    let target = Pid::from_u32(ancestor);
    for _ in 0..64 {
        if current == target {
            return true;
        }
        match sys.process(current).and_then(|p| p.parent()) {
            Some(parent) => current = parent,
            None => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_registry_entry() {
        let json = r#"{ "pid": 1234, "sessionId": "da89-uuid", "cwd": "/home/u/dev",
            "startedAt": 1785250651905, "procStart": "50262977", "version": "2.1.220",
            "kind": "interactive", "entrypoint": "cli", "name": "dev-13",
            "nameSource": "derived", "status": "busy", "updatedAt": 1, "statusUpdatedAt": 2 }"#;
        let e: SessionEntry = serde_json::from_str(json).unwrap();
        assert_eq!(e.pid, 1234);
        assert_eq!(e.started_at_ms, 1785250651905);
        assert!(e.busy());
        assert_eq!(e.name.as_deref(), Some("dev-13"));
        assert_eq!(e.cwd, PathBuf::from("/home/u/dev"));
        assert!(!e.job_worker());
        let worker: SessionEntry = serde_json::from_str(
            r#"{"pid":2209118,"sessionId":"6e7e56e0-1dca","kind":"bg","jobId":"6e7e56e0","status":"idle"}"#,
        )
        .unwrap();
        assert!(worker.job_worker());
        assert_eq!(worker.job_id.as_deref(), Some("6e7e56e0"));
    }

    #[test]
    fn scan_filters_dead_pids() {
        let dir = std::env::temp_dir().join(format!("giverny-reg-{}", std::process::id()));
        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        // Our own pid = alive; pid 4194304+1 range = almost surely dead.
        let me = std::process::id();
        std::fs::write(
            sessions.join(format!("{me}.json")),
            format!(r#"{{"pid":{me},"sessionId":"alive","status":"idle"}}"#),
        )
        .unwrap();
        std::fs::write(
            sessions.join("4194301.json"),
            r#"{"pid":4194301,"sessionId":"dead","status":"busy"}"#,
        )
        .unwrap();
        let live = scan([dir.clone()]);
        assert_eq!(live.len(), 1, "{live:?}");
        assert_eq!(live[0].entry.session_id, "alive");
        assert!(session_is_live([dir.clone()], "alive"));
        assert!(!session_is_live([dir.clone()], "dead"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn munge_matches_claude_layout() {
        assert_eq!(
            munge_cwd(Path::new("/home/yoz/Dev/claude_test")),
            "-home-yoz-Dev-claude-test",
            "underscores and slashes become dashes, case preserved"
        );
        assert_eq!(
            munge_cwd(Path::new("/home/yoz/Dev/yoav.xyz.next")),
            "-home-yoz-Dev-yoav-xyz-next"
        );
    }

    #[test]
    fn lists_sessions_with_tail_titles() {
        let dir = std::env::temp_dir().join(format!("giverny-list-{}", std::process::id()));
        let cwd = Path::new("/home/u/proj_x");
        let proj = dir.join("projects").join(munge_cwd(cwd));
        std::fs::create_dir_all(&proj).unwrap();
        let sid_a = "aaaaaaaa-1111-2222-3333-444444444444";
        let sid_b = "bbbbbbbb-1111-2222-3333-444444444444";
        std::fs::write(
            proj.join(format!("{sid_a}.jsonl")),
            "{\"type\":\"ai-title\",\"aiTitle\":\"fix auth bug\",\"sessionId\":\"a\"}\n",
        )
        .unwrap();
        std::fs::write(
            proj.join(format!("{sid_b}.jsonl")),
            "{\"type\":\"last-prompt\",\"lastPrompt\":\"run the tests\",\"sessionId\":\"b\"}\n",
        )
        .unwrap();
        std::fs::write(proj.join("not-a-session.jsonl"), "junk").unwrap();

        let sessions = list_sessions(std::slice::from_ref(&dir), cwd);
        assert_eq!(sessions.len(), 2, "{sessions:?}");
        let a = sessions.iter().find(|s| s.id == sid_a).unwrap();
        assert_eq!(a.title, "fix auth bug");
        let b = sessions.iter().find(|s| s.id == sid_b).unwrap();
        assert_eq!(b.title, "run the tests");
        assert!(!a.live);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_transcript_and_its_cwd() {
        let dir = std::env::temp_dir().join(format!(
            "giverny-registry-transcript-{}",
            std::process::id()
        ));
        let proj = dir.join("projects").join("-home-u-Dev-myproj");
        std::fs::create_dir_all(&proj).unwrap();
        let sid = "b263c7bf-2cc6-4ee1-b00a-948a4152f6ab";
        std::fs::write(
            proj.join(format!("{sid}.jsonl")),
            concat!(
                "{\"mode\":\"default\",\"sessionId\":\"x\",\"type\":\"mode\"}\n",
                "{\"type\":\"user\",\"cwd\":\"/home/u/Dev/myproj\",\"sessionId\":\"x\"}\n",
            ),
        )
        .unwrap();

        let found = find_transcript(&dir, sid).expect("transcript located");
        assert_eq!(
            transcript_cwd(&found),
            Some(PathBuf::from("/home/u/Dev/myproj")),
            "cwd read from early transcript lines"
        );
        assert!(find_transcript(&dir, "0000-not-there").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn transcript(name: &str, lines: &[serde_json::Value]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("giverny-prompt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.jsonl"));
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, body).unwrap();
        path
    }

    fn user(content: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"type": "user", "message": {"role": "user", "content": content}})
    }

    fn marker(prompt: &str) -> serde_json::Value {
        serde_json::json!({"type": "last-prompt", "lastPrompt": prompt, "sessionId": "s"})
    }

    #[test]
    fn last_prompt_is_the_full_message_the_marker_names() {
        let long = format!("first line\nsecond line {}", "x".repeat(300));
        // Claude Code's own marker: flattened and cut at 200 characters.
        let cut: String = long
            .replace('\n', " ")
            .chars()
            .take(200)
            .collect::<String>()
            + "…";
        let path = transcript(
            "full",
            &[
                user(serde_json::json!("an earlier prompt")),
                user(serde_json::json!(long)),
                user(serde_json::json!([{"type": "tool_result", "content": "output"}])),
                serde_json::json!({"type": "user", "isMeta": true,
                    "message": {"content": "<local-command-caveat>…"}}),
                user(serde_json::json!(
                    "<task-notification>done</task-notification>"
                )),
                serde_json::json!({"type": "assistant", "message": {"content": "sure"}}),
                marker(&cut),
            ],
        );
        assert_eq!(last_prompt(&path).as_deref(), Some(long.as_str()));
    }

    #[test]
    fn prompt_history_lists_the_typed_prompts_in_order() {
        let path = transcript(
            "history",
            &[
                user(serde_json::json!("first")),
                serde_json::json!({"type": "assistant", "message": {"content": "ok"}}),
                user(serde_json::json!([{"type": "tool_result", "content": "out"}])),
                user(serde_json::json!("<command-name>/model</command-name>")),
                serde_json::json!({"type": "user", "isCompactSummary": true,
                    "message": {"content": "This session is being continued"}}),
                user(serde_json::json!("second\nwith a second line")),
                marker("second with a second line"),
                user(serde_json::json!("third")),
                marker("third"),
            ],
        );
        assert_eq!(
            prompt_history(&path),
            vec!["first", "second\nwith a second line", "third"]
        );
        // `!` shell input: the wrapped message is not listed, the marker ends it.
        let path = transcript(
            "history-bash",
            &[
                user(serde_json::json!("first")),
                user(serde_json::json!("<bash-input>ls</bash-input>")),
                marker("!  ls"),
            ],
        );
        assert_eq!(prompt_history(&path), vec!["first", "!  ls"]);
        assert!(prompt_history(Path::new("/nonexistent/x.jsonl")).is_empty());
    }

    #[test]
    fn last_prompt_falls_back_to_the_marker() {
        // `!` bash mode: the message is wrapped, the marker is what was typed.
        let path = transcript(
            "bash",
            &[
                user(serde_json::json!("<bash-input>ls</bash-input>")),
                marker("!  ls"),
            ],
        );
        assert_eq!(last_prompt(&path).as_deref(), Some("!  ls"));

        // Text-and-image prompts arrive as an array.
        let path = transcript(
            "array",
            &[
                user(serde_json::json!([{"type": "text", "text": "look at this"},
                                         {"type": "image"}])),
                marker("look at this"),
            ],
        );
        assert_eq!(last_prompt(&path).as_deref(), Some("look at this"));

        // No marker: the last plain message.
        let path = transcript(
            "old",
            &[
                user(serde_json::json!("fix the build")),
                user(serde_json::json!("<command-name>/clear</command-name>")),
            ],
        );
        assert_eq!(last_prompt(&path).as_deref(), Some("fix the build"));

        let path = transcript("empty", &[serde_json::json!({"type": "mode"})]);
        assert_eq!(last_prompt(&path), None);
        assert_eq!(last_prompt(Path::new("/nonexistent/x.jsonl")), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ancestor_chain_finds_self_and_parent() {
        let me = std::process::id();
        assert!(has_ancestor(me, me));
        // Our parent chain reaches pid 1 eventually without panicking.
        assert!(!has_ancestor(me, 4194301));
    }
}

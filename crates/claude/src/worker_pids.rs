//! Which worker started which process.
//!
//! Every Bash command a Claude Code session runs, its own or any of its
//! subagents', is a child of the one `claude` process:
//!
//! ```text
//! /bin/bash -c source <snapshot> … && eval '<command>' < /dev/null && pwd -P >| /tmp/claude-…-cwd
//! ```
//!
//! so the process tree alone cannot say which worker a build belongs to.
//! The command text can: a worker's transcript
//! (`<session>/subagents/agent-<id>.jsonl`) has the Bash `tool_use` with
//! that exact `command`, and it is in flight — no `tool_result` yet — or
//! running in the background. When exactly one worker has it in flight
//! (and the session itself does not), the bash process and everything
//! under it are that worker's. Anything else — the session's own commands,
//! the same command in flight in two workers — stays with the session,
//! never guessed.
//!
//! A pid's match never changes, so it is made once, when the process is
//! first seen ([`Attributor`]); transcripts are read incrementally, only
//! for those.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::session_use::{Proc, subtree};

/// The command Claude Code's Bash wrapper evals, out of the wrapper's `-c`
/// script: the single shell word after `&& eval `, unquoted, when the
/// script ends the way the wrapper's does (`< /dev/null && pwd -P …`).
pub fn eval_command(script: &str) -> Option<String> {
    let at = script.find("&& eval ")? + "&& eval ".len();
    let (word, rest) = shell_word(&script[at..])?;
    rest.trim_start().starts_with("< /dev/null").then_some(word)
}

/// One shell word from the start of `s` — `'…'`, `"…"` (with `\"`, `\\`,
/// `\$`, `` \` `` escapes) and bare characters, run together — and what
/// follows it. `None` for an unterminated quote.
fn shell_word(s: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut chars = s.char_indices().peekable();
    while let Some(&(i, c)) = chars.peek() {
        match c {
            '\'' => {
                chars.next();
                loop {
                    match chars.next()? {
                        (_, '\'') => break,
                        (_, c) => out.push(c),
                    }
                }
            }
            '"' => {
                chars.next();
                loop {
                    match chars.next()? {
                        (_, '"') => break,
                        (_, '\\') => match chars.next()? {
                            (_, c @ ('"' | '\\' | '$' | '`')) => out.push(c),
                            (_, '\n') => {}
                            (_, c) => {
                                out.push('\\');
                                out.push(c);
                            }
                        },
                        (_, c) => out.push(c),
                    }
                }
            }
            '\\' => {
                chars.next();
                if let Some((_, c)) = chars.next() {
                    out.push(c);
                }
            }
            c if c.is_whitespace() => return Some((out, &s[i..])),
            c => {
                chars.next();
                out.push(c);
            }
        }
    }
    Some((out, ""))
}

/// Who wrote a `tool_use`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Who {
    /// The session's own transcript.
    Session,
    /// A worker, by agent id.
    Agent(String),
}

#[derive(Debug, Clone)]
struct Call {
    who: Who,
    id: String,
    background: bool,
}

/// Every Bash `tool_use` in a session's transcripts, by command, and the
/// ids that have their `tool_result`. Fed incrementally: each file is read
/// from where the last read stopped.
#[derive(Debug, Default)]
pub struct CommandIndex {
    read: HashMap<PathBuf, (u64, Vec<u8>)>,
    calls: HashMap<String, Vec<Call>>,
    done: HashSet<String>,
}

impl CommandIndex {
    /// Read what `path` (written by `who`) gained since the last read.
    pub fn update(&mut self, path: &Path, who: &Who) {
        let (offset, partial) = self.read.entry(path.to_path_buf()).or_default();
        let Ok(mut f) = std::fs::File::open(path) else {
            return;
        };
        if f.seek(SeekFrom::Start(*offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            return;
        }
        *offset += buf.len() as u64;
        partial.extend_from_slice(&buf);
        let Some(end) = partial.iter().rposition(|b| *b == b'\n') else {
            return;
        };
        let lines: Vec<u8> = partial.drain(..=end).collect();
        for line in lines.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            self.line(line, who);
        }
    }

    /// One transcript line.
    pub fn line(&mut self, line: &[u8], who: &Who) {
        // Most lines are neither; skip them unparsed.
        let has = |pat: &[u8]| line.windows(pat.len()).any(|w| w == pat);
        if !has(b"\"tool_use\"") && !has(b"\"tool_result\"") {
            return;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            return;
        };
        let Some(content) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            return;
        };
        for c in content {
            match c.get("type").and_then(|t| t.as_str()) {
                Some("tool_use") if c.get("name").and_then(|n| n.as_str()) == Some("Bash") => {
                    let input = c.get("input");
                    let (Some(cmd), Some(id)) = (
                        input
                            .and_then(|i| i.get("command"))
                            .and_then(|c| c.as_str()),
                        c.get("id").and_then(|i| i.as_str()),
                    ) else {
                        continue;
                    };
                    let background = input
                        .and_then(|i| i.get("run_in_background"))
                        .and_then(|b| b.as_bool())
                        .unwrap_or(false);
                    self.calls.entry(cmd.to_string()).or_default().push(Call {
                        who: who.clone(),
                        id: id.to_string(),
                        background,
                    });
                }
                Some("tool_result") => {
                    if let Some(id) = c.get("tool_use_id").and_then(|i| i.as_str()) {
                        self.done.insert(id.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    /// The worker whose call `command` is: the one agent among the calls
    /// with that command still in flight or in the background. `None` when
    /// there is none, the session's own is among them, or two workers are.
    pub fn worker_of(&self, command: &str) -> Option<String> {
        let mut who: Option<&Who> = None;
        for c in self.calls.get(command)? {
            if !c.background && self.done.contains(&c.id) {
                continue;
            }
            match who {
                None => who = Some(&c.who),
                Some(w) if *w == c.who => {}
                Some(_) => return None,
            }
        }
        match who? {
            Who::Agent(id) => Some(id.clone()),
            Who::Session => None,
        }
    }
}

/// How many passes a command that matches no worker yet is tried again
/// (its `tool_use` may not be written yet) before it is the session's.
const TRIES: u8 = 3;

/// Keeps every bash process's worker, once found, and each session's
/// [`CommandIndex`].
#[derive(Debug, Default)]
pub struct Attributor {
    /// `(pid, start)` → the worker, or `None` once given up on.
    known: HashMap<(u32, u64), Option<String>>,
    tries: HashMap<(u32, u64), u8>,
    sessions: HashMap<String, CommandIndex>,
}

/// What [`Attributor::attribute`] needs from the machine, so tests can
/// stand in for `/proc` and `~/.claude`.
pub trait Source {
    /// The bash wrapper's `-c` script, if `pid` is one.
    fn wrapper_script(&self, pid: u32) -> Option<String>;
    /// The session id and the Claude config dir from `pid`'s environment.
    fn session_of(&self, pid: u32) -> Option<(String, Option<PathBuf>)>;
    /// The session's transcripts: its own, and each worker's by agent id.
    fn transcripts(&self, session: &str, config_dir: Option<&Path>) -> Vec<(Who, PathBuf)>;
}

impl Attributor {
    /// Each worker's processes: every child of a claude in `claudes` that
    /// is a Bash wrapper matched to a worker, with all under it.
    pub fn attribute(
        &mut self,
        procs: &[Proc],
        claudes: &HashSet<u32>,
        src: &impl Source,
    ) -> HashMap<String, HashSet<u32>> {
        let alive: HashSet<(u32, u64)> = procs.iter().map(|p| (p.pid, p.start)).collect();
        self.known.retain(|k, _| alive.contains(k));
        self.tries.retain(|k, _| alive.contains(k));
        let mut out: HashMap<String, HashSet<u32>> = HashMap::new();
        for p in procs.iter().filter(|p| claudes.contains(&p.ppid)) {
            let key = (p.pid, p.start);
            let worker = match self.known.get(&key) {
                Some(w) => w.clone(),
                None => {
                    let found = self.find(p.pid, src);
                    if found.is_some() {
                        self.known.insert(key, found.clone());
                    } else {
                        let n = self.tries.entry(key).or_default();
                        *n += 1;
                        if *n >= TRIES {
                            self.known.insert(key, None);
                        }
                    }
                    found
                }
            };
            if let Some(w) = worker {
                out.entry(w).or_default().extend(subtree(procs, p.pid));
            }
        }
        out
    }

    fn find(&mut self, pid: u32, src: &impl Source) -> Option<String> {
        let command = eval_command(&src.wrapper_script(pid)?)?;
        let (session, config_dir) = src.session_of(pid)?;
        let index = self.sessions.entry(session.clone()).or_default();
        for (who, path) in src.transcripts(&session, config_dir.as_deref()) {
            index.update(&path, &who);
        }
        index.worker_of(&command)
    }
}

/// The real machine: `/proc` and the session's transcripts on disk.
pub struct Machine;

impl Source for Machine {
    fn wrapper_script(&self, pid: u32) -> Option<String> {
        let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let mut argv = cmd.split(|b| *b == 0);
        let bin = String::from_utf8_lossy(argv.next()?).into_owned();
        if !(bin.ends_with("/bash") || bin == "bash") || argv.next()? != b"-c" {
            return None;
        }
        Some(String::from_utf8_lossy(argv.next()?).into_owned())
    }

    fn session_of(&self, pid: u32) -> Option<(String, Option<PathBuf>)> {
        let env = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
        let var = |name: &[u8]| {
            env.split(|b| *b == 0)
                .find_map(|kv| kv.strip_prefix(name)?.strip_prefix(b"="))
                .map(|v| String::from_utf8_lossy(v).into_owned())
                .filter(|v| !v.is_empty())
        };
        Some((
            var(b"CLAUDE_CODE_SESSION_ID")?,
            var(b"CLAUDE_CONFIG_DIR").map(PathBuf::from),
        ))
    }

    fn transcripts(&self, session: &str, config_dir: Option<&Path>) -> Vec<(Who, PathBuf)> {
        let cfg = config_dir
            .map(Path::to_path_buf)
            .or_else(|| dirs::home_dir().map(|h| h.join(".claude")));
        let Some(cfg) = cfg else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for project in std::fs::read_dir(cfg.join("projects"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let own = project.path().join(format!("{session}.jsonl"));
            if own.is_file() {
                out.push((Who::Session, own));
            }
            let subagents = project.path().join(session).join("subagents");
            for id in crate::subagents::list_agent_ids(&subagents) {
                out.push((
                    Who::Agent(id.clone()),
                    crate::subagents::agent_transcript(&subagents, &id),
                ));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const WRAP: &str = "source /home/u/.claude/shell-snapshots/snapshot-bash-1-x.sh 2>/dev/null || true && shopt -u extglob 2>/dev/null || true && { \\builtin unalias -- 'unsetenv'; \\builtin unset -f -- 'unsetenv'; } >/dev/null 2>&1 || true && eval ";

    fn wrapped(quoted: &str) -> String {
        format!("{WRAP}{quoted} < /dev/null && pwd -P >| /tmp/claude-e170-cwd")
    }

    #[test]
    fn the_command_comes_out_of_the_wrapper_as_written() {
        // As seen on a live session: single quotes, `'"'"'` for a quote.
        let s = wrapped(
            r#"'until grep -q "peak" /tmp/x.out; do sleep 10; done; tail -n 12 /tmp/x.out | tr '"'"'\r'"'"' '"'"'\n'"'"''"#,
        );
        assert_eq!(
            eval_command(&s).as_deref(),
            Some(
                r#"until grep -q "peak" /tmp/x.out; do sleep 10; done; tail -n 12 /tmp/x.out | tr '\r' '\n'"#
            )
        );
        // Several lines.
        let s = wrapped("'cd /a\ncargo build \\\n  --release'");
        assert_eq!(
            eval_command(&s).as_deref(),
            Some("cd /a\ncargo build \\\n  --release")
        );
        // Not the wrapper's shape: nothing.
        assert_eq!(eval_command("echo hi"), None);
        assert_eq!(eval_command(&format!("{WRAP}'unterminated")), None);
        assert_eq!(eval_command(&format!("{WRAP}'x' | cat")), None);
    }

    fn use_line(id: &str, cmd: &str, bg: bool) -> String {
        json!({"type":"assistant","message":{"role":"assistant","content":[
            {"type":"tool_use","id":id,"name":"Bash","input":{"command":cmd,"run_in_background":bg}}]}})
        .to_string()
    }

    fn result_line(id: &str) -> String {
        json!({"type":"user","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":id,"content":"ok"}]}})
        .to_string()
    }

    fn agent(id: &str) -> Who {
        Who::Agent(id.into())
    }

    #[test]
    fn a_command_in_flight_in_one_worker_is_that_workers() {
        let mut ix = CommandIndex::default();
        ix.line(use_line("t1", "cargo test", false).as_bytes(), &agent("a1"));
        assert_eq!(ix.worker_of("cargo test").as_deref(), Some("a1"));
        // Answered: no longer in flight.
        ix.line(result_line("t1").as_bytes(), &agent("a1"));
        assert_eq!(ix.worker_of("cargo test"), None);
        // In the background, answered at once: still that worker's.
        ix.line(
            use_line("t2", "node sim.mjs", true).as_bytes(),
            &agent("a2"),
        );
        ix.line(result_line("t2").as_bytes(), &agent("a2"));
        assert_eq!(ix.worker_of("node sim.mjs").as_deref(), Some("a2"));
        // The same command in flight in two workers: nobody's.
        ix.line(use_line("t3", "make", false).as_bytes(), &agent("a1"));
        ix.line(use_line("t4", "make", false).as_bytes(), &agent("a2"));
        assert_eq!(ix.worker_of("make"), None);
        // Twice in one worker: still that one's.
        ix.line(use_line("t5", "ls", false).as_bytes(), &agent("a1"));
        ix.line(use_line("t6", "ls", false).as_bytes(), &agent("a1"));
        assert_eq!(ix.worker_of("ls").as_deref(), Some("a1"));
        // The session's own command: the session's.
        ix.line(
            use_line("t7", "git status", false).as_bytes(),
            &Who::Session,
        );
        assert_eq!(ix.worker_of("git status"), None);
        assert_eq!(ix.worker_of("never run"), None);
    }

    #[test]
    fn transcripts_are_read_as_they_grow() {
        let dir = std::env::temp_dir().join(format!("giverny-worker-pids-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-a1.jsonl");
        let line = use_line("t1", "cargo build", false);
        // Half a line: not read yet.
        std::fs::write(&path, &line[..20]).unwrap();
        let mut ix = CommandIndex::default();
        ix.update(&path, &agent("a1"));
        assert_eq!(ix.worker_of("cargo build"), None);
        std::fs::write(&path, format!("{line}\n")).unwrap();
        ix.update(&path, &agent("a1"));
        assert_eq!(ix.worker_of("cargo build").as_deref(), Some("a1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct Fake {
        scripts: HashMap<u32, String>,
        dir: PathBuf,
    }

    impl Source for Fake {
        fn wrapper_script(&self, pid: u32) -> Option<String> {
            self.scripts.get(&pid).cloned()
        }
        fn session_of(&self, _pid: u32) -> Option<(String, Option<PathBuf>)> {
            Some(("s1".into(), None))
        }
        fn transcripts(&self, _: &str, _: Option<&Path>) -> Vec<(Who, PathBuf)> {
            vec![
                (Who::Session, self.dir.join("s1.jsonl")),
                (agent("a1"), self.dir.join("agent-a1.jsonl")),
                (agent("a2"), self.dir.join("agent-a2.jsonl")),
            ]
        }
    }

    fn p(pid: u32, ppid: u32) -> Proc {
        Proc {
            pid,
            ppid,
            ticks: 0,
            mem_kb: 0,
            start: pid as u64,
        }
    }

    #[test]
    fn a_workers_commands_and_all_under_them_are_its() {
        let dir = std::env::temp_dir().join(format!("giverny-worker-attr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let w = |name: &str, lines: &[String]| {
            std::fs::write(dir.join(name), lines.join("\n") + "\n").unwrap();
        };
        w(
            "agent-a1.jsonl",
            &[
                use_line("t1", "node sim.mjs --jobs 4", true),
                result_line("t1"),
            ],
        );
        w("agent-a2.jsonl", &[use_line("t2", "cargo test", false)]);
        w("s1.jsonl", &[use_line("t3", "git log", false)]);
        let q = |c: &str| wrapped(&format!("'{c}'"));
        let src = Fake {
            scripts: HashMap::from([
                (11, q("node sim.mjs --jobs 4")),
                (21, q("cargo test")),
                (31, q("git log")),
            ]),
            dir: dir.clone(),
        };
        // claude 10 → bash 11 → node 12 → worker 13; bash 21 → cargo 22;
        // bash 31 (the session's own); claude's own child 40 (no wrapper).
        let procs = [
            p(10, 1),
            p(11, 10),
            p(12, 11),
            p(13, 12),
            p(21, 10),
            p(22, 21),
            p(31, 10),
            p(40, 10),
        ];
        let mut a = Attributor::default();
        let got = a.attribute(&procs, &HashSet::from([10]), &src);
        assert_eq!(got["a1"], HashSet::from([11, 12, 13]));
        assert_eq!(got["a2"], HashSet::from([21, 22]));
        assert_eq!(got.len(), 2, "the session's own command is nobody's");
        // Once matched, kept: the call answered since changes nothing.
        w(
            "agent-a2.jsonl",
            &[use_line("t2", "cargo test", false), result_line("t2")],
        );
        let again = a.attribute(&procs, &HashSet::from([10]), &src);
        assert_eq!(again["a2"], HashSet::from([21, 22]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

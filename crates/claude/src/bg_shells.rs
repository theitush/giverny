//! A task worker's own background shells (giverny#472): recorded as it
//! starts them, checked when it ends its turn, and named to its manager.
//! Giverny suggests, managers decide: nothing here stops or signals
//! anything.
//!
//! `giverny-manage pause` lists a task's live `run` commands (giverny#471),
//! but a worker's Bash `run_in_background` shells ran on unseen: that is
//! what kept poly's task 170 going after its worker ended its turn.
//!
//! - **Recorded.** The plugin's `PostToolUse` hook sees a worker's Bash
//!   call that left a shell in the background (its response carries
//!   `backgroundTaskId`, measured on Claude Code 2.1.303; a call its
//!   timeout moved there carries it too). When the worker holds a row of
//!   its manager session, the shell is recorded against that row's task:
//!   its id, the worker, the command and the time ([`record`]).
//! - **At its turn's end.** The worker's `SubagentStop` payload lists the
//!   session's whole task registry in `background_tasks` (2.1.303: the
//!   manager's shells and every worker's alike, each with `id`, `type`,
//!   `status`, `command`, but no owner). A recorded shell no longer in
//!   flight there has ended and drops off; one still running is told to
//!   the manager once, on its next tool call ([`turn_ended`], [`deliver`]):
//!   "`g472`'s worker ended its turn; 1 background shell still running:
//!   `sleep 600` (40m) …".
//! - **On `pause` and `resume`** the task's live shells are listed with how
//!   long each has run and how to stop it ([`task_shells`], [`lines`]).
//!
//! **How to stop one.** The registry is the session's, so the manager's
//! own `TaskStop <shell id>` stops a worker's shell (measured on 2.1.303:
//! "Successfully stopped task: bq4h1i6dn (sleep 601)", its process gone).
//! That is always named. A pid is named only when it is certain
//! ([`Probe`]): the shell's output file, `…/<session>/tasks/<id>.output`,
//! is held open by exactly one top process, a session leader (Claude Code
//! starts each command's `bash -c` wrapper in a session of its own) whose
//! `eval` is the recorded command. Then `pkill -s <pid>` stops the wrapper
//! and all it started (the command itself runs in another process group of
//! that session, so a group kill would miss it).
//!
//! **Dropping off.** A shell drops off when a `SubagentStop` no longer
//! lists it in flight, when a look at `/proc` finds nothing holding its
//! output file, or two days after it started. The file goes when its last
//! shell does.
//!
//! **Cheap.** A `PostToolUse` writes only for a background shell; a
//! `SubagentStop` with no record `stat`s one file; a manager's other calls
//! `stat` the tell file, there only while a notice waits.
//!
//! **Files**, under `<feed dir>/bgshells/`: `<session>.json`, the shells
//! recorded; `<session>.tell.json`, the notices the manager has yet to
//! hear.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::manage::{self, Lock};

/// The directory under the feed dir the files live in.
pub const DIR: &str = "bgshells";

/// A record is kept at most this long, live or not.
pub const KEEP_MS: u64 = 2 * 24 * 60 * 60 * 1000;

/// A worker's background shell, as recorded when it started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shell {
    /// Claude Code's task id for it (`backgroundTaskId`).
    pub id: String,
    /// The worker that started it.
    pub agent: String,
    /// The task its worker held.
    pub task: String,
    /// Its command, as the worker gave it.
    pub cmd: String,
    /// When it started, epoch ms.
    pub started_ms: u64,
}

/// What a look at a shell finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Found {
    /// Whether anything still runs it; `None` where that cannot be read.
    pub running: Option<bool>,
    /// Its wrapper's pid, only when certain: what `pkill -s` would stop.
    pub pid: Option<u32>,
}

/// A live shell, as `pause`, `resume` and the notice name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live {
    pub shell: Shell,
    pub pid: Option<u32>,
}

/// How shells are looked at: the system's processes, or a test's.
pub trait Probe {
    /// One look at each of `session`'s `shells`, in order.
    fn look(&self, session: &str, shells: &[Shell]) -> Vec<Found>;
}

/// The real look: on Linux, `/proc` for the processes holding each shell's
/// output file open. Elsewhere nothing is known.
pub struct System;

impl Probe for System {
    fn look(&self, session: &str, shells: &[Shell]) -> Vec<Found> {
        look_proc(session, shells)
    }
}

/// The processes holding each shell's output file, and the one top among
/// them when it is certainly the shell's wrapper. The caller's own
/// ancestors (the claude that runs it, which may be reading the file) are
/// not the shell's.
#[cfg(target_os = "linux")]
fn look_proc(session: &str, shells: &[Shell]) -> Vec<Found> {
    use std::collections::HashSet;
    if shells.is_empty() {
        return Vec::new();
    }
    let table = crate::session_use::proc_table(4);
    if table.is_empty() {
        return vec![Found::default(); shells.len()];
    }
    let parent: std::collections::HashMap<u32, u32> =
        table.iter().map(|p| (p.pid, p.ppid)).collect();
    let mut mine = HashSet::new();
    let mut at = std::process::id();
    while mine.insert(at) {
        match parent.get(&at) {
            Some(&p) if p > 1 => at = p,
            _ => break,
        }
    }
    let wanted: Vec<String> = shells
        .iter()
        .map(|s| format!("/{session}/tasks/{}.output", s.id))
        .collect();
    let mut holders: Vec<Vec<u32>> = vec![Vec::new(); shells.len()];
    for p in &table {
        if mine.contains(&p.pid) {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(format!("/proc/{}/fd", p.pid)) else {
            continue; // another user's, or gone
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let Some(target) = target.to_str() else {
                continue;
            };
            for (i, w) in wanted.iter().enumerate() {
                if target.ends_with(w.as_str()) && !holders[i].contains(&p.pid) {
                    holders[i].push(p.pid);
                }
            }
        }
    }
    shells
        .iter()
        .zip(holders)
        .map(|(shell, pids)| {
            if pids.is_empty() {
                return Found {
                    running: Some(false),
                    pid: None,
                };
            }
            let tops: Vec<u32> = pids
                .iter()
                .copied()
                .filter(|p| parent.get(p).is_none_or(|pp| !pids.contains(pp)))
                .collect();
            let pid = match tops[..] {
                [top] if wrapper_of(top, &shell.cmd) => Some(top),
                _ => None,
            };
            Found {
                running: Some(true),
                pid,
            }
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn look_proc(_session: &str, shells: &[Shell]) -> Vec<Found> {
    vec![Found::default(); shells.len()]
}

/// Whether `pid` is Claude Code's wrapper for `cmd`: a session leader
/// (`/proc/<pid>/stat` field 6 is itself) running `bash -c …`, whose
/// script evals exactly `cmd` ([`crate::worker_pids::eval_command`]).
#[cfg(target_os = "linux")]
fn wrapper_of(pid: u32, cmd: &str) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return false;
    };
    // Fields count from `state` (3): `session` is 6.
    let sid = rest
        .split_whitespace()
        .nth(3)
        .and_then(|s| s.parse::<u32>().ok());
    if sid != Some(pid) {
        return false;
    }
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let argv: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
    let script = match argv.iter().position(|a| *a == b"-c") {
        Some(i) => argv.get(i + 1).copied().unwrap_or_default(),
        None => return false,
    };
    crate::worker_pids::eval_command(&String::from_utf8_lossy(script)).as_deref() == Some(cmd)
}

/// A notice the manager has yet to hear: a worker ended its turn with
/// these still running.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Tell {
    task: String,
    agent: String,
    at_ms: u64,
    shells: Vec<Shell>,
}

/// The shells recorded in a session.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Book {
    #[serde(default)]
    shells: Vec<Shell>,
}

fn safe(session: &str) -> bool {
    !session.is_empty()
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `<feed dir>/bgshells/<session>.json`: the shells recorded.
pub fn path(feed_dir: &Path, session: &str) -> PathBuf {
    feed_dir.join(DIR).join(format!("{session}.json"))
}

/// `<feed dir>/bgshells/<session>.tell.json`: the notices to come.
pub fn tell_path(feed_dir: &Path, session: &str) -> PathBuf {
    feed_dir.join(DIR).join(format!("{session}.tell.json"))
}

fn read<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Write the book, or remove its file when it holds nothing.
fn put(file: &Path, book: &Book) {
    if book.shells.is_empty() {
        let _ = std::fs::remove_file(file);
    } else {
        let _ = manage::write(file, &json!(book));
    }
}

/// Record `shell`, started by a worker holding its task. Records past
/// [`KEEP_MS`] go.
pub fn record(feed_dir: &Path, session: &str, shell: Shell, now: u64) {
    if !safe(session) || shell.id.is_empty() {
        return;
    }
    let file = path(feed_dir, session);
    let Some(dir) = file.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(_lock) = Lock::take(&file) else { return };
    let mut book: Book = read(&file);
    book.shells
        .retain(|s| s.id != shell.id && now.saturating_sub(s.started_ms) < KEEP_MS);
    book.shells.push(shell);
    put(&file, &book);
}

/// Whether shell `id` is recorded as a task worker's: its turn-end notice
/// is this module's, so [`crate::leftovers`] does not tell it again.
pub fn is_recorded(feed_dir: &Path, session: &str, id: &str) -> bool {
    if !safe(session) {
        return false;
    }
    let file = path(feed_dir, session);
    if std::fs::metadata(&file).is_err() {
        return false;
    }
    read::<Book>(&file).shells.iter().any(|s| s.id == id)
}

/// A worker's `SubagentStop` was let through: its turn ended. `in_flight`
/// is the payload's `background_tasks` ids still running or pending (the
/// session's whole registry). A recorded shell not among them, or that
/// `probe` finds nothing runs, has ended and drops off; the worker's shells
/// still running are queued to its manager, once for this turn's end.
pub fn turn_ended(
    feed_dir: &Path,
    session: &str,
    agent: &str,
    in_flight: &[&str],
    now: u64,
    probe: &dyn Probe,
) {
    if !safe(session) {
        return;
    }
    let file = path(feed_dir, session);
    if std::fs::metadata(&file).is_err() {
        return;
    }
    let Ok(_lock) = Lock::take(&file) else { return };
    let mut book: Book = read(&file);
    book.shells.retain(|s| {
        in_flight.contains(&s.id.as_str()) && now.saturating_sub(s.started_ms) < KEEP_MS
    });
    let theirs: Vec<Shell> = book
        .shells
        .iter()
        .filter(|s| s.agent == agent)
        .cloned()
        .collect();
    let found = probe.look(session, &theirs);
    let ended: Vec<&str> = theirs
        .iter()
        .zip(&found)
        .filter(|(_, f)| f.running == Some(false))
        .map(|(s, _)| s.id.as_str())
        .collect();
    let live: Vec<Shell> = theirs
        .iter()
        .filter(|s| !ended.contains(&s.id.as_str()))
        .cloned()
        .collect();
    let ended: Vec<String> = ended.into_iter().map(String::from).collect();
    book.shells.retain(|s| !ended.contains(&s.id));
    put(&file, &book);
    let tell_file = tell_path(feed_dir, session);
    let mut tell: Vec<Tell> = read(&tell_file);
    tell.retain(|t| t.agent != agent);
    if let Some(task) = live.first().map(|s| s.task.clone()) {
        tell.push(Tell {
            task,
            agent: agent.to_string(),
            at_ms: now,
            shells: live,
        });
    }
    if tell.is_empty() {
        let _ = std::fs::remove_file(&tell_file);
    } else {
        let _ = manage::write(&tell_file, &json!(tell));
    }
}

/// The manager's notices, on its own hook call, each once: a `stat` when
/// there are none. A shell that ended since its worker's turn did is left
/// out.
pub fn deliver(feed_dir: &Path, session: &str, now: u64, probe: &dyn Probe) -> Option<String> {
    if !safe(session) {
        return None;
    }
    let tell_file = tell_path(feed_dir, session);
    std::fs::metadata(&tell_file).ok()?;
    let _lock = Lock::take(&path(feed_dir, session)).ok()?;
    let tell: Vec<Tell> = read(&tell_file);
    let _ = std::fs::remove_file(&tell_file);
    let lines: Vec<String> = tell
        .into_iter()
        .filter_map(|t| {
            let found = probe.look(session, &t.shells);
            let live: Vec<Live> = t
                .shells
                .into_iter()
                .zip(found)
                .filter(|(_, f)| f.running != Some(false))
                .map(|(shell, f)| Live { shell, pid: f.pid })
                .collect();
            (!live.is_empty()).then(|| notice(&t.task, &live, now))
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n\n"))
}

/// The turn-end notice: "`g472`'s worker ended its turn; 1 background shell
/// still running: `sleep 600` (40m) …".
pub fn notice(task: &str, live: &[Live], now: u64) -> String {
    let n = live.len();
    let (what, it) = if n == 1 {
        ("1 background shell still running".to_string(), "it")
    } else {
        (format!("{n} background shells still running"), "each")
    };
    let list: Vec<String> = live
        .iter()
        .map(|l| {
            format!(
                "`{}` ({}, shell {})",
                crate::strays::quote(&l.shell.cmd),
                ran(l.shell.started_ms, now),
                l.shell.id
            )
        })
        .collect();
    let stop: Vec<String> = live.iter().map(stop_with).collect();
    format!(
        "Giverny: `{task}`'s worker ended its turn; {what}: {}. Giverny stops nothing: if \
         the work is meant to stop, stop {it} ({}), or let {it} run on if needed.",
        list.join(", "),
        stop.join("; ")
    )
}

/// How long a shell has run: `40m`, `12s`.
fn ran(started_ms: u64, now: u64) -> String {
    let s = now.saturating_sub(started_ms) / 1000;
    match s {
        0..60 => format!("{s}s"),
        _ => crate::feed::fmt_span(s as i64),
    }
}

/// What stops a shell: `TaskStop <id>` from the session, which always
/// works on a worker's shell, and its wrapper's session when its pid is
/// certain.
fn stop_with(l: &Live) -> String {
    match l.pid {
        Some(pid) => format!("`TaskStop {}` or `pkill -s {pid}`", l.shell.id),
        None => format!("`TaskStop {}`", l.shell.id),
    }
}

/// `task`'s live background shells in `session`, oldest first. Those
/// `probe` finds ended drop off the record.
pub fn task_shells(feed_dir: &Path, session: &str, task: &str, probe: &dyn Probe) -> Vec<Live> {
    if !safe(session) {
        return Vec::new();
    }
    let file = path(feed_dir, session);
    if std::fs::metadata(&file).is_err() {
        return Vec::new();
    }
    let Ok(_lock) = Lock::take(&file) else {
        return Vec::new();
    };
    let mut book: Book = read(&file);
    let found = probe.look(session, &book.shells);
    let mut live = Vec::new();
    let mut ended = Vec::new();
    for (s, f) in book.shells.iter().zip(found) {
        if f.running == Some(false) {
            ended.push(s.id.clone());
        } else if s.task == task {
            live.push(Live {
                shell: s.clone(),
                pid: f.pid,
            });
        }
    }
    if !ended.is_empty() {
        book.shells.retain(|s| !ended.contains(&s.id));
        put(&file, &book);
    }
    live.sort_by_key(|l| l.shell.started_ms);
    live
}

/// Each live shell as a line: `` `sleep 600` (shell bjj5phx0d), 40m in:
/// `TaskStop bjj5phx0d` from this session stops it, or `pkill -s 4242` ``.
pub fn lines(live: &[Live], now: u64) -> Vec<String> {
    live.iter()
        .map(|l| {
            let how = match l.pid {
                Some(pid) => format!(
                    "`TaskStop {id}` from this session stops it, or `pkill -s {pid}` (its \
                     shell, pid {pid}, and all it started)",
                    id = l.shell.id
                ),
                None => format!(
                    "`TaskStop {}` from this session stops it (its pid is not certain)",
                    l.shell.id
                ),
            };
            format!(
                "`{}` (shell {}), {} in: {how}",
                crate::strays::quote(&l.shell.cmd),
                l.shell.id,
                ran(l.shell.started_ms, now)
            )
        })
        .collect()
}

/// What `pause` and `resume` add for `task`'s live shells: nothing when
/// none runs.
pub fn still_running(task: &str, live: &[Live], now: u64) -> String {
    if live.is_empty() {
        return String::new();
    }
    let what = match live.len() {
        1 => "1 background shell its worker started still runs".to_string(),
        n => format!("{n} background shells its worker started still run"),
    };
    let mut out = format!("\n  {what} for {task} (Giverny stops nothing):");
    for line in lines(live, now) {
        out.push_str(&format!("\n    {line}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const T0: u64 = 1_790_000_000_000;
    const MIN: u64 = 60_000;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("giverny-bg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A probe that says what it was told: ids not listed are unknown.
    #[derive(Default)]
    struct Fake {
        found: RefCell<Vec<(String, Found)>>,
    }

    impl Fake {
        fn set(&self, id: &str, running: Option<bool>, pid: Option<u32>) {
            let mut f = self.found.borrow_mut();
            f.retain(|(i, _)| i != id);
            f.push((id.into(), Found { running, pid }));
        }
    }

    impl Probe for Fake {
        fn look(&self, _session: &str, shells: &[Shell]) -> Vec<Found> {
            let f = self.found.borrow();
            shells
                .iter()
                .map(|s| {
                    f.iter()
                        .find(|(i, _)| *i == s.id)
                        .map(|(_, x)| *x)
                        .unwrap_or_default()
                })
                .collect()
        }
    }

    fn shell(id: &str, agent: &str, task: &str, cmd: &str, at: u64) -> Shell {
        Shell {
            id: id.into(),
            agent: agent.into(),
            task: task.into(),
            cmd: cmd.into(),
            started_ms: at,
        }
    }

    #[test]
    fn a_worker_ending_its_turn_with_a_shell_running_is_told_once() {
        let dir = temp("told");
        let probe = Fake::default();
        record(&dir, "s1", shell("b1", "a1", "g472", "sleep 600", T0), T0);
        record(&dir, "s1", shell("b2", "a1", "g472", "make", T0), T0);
        record(&dir, "s1", shell("b3", "a2", "other", "tail -f x", T0), T0);
        assert!(is_recorded(&dir, "s1", "b1"));
        assert_eq!(deliver(&dir, "s1", T0, &probe), None, "nothing yet");
        // b2 finished before the turn ended: not in flight.
        turn_ended(&dir, "s1", "a1", &["b1", "b3", "a1"], T0 + 40 * MIN, &probe);
        assert!(!is_recorded(&dir, "s1", "b2"), "finished: dropped");
        assert!(is_recorded(&dir, "s1", "b3"), "another worker's stays");
        probe.set("b1", Some(true), Some(4242));
        assert_eq!(
            deliver(&dir, "s1", T0 + 41 * MIN, &probe).as_deref(),
            Some(
                "Giverny: `g472`'s worker ended its turn; 1 background shell still running: \
                 `sleep 600` (41m, shell b1). Giverny stops nothing: if the work is meant to \
                 stop, stop it (`TaskStop b1` or `pkill -s 4242`), or let it run on if needed."
            )
        );
        assert_eq!(deliver(&dir, "s1", T0 + 42 * MIN, &probe), None, "once");
        // pause lists it, and it is still recorded: nothing was stopped.
        let live = task_shells(&dir, "s1", "g472", &probe);
        assert_eq!(live.len(), 1);
        assert_eq!(
            still_running("g472", &live, T0 + 50 * MIN),
            "\n  1 background shell its worker started still runs for g472 (Giverny stops \
             nothing):\n    `sleep 600` (shell b1), 50m in: `TaskStop b1` from this session \
             stops it, or `pkill -s 4242` (its shell, pid 4242, and all it started)"
        );
        assert!(task_shells(&dir, "s1", "nope", &probe).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shells_that_ended_drop_off_and_their_file_goes() {
        let dir = temp("drop");
        let probe = Fake::default();
        record(&dir, "s1", shell("b1", "a1", "t", "sleep 9", T0), T0);
        record(&dir, "s1", shell("b2", "a1", "t", "sleep 9", T0), T0);
        // Ended between turns: the look finds nothing holding it.
        probe.set("b1", Some(false), None);
        let live = task_shells(&dir, "s1", "t", &probe);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].shell.id, "b2");
        assert!(live[0].pid.is_none());
        assert!(still_running("t", &live, T0 + 5 * MIN).contains(
            "`sleep 9` (shell b2), 5m in: `TaskStop b2` from this session stops it (its pid \
                 is not certain)"
        ));
        // A turn's end with nothing in flight: the rest drop, the file goes,
        // and nothing is told.
        turn_ended(&dir, "s1", "a1", &[], T0 + 6 * MIN, &probe);
        assert!(!path(&dir, "s1").exists());
        assert_eq!(deliver(&dir, "s1", T0 + 7 * MIN, &probe), None);
        // An old record goes on the next write.
        record(&dir, "s1", shell("b3", "a1", "t", "x", T0), T0);
        record(
            &dir,
            "s1",
            shell("b4", "a1", "t", "y", T0 + KEEP_MS),
            T0 + KEEP_MS,
        );
        assert!(!is_recorded(&dir, "s1", "b3"));
        assert!(is_recorded(&dir, "s1", "b4"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_shell_ended_before_the_notice_is_left_out() {
        let dir = temp("late");
        let probe = Fake::default();
        record(&dir, "s1", shell("b1", "a1", "t", "sleep 600", T0), T0);
        record(
            &dir,
            "s1",
            shell("b2", "a1", "t", "npm run sim -- --long", T0 + MIN),
            T0,
        );
        turn_ended(&dir, "s1", "a1", &["b1", "b2"], T0 + 2 * MIN, &probe);
        probe.set("b1", Some(false), None);
        let told = deliver(&dir, "s1", T0 + 3 * MIN, &probe).unwrap();
        assert_eq!(
            told,
            "Giverny: `t`'s worker ended its turn; 1 background shell still running: `npm run \
             sim -- --long` (2m, shell b2). Giverny stops nothing: if the work is meant to stop, \
             stop it (`TaskStop b2`), or let it run on if needed."
        );
        // Both still running: listed together.
        let probe = Fake::default();
        let dir2 = temp("both");
        record(&dir2, "s1", shell("b1", "a1", "t", "sleep 600", T0), T0);
        record(&dir2, "s1", shell("b2", "a1", "t", "sleep 700", T0), T0);
        turn_ended(&dir2, "s1", "a1", &["b1", "b2"], T0 + 30_000, &probe);
        let both = deliver(&dir2, "s1", T0 + 30_000, &probe).unwrap();
        assert!(
            both.contains("2 background shells still running: `sleep 600` (30s, shell b1), `sleep 700` (30s, shell b2)"),
            "{both}"
        );
        assert!(
            both.contains("stop each (`TaskStop b1`; `TaskStop b2`)"),
            "{both}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn no_record_reads_nothing_and_bad_sessions_are_refused() {
        let dir = temp("none");
        let probe = Fake::default();
        turn_ended(&dir, "s1", "a1", &["b1"], T0, &probe);
        assert!(!dir.join(DIR).exists(), "nothing written");
        record(&dir, "../x", shell("b1", "a1", "t", "x", T0), T0);
        assert!(!dir.join(DIR).exists());
        assert!(task_shells(&dir, "s1", "t", &probe).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real look on a real shell started like Claude Code starts one: a
    /// session-leading `bash -c` with the command's `eval`, its output file
    /// `…/<session>/tasks/<id>.output` as stdout. Its pid is found, and
    /// nothing is stopped; a shell whose file nothing holds has ended.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_real_look_finds_a_wrapper_and_its_pid() {
        use std::os::unix::process::CommandExt;
        let dir = temp("real");
        let session = "0d5e2a4c-0000-4000-8000-00000000beef";
        let tasks = dir.join(session).join("tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        let out = std::fs::File::create(tasks.join("bzz1real.output")).unwrap();
        let script = "true && eval 'sleep 30' < /dev/null && pwd -P >| /dev/null";
        let mut child = std::process::Command::new("/bin/bash")
            .args(["-c", script])
            .stdout(out)
            .process_group(0)
            .spawn()
            .unwrap();
        // A session of its own, as Claude Code's are: setsid in the child.
        // (process_group alone is not a session; the pid is then not
        // certain, which is what this checks first.)
        std::thread::sleep(std::time::Duration::from_millis(300));
        let shells = [
            shell("bzz1real", "a1", "t", "sleep 30", T0),
            shell("bzz1gone", "a1", "t", "sleep 30", T0),
        ];
        let found = System.look(session, &shells);
        assert_eq!(found[0].running, Some(true));
        assert_eq!(found[0].pid, None, "not a session leader: not certain");
        assert_eq!(found[1].running, Some(false));
        // SAFETY: a signal to the group the test's own child leads.
        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        let _ = child.wait();
        let out = std::fs::File::create(tasks.join("bzz2real.output")).unwrap();
        // SAFETY: `setsid` is async-signal-safe, all `pre_exec` may call.
        let mut child = unsafe {
            std::process::Command::new("/bin/bash")
                .args(["-c", script])
                .stdout(out)
                .pre_exec(|| {
                    libc::setsid();
                    Ok(())
                })
                .spawn()
                .unwrap()
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        let shells = [
            shell("bzz2real", "a1", "t", "sleep 30", T0),
            shell("bzz2real", "a1", "t", "sleep 31", T0),
        ];
        let found = System.look(session, &shells);
        assert_eq!(found[0].pid, Some(child.id()), "{found:?}");
        assert_eq!(found[1].running, Some(true));
        assert_eq!(found[1].pid, None, "another command: not certain");
        assert!(crate::run_live::alive(child.id()), "nothing stopped");
        // SAFETY: a signal to the session's group the test's own child leads.
        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! What a session is told about work leaving its tab's limits, and the
//! mark of a session whose plugin hooks run (giverny#271).
//!
//! Ita's call: Giverny reminds, it does not take charge; the slice's
//! memory cap stays the floor. Three parts live here:
//!
//! - **Reminders, once a kind.** The plugin's `PreToolUse` Bash guard
//!   ([`crate::bash_guard`]) lets an escape run and tells the session once
//!   per kind of escape (`systemd-run`, a cgroup write, `wsl.exe`, …): the
//!   kinds told are kept here ([`remind`]).
//! - **Escapes found afterwards.** What the guard cannot see — a command
//!   built at run time, a script file, a program that calls `systemd-run`
//!   itself — the app's sampler finds on its next pass ([`Sweep`]): a
//!   process below a capped claude whose cgroup is outside
//!   [`crate::tab_cap::SLICE`], or one reparented away from it that
//!   carries a tab's `GIVERNY_TAB_ID` and the `CLAUDE_PID` of a capped
//!   claude in its environment. Each is recorded once ([`record`]), the
//!   topmost of an escaped tree only, and the session is told on its next
//!   tool call or prompt ([`deliver`]), as strays are ([`crate::strays`],
//!   giverny#450): pid, command, "yours?". Nothing is moved back.
//! - **The hook mark.** Every plugin hook call of a tab's session leaves
//!   `<feeds>/hooked/<session>` ([`mark_hooked`]); a session the
//!   management panel shows with none ran no plugin hook at all (the
//!   plugin off, or a claude started before it): no reminders, no early
//!   cap, and the panel marks it ([`hooked`]).
//!
//! **Files:** `<feed dir>/escapes/<session>.json` — `told` (the kinds
//! reminded of) and `found` (the escapes not yet told); `<feed
//! dir>/hooked/<session>`, empty.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::bash_guard::Escape;
use crate::manage::{self, Lock};
use crate::session_use::Proc;
use crate::tab_cap::{SLICE, in_slice};

/// The directory under the feed dir.
pub const DIR: &str = "escapes";

/// The hook marks' directory under the feed dir.
pub const HOOKED_DIR: &str = "hooked";

/// How many escapes waiting to be told one session's file keeps.
const FOUND_MAX: usize = 20;

/// A hook mark older than this is cleared at a session's start (a live
/// session's next hook call writes it again).
const HOOKED_KEEP_MS: u64 = 30 * 24 * 3_600_000;

fn safe(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 128
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `<feed dir>/escapes/<session>.json`.
pub fn path(feed_dir: &Path, session: &str) -> PathBuf {
    feed_dir.join(DIR).join(format!("{session}.json"))
}

/// One escape found by the sampler, not yet told.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    pub pid: u32,
    /// Its start, clock ticks after boot: with the pid, which process.
    pub start: u64,
    /// Its command line, as a line quotes it.
    pub cmd: String,
    /// The cgroup it runs in (its last part: `run-u12.scope`).
    pub cgroup: String,
    #[serde(default)]
    pub seen_ms: u64,
}

/// A session's file.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Doc {
    #[serde(default)]
    told: Vec<String>,
    #[serde(default)]
    found: Vec<Found>,
}

fn read(file: &Path) -> Doc {
    std::fs::read(file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// What the session is told before its command runs: the reminder for
/// each kind in `found` it has not been told of yet, those kinds kept as
/// told. With no session known (or none that names a file), every kind is
/// said.
pub fn remind(feed_dir: &Path, session: Option<&str>, found: &[Escape]) -> Option<String> {
    if found.is_empty() {
        return None;
    }
    let Some(session) = session.filter(|s| safe(s)) else {
        return Some(crate::bash_guard::reminder(
            &found.iter().collect::<Vec<_>>(),
        ));
    };
    let file = path(feed_dir, session);
    std::fs::create_dir_all(file.parent()?).ok()?;
    let _lock = Lock::take(&file).ok()?;
    let mut doc = read(&file);
    let new: Vec<&Escape> = found
        .iter()
        .filter(|e| !doc.told.iter().any(|t| t == e.kind))
        .collect();
    if new.is_empty() {
        return None;
    }
    doc.told.extend(new.iter().map(|e| e.kind.to_string()));
    manage::write(&file, &json!(doc)).ok()?;
    Some(crate::bash_guard::reminder(&new))
}

/// Add what the sampler found to its sessions' files, each process once.
pub fn record(feed_dir: &Path, found: &[(String, Found)]) {
    let mut by: HashMap<&str, Vec<&Found>> = HashMap::new();
    for (s, f) in found {
        if safe(s) {
            by.entry(s.as_str()).or_default().push(f);
        }
    }
    for (session, fs) in by {
        let file = path(feed_dir, session);
        if std::fs::create_dir_all(feed_dir.join(DIR)).is_err() {
            return;
        }
        let Ok(_lock) = Lock::take(&file) else {
            continue;
        };
        let mut doc = read(&file);
        for f in fs {
            if !doc
                .found
                .iter()
                .any(|d| d.pid == f.pid && d.start == f.start)
            {
                doc.found.push(f.clone());
            }
        }
        let over = doc.found.len().saturating_sub(FOUND_MAX);
        doc.found.drain(..over);
        let _ = manage::write(&file, &json!(doc));
    }
}

/// The session's escapes found since it was last told, said once: on its
/// next tool call or prompt. A `stat` when there is no file.
pub fn deliver(feed_dir: &Path, session: &str) -> Option<String> {
    if !safe(session) {
        return None;
    }
    let file = path(feed_dir, session);
    std::fs::metadata(&file).ok()?;
    let _lock = Lock::take(&file).ok()?;
    let mut doc = read(&file);
    if doc.found.is_empty() {
        return None;
    }
    let found = std::mem::take(&mut doc.found);
    manage::write(&file, &json!(doc)).ok()?;
    Some(told(&found))
}

/// The words for escapes found afterwards.
pub fn told(found: &[Found]) -> String {
    let line = |f: &Found| {
        format!(
            "pid {} `{}` (in {}, not {SLICE})",
            f.pid,
            crate::strays::quote(&f.cmd),
            f.cgroup
        )
    };
    let tail = |it: &str| {
        format!(
            "Yours? Giverny does not move {it} back: heavy work goes through \
             `giverny-manage run <task> -- <command>`, or stays in the tab."
        )
    };
    if let [one] = found {
        return format!(
            "Giverny found a process of this session running outside its tab's limits: {}. \
             {}",
            line(one),
            tail("it")
        );
    }
    let list: Vec<String> = found.iter().map(|f| format!("- {}", line(f))).collect();
    format!(
        "Giverny found processes of this session running outside its tab's limits:\n{}\n{}",
        list.join("\n"),
        tail("them")
    )
}

/// How the sampler looks at a process: the system's `/proc`, or a test's.
pub trait Look {
    /// Its `/proc/<pid>/cgroup` text.
    fn cgroup(&self, pid: u32) -> Option<String>;
    /// `GIVERNY_TAB_ID`, `CLAUDE_PID` and `CLAUDE_CODE_SESSION_ID` from
    /// its environment, when it is readable (the user's own).
    fn environ(&self, pid: u32) -> Option<Env>;
    /// Its command line.
    fn cmdline(&self, pid: u32) -> Option<String>;
    /// A claude's session (its registry file).
    fn session_of(&self, claude: u32, start: u64) -> Option<String>;
}

/// What the sweep reads of a process's environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    pub tab: Option<String>,
    pub claude: Option<u32>,
    pub session: Option<String>,
}

impl Env {
    /// Read from `/proc/<pid>/environ`'s bytes.
    pub fn parse(bytes: &[u8]) -> Env {
        let mut e = Env::default();
        for kv in bytes.split(|&b| b == 0) {
            let Ok(kv) = std::str::from_utf8(kv) else {
                continue;
            };
            let Some((k, v)) = kv.split_once('=') else {
                continue;
            };
            match k {
                "GIVERNY_TAB_ID" => e.tab = Some(v.to_string()),
                "CLAUDE_PID" => e.claude = v.trim().parse().ok(),
                manage::SESSION_ENV => e.session = Some(v.to_string()),
                _ => {}
            }
        }
        e
    }
}

/// The real look, at `/proc`.
#[derive(Debug)]
pub struct System {
    config_dirs: Vec<PathBuf>,
}

impl System {
    pub fn new() -> System {
        System {
            config_dirs: crate::registry::ambient_config_dirs(None),
        }
    }
}

impl Default for System {
    fn default() -> Self {
        System::new()
    }
}

impl Look for System {
    fn cgroup(&self, pid: u32) -> Option<String> {
        std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()
    }
    fn environ(&self, pid: u32) -> Option<Env> {
        std::fs::read(format!("/proc/{pid}/environ"))
            .ok()
            .map(|b| Env::parse(&b))
    }
    fn cmdline(&self, pid: u32) -> Option<String> {
        let b = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let s: Vec<String> = b
            .split(|&c| c == 0)
            .filter(|w| !w.is_empty())
            .map(|w| String::from_utf8_lossy(w).into_owned())
            .collect();
        (!s.is_empty()).then(|| s.join(" "))
    }
    fn session_of(&self, claude: u32, start: u64) -> Option<String> {
        crate::registry::session_of_pid(&self.config_dirs, claude, start)
    }
}

/// The sampler's sweep for escapes, each pass ([`crate::tab_cap::Adopter`]).
///
/// - Every process below a capped claude has its cgroup read each pass
///   (one small file each): outside the slice, it left (a
///   `systemd-run --scope`, a cgroup write).
/// - Every other process is looked at once, when first seen: outside the
///   slice, with a tab's `GIVERNY_TAB_ID` and a capped claude's
///   `CLAUDE_PID` in its environment, it left and was reparented (its
///   parent ended, or a program double-forked it out).
///
/// Only the topmost process of an escaped tree is told; what it starts
/// after is known as its, and told never. What is outside the user
/// manager entirely (`wsl.exe`'s `/init.scope`, carrying no tab's
/// environment) or a `systemd-run` service (the manager's environment,
/// not the caller's) is not found.
#[derive(Debug, Default)]
pub struct Sweep {
    /// Processes outside every capped tree, looked at once (pid → start).
    looked: HashMap<u32, u64>,
    /// Processes found escaped, told or under one told (pid → start).
    escaped: HashMap<u32, u64>,
    /// Claudes ever seen capped: an orphan carrying one's pid is theirs.
    capped_before: HashSet<u32>,
}

impl Sweep {
    /// One pass: the escapes new since the last, each with its session.
    /// `capped` are the outermost claudes in the slice, `claudes` every
    /// claude.
    pub fn tick(
        &mut self,
        procs: &[Proc],
        capped: &HashSet<u32>,
        claudes: &HashSet<u32>,
        look: &dyn Look,
        now_ms: u64,
    ) -> Vec<(String, Found)> {
        let start: HashMap<u32, u64> = procs.iter().map(|p| (p.pid, p.start)).collect();
        let parent: HashMap<u32, u32> = procs.iter().map(|p| (p.pid, p.ppid)).collect();
        let live = |m: &mut HashMap<u32, u64>| m.retain(|p, s| start.get(p) == Some(s));
        live(&mut self.looked);
        live(&mut self.escaped);
        if self.capped_before.len() > 4096 {
            self.capped_before.clear();
        }
        self.capped_before.extend(capped);
        if self.capped_before.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut under: HashSet<u32> = HashSet::new();
        let mut roots: Vec<u32> = capped.iter().copied().collect();
        roots.sort_unstable();
        for root in roots {
            let mut tree: Vec<u32> = crate::session_use::subtree(procs, root)
                .into_iter()
                .collect();
            // Parents before children: pids grow, mostly; a parent's
            // verdict is what a child's is read against.
            tree.sort_unstable_by_key(|p| (depth_of(*p, root, &parent), *p));
            for p in tree {
                under.insert(p);
                if p == root || claudes.contains(&p) {
                    continue;
                }
                let st = start.get(&p).copied().unwrap_or_default();
                if self.escaped.get(&p) == Some(&st) {
                    continue;
                }
                let Some(cg) = look.cgroup(p) else { continue };
                if in_slice(&cg) {
                    continue;
                }
                self.escaped.insert(p, st);
                if parent
                    .get(&p)
                    .is_some_and(|pp| self.escaped.contains_key(pp))
                {
                    continue; // below one found: its
                }
                let session = look
                    .environ(p)
                    .and_then(|e| e.session)
                    .or_else(|| look.session_of(root, start.get(&root).copied().unwrap_or(0)));
                if let Some(s) = session {
                    out.push((s, self.found(look, p, st, &cg, now_ms)));
                }
            }
        }
        let mut rest: Vec<&Proc> = procs
            .iter()
            .filter(|p| !under.contains(&p.pid) && !claudes.contains(&p.pid))
            .filter(|p| self.looked.get(&p.pid) != Some(&p.start))
            .collect();
        rest.sort_unstable_by_key(|p| p.start);
        for p in rest {
            self.looked.insert(p.pid, p.start);
            if self.escaped.contains_key(&p.ppid) {
                self.escaped.insert(p.pid, p.start);
                continue;
            }
            let Some(cg) = look.cgroup(p.pid) else {
                continue;
            };
            if in_slice(&cg) {
                continue;
            }
            let Some(env) = look.environ(p.pid) else {
                continue;
            };
            let (Some(_tab), Some(claude)) = (&env.tab, env.claude) else {
                continue;
            };
            let capped_claude = self.capped_before.contains(&claude)
                || claudes.contains(&claude) && look.cgroup(claude).is_some_and(|c| in_slice(&c));
            if !capped_claude {
                continue;
            }
            self.escaped.insert(p.pid, p.start);
            let session = env
                .session
                .clone()
                .or_else(|| look.session_of(claude, start.get(&claude).copied().unwrap_or(0)));
            if let Some(s) = session {
                out.push((s, self.found(look, p.pid, p.start, &cg, now_ms)));
            }
        }
        out
    }

    fn found(&self, look: &dyn Look, pid: u32, start: u64, cg: &str, now_ms: u64) -> Found {
        let cgroup = cg
            .lines()
            .find_map(|l| l.strip_prefix("0::"))
            .map(|p| p.trim().rsplit('/').next().unwrap_or("").to_string())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "the root cgroup".into());
        Found {
            pid,
            start,
            cmd: look.cmdline(pid).unwrap_or_else(|| format!("pid {pid}")),
            cgroup,
            seen_ms: now_ms,
        }
    }
}

/// How far below `root` `pid` sits (`parent` walked up).
fn depth_of(pid: u32, root: u32, parent: &HashMap<u32, u32>) -> u32 {
    let mut d = 0;
    let mut p = pid;
    while p != root && d < 64 {
        match parent.get(&p) {
            Some(&pp) if pp != p => p = pp,
            _ => break,
        }
        d += 1;
    }
    d
}

/// `<feed dir>/hooked/<session>`.
pub fn hooked_path(feed_dir: &Path, session: &str) -> PathBuf {
    feed_dir.join(HOOKED_DIR).join(session)
}

/// A plugin hook ran for `session`: leave its mark, once. A `stat` when
/// it is there.
pub fn mark_hooked(feed_dir: &Path, session: &str) {
    if !safe(session) {
        return;
    }
    let f = hooked_path(feed_dir, session);
    if std::fs::metadata(&f).is_ok() {
        return;
    }
    if std::fs::create_dir_all(feed_dir.join(HOOKED_DIR)).is_ok() {
        let _ = std::fs::File::create(&f);
    }
}

/// Whether a plugin hook ever ran for any of `ids` (a session and the
/// earlier ids of its conversation).
pub fn hooked(feed_dir: &Path, ids: &[&str]) -> bool {
    ids.iter()
        .any(|s| safe(s) && std::fs::metadata(hooked_path(feed_dir, s)).is_ok())
}

/// Clear hook marks older than [`HOOKED_KEEP_MS`] (at a session's start).
pub fn prune_hooked(feed_dir: &Path) {
    let Ok(rd) = std::fs::read_dir(feed_dir.join(HOOKED_DIR)) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age.as_millis() as u64 > HOOKED_KEEP_MS);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("giverny-escapes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn esc(cmd: &str) -> Vec<Escape> {
        crate::bash_guard::escapes(cmd)
    }

    #[test]
    fn each_kind_is_reminded_once_a_session() {
        let dir = temp("remind");
        let first = remind(&dir, Some("s1"), &esc("systemd-run --user --scope make"));
        assert!(
            first
                .as_deref()
                .is_some_and(|r| r.contains("`systemd-run`")),
            "{first:?}"
        );
        // The second of a kind: nothing.
        assert_eq!(
            remind(&dir, Some("s1"), &esc("systemd-run --user --scope x")),
            None
        );
        // A new kind alone is said, the old one not again.
        let both = remind(
            &dir,
            Some("s1"),
            &esc("nohup systemd-run --user --scope x &"),
        )
        .unwrap();
        assert!(
            both.contains("`nohup`") && !both.contains("`systemd-run`"),
            "{both}"
        );
        // Another session hears of it afresh.
        assert!(remind(&dir, Some("s2"), &esc("systemd-run --user --scope x")).is_some());
        // Nothing found, nothing said.
        assert_eq!(remind(&dir, Some("s3"), &esc("cargo build")), None);
    }

    fn p(pid: u32, ppid: u32, start: u64) -> Proc {
        Proc {
            pid,
            ppid,
            ticks: 0,
            mem_kb: 0,
            start,
        }
    }

    const IN: &str = "0::/user.slice/user-1000.slice/user@1000.service/giverny.slice/giverny-claude.slice/giverny-claude-10.scope\n";
    const OUT: &str = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/run-u7.scope\n";
    const TAB_SHELL: &str =
        "0::/user.slice/user-1000.slice/user@1000.service/app.slice/giverny.scope\n";

    #[derive(Default)]
    struct Fake {
        cg: HashMap<u32, &'static str>,
        env: HashMap<u32, Env>,
        reads: std::cell::RefCell<Vec<u32>>,
    }

    impl Look for Fake {
        fn cgroup(&self, pid: u32) -> Option<String> {
            self.reads.borrow_mut().push(pid);
            self.cg.get(&pid).map(|s| s.to_string())
        }
        fn environ(&self, pid: u32) -> Option<Env> {
            self.env.get(&pid).cloned()
        }
        fn cmdline(&self, pid: u32) -> Option<String> {
            Some(format!("cargo build --pid {pid}"))
        }
        fn session_of(&self, claude: u32, _: u64) -> Option<String> {
            (claude == 10).then(|| "s-claude".into())
        }
    }

    fn env(session: &str) -> Env {
        Env {
            tab: Some("7".into()),
            claude: Some(10),
            session: Some(session.into()),
        }
    }

    #[test]
    fn a_sweep_found_escape_is_told_once() {
        // app 1 → tab shell 5 → claude 10 (capped) → bash 11 → cargo 12,
        // which a `systemd-run --scope` put outside, with rustc 13 under it.
        let mut procs = vec![
            p(1, 0, 1),
            p(5, 1, 2),
            p(10, 5, 3),
            p(11, 10, 4),
            p(12, 11, 5),
        ];
        let mut fake = Fake::default();
        for pid in [10, 11] {
            fake.cg.insert(pid, IN);
        }
        fake.cg.insert(1, TAB_SHELL);
        fake.cg.insert(5, TAB_SHELL);
        fake.cg.insert(12, OUT);
        fake.cg.insert(13, OUT);
        fake.env.insert(12, env("s1"));
        let capped: HashSet<u32> = [10].into();
        let claudes: HashSet<u32> = [10].into();
        let mut sweep = Sweep::default();
        let found = sweep.tick(&procs, &capped, &claudes, &fake, 100);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, "s1");
        assert_eq!(found[0].1.pid, 12);
        assert_eq!(found[0].1.cgroup, "run-u7.scope");
        // Its child, and the next pass: nothing new.
        procs.push(p(13, 12, 6));
        assert!(sweep.tick(&procs, &capped, &claudes, &fake, 200).is_empty());

        // Told on the next call, once.
        let dir = temp("sweep");
        record(&dir, &found);
        record(&dir, &found);
        let said = deliver(&dir, "s1").unwrap();
        assert!(
            said.contains("pid 12 `cargo build --pid 12` (in run-u7.scope"),
            "{said}"
        );
        assert!(said.contains("Yours?"), "{said}");
        assert_eq!(deliver(&dir, "s1"), None);
        assert_eq!(deliver(&dir, "s2"), None);
    }

    #[test]
    fn an_orphan_carrying_a_capped_claudes_pid_is_found_and_a_tab_shells_work_is_not() {
        // 20: escaped, its parent gone (reparented to 1); 30: a plain
        // shell tab's command, outside the slice as it should be; 40: a
        // process of an uncapped claude's.
        let procs = vec![
            p(1, 0, 1),
            p(10, 5, 3),
            p(20, 1, 9),
            p(21, 20, 10),
            p(30, 5, 11),
            p(40, 1, 12),
        ];
        let mut fake = Fake::default();
        fake.cg.insert(10, IN);
        for pid in [1, 20, 21, 30, 40] {
            fake.cg.insert(pid, OUT);
        }
        fake.env.insert(20, env("s9"));
        fake.env.insert(21, env("s9"));
        fake.env.insert(
            30,
            Env {
                tab: Some("7".into()),
                ..Env::default()
            },
        );
        fake.env.insert(
            40,
            Env {
                tab: Some("8".into()),
                claude: Some(99),
                session: Some("other".into()),
            },
        );
        let mut sweep = Sweep::default();
        let found = sweep.tick(&procs, &[10].into(), &[10].into(), &fake, 1);
        let pids: Vec<u32> = found.iter().map(|f| f.1.pid).collect();
        assert_eq!(pids, [20]);
        assert_eq!(found[0].0, "s9");
        // Each outside process is looked at once.
        fake.reads.borrow_mut().clear();
        assert!(
            sweep
                .tick(&procs, &[10].into(), &[10].into(), &fake, 2)
                .is_empty()
        );
        assert!(
            !fake
                .reads
                .borrow()
                .iter()
                .any(|p| [20, 21, 30, 40].contains(p))
        );
    }

    #[test]
    fn with_no_capped_claude_nothing_is_read() {
        let procs = vec![p(1, 0, 1), p(20, 1, 2)];
        let fake = Fake::default();
        let mut sweep = Sweep::default();
        assert!(
            sweep
                .tick(&procs, &HashSet::new(), &HashSet::new(), &fake, 1)
                .is_empty()
        );
        assert!(fake.reads.borrow().is_empty());
    }

    #[test]
    fn the_environment_is_read_for_the_three_names() {
        let e =
            Env::parse(b"A=1\0GIVERNY_TAB_ID=12\0CLAUDE_PID=4242\0CLAUDE_CODE_SESSION_ID=abc-1\0");
        assert_eq!(
            e,
            Env {
                tab: Some("12".into()),
                claude: Some(4242),
                session: Some("abc-1".into()),
            }
        );
    }

    #[test]
    fn a_hook_mark_is_left_once_and_read_by_any_id() {
        let dir = temp("hooked");
        assert!(!hooked(&dir, &["s1", "old"]));
        mark_hooked(&dir, "old");
        assert!(hooked(&dir, &["s1", "old"]));
        assert!(!hooked(&dir, &["s1"]));
        mark_hooked(&dir, "../x");
        assert!(!dir.join("x").exists());
        prune_hooked(&dir);
        assert!(hooked(&dir, &["old"]));
    }

    /// The sweep on this machine's `/proc` (giverny#271's live check):
    /// `G271_CLAUDE` names a capped claude, `G271_FEEDS` a scratch feed
    /// dir; prints what it finds and what its session would be told.
    #[test]
    #[ignore]
    #[cfg(target_os = "linux")]
    fn live_sweep_finds_what_left_the_slice() {
        let claude: u32 = std::env::var("G271_CLAUDE").unwrap().parse().unwrap();
        let feeds = PathBuf::from(std::env::var("G271_FEEDS").unwrap());
        let procs = crate::session_use::proc_table(4);
        let set: HashSet<u32> = [claude].into();
        let mut sweep = Sweep::default();
        let found = sweep.tick(&procs, &set, &set, &System::new(), 1);
        for (s, f) in &found {
            println!(
                "found: session {s} pid {} `{}` in {}",
                f.pid, f.cmd, f.cgroup
            );
        }
        assert!(!found.is_empty());
        assert!(sweep.tick(&procs, &set, &set, &System::new(), 2).is_empty());
        record(&feeds, &found);
        for s in found.iter().map(|f| f.0.as_str()).collect::<HashSet<_>>() {
            println!("told {s}: {:?}", deliver(&feeds, s));
            assert_eq!(deliver(&feeds, s), None);
        }
    }
}

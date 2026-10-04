//! What the whole Claude Code session uses right now, for the status line:
//! CPU as a share of the machine, memory, and GPU memory when the machine
//! has an NVIDIA GPU.
//!
//! The session is the `claude` process the status line command runs under
//! and every process below it: its Bash commands, their builds, `giverny
//! pass run` commands (a `systemd-run --scope` execs the command in place,
//! so a capped command stays in the tree), background shells. Its
//! subagents run inside the `claude` process itself.
//!
//! The status line command is itself a child of that claude, so the walk
//! goes up to the nearest claude ancestor ([`crate::lineage`]'s test of a
//! claude) and sums its subtree from `/proc`. CPU needs two readings: the
//! last one is kept in a small file per session, and the share is the
//! CPU time spent since then over the wall time since then. With no
//! recent reading (the first line of a session, or one long quiet), it
//! reads twice [`SHORT_SAMPLE`] apart. GPU memory comes from `nvidia-smi
//! --query-compute-apps`, which is slow: a copy of its answer is kept and
//! refreshed in the background, never waited on.
//!
//! Linux only. Elsewhere there is nothing to show and nothing is spent.

use std::collections::{HashMap, HashSet};
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

/// The session's use now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionUse {
    /// CPU, percent of the whole machine (every core busy is 100).
    pub cpu_pct: u32,
    /// Resident memory summed over the session's processes, MiB.
    pub mem_mb: u64,
    /// GPU memory of the session's processes, MiB; `None` when the
    /// machine has no NVIDIA GPU (or its answer is not in yet).
    pub gpu_mb: Option<u64>,
}

/// One process as the tree sum needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    /// CPU time in clock ticks: its own and its reaped children's
    /// (`utime + stime + cutime + cstime`), so a command that ended is
    /// still counted, in its parent.
    pub ticks: u64,
    /// Resident set, KiB.
    pub rss_kb: u64,
}

/// `root` and every process below it.
pub fn subtree(procs: &[Proc], root: u32) -> HashSet<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs {
        if p.pid != p.ppid {
            children.entry(p.ppid).or_default().push(p.pid);
        }
    }
    let mut seen = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if seen.insert(pid) {
            stack.extend(children.get(&pid).into_iter().flatten().copied());
        }
    }
    seen.retain(|pid| procs.iter().any(|p| p.pid == *pid));
    seen
}

/// `(ticks, rss KiB)` summed over the processes in `pids`.
pub fn sum(procs: &[Proc], pids: &HashSet<u32>) -> (u64, u64) {
    procs
        .iter()
        .filter(|p| pids.contains(&p.pid))
        .fold((0, 0), |(t, m), p| (t + p.ticks, m + p.rss_kb))
}

/// CPU spent between two readings, percent of `cores` cores, rounded and
/// at most 100. A total that went down (a process left the tree with its
/// time) reads as nothing rather than wrapping.
pub fn cpu_pct(ticks_then: u64, ticks_now: u64, ticks_per_s: u64, over_ms: u64, cores: u32) -> u32 {
    if over_ms == 0 || cores == 0 || ticks_per_s == 0 {
        return 0;
    }
    let used_ms = ticks_now.saturating_sub(ticks_then) as f64 * 1000.0 / ticks_per_s as f64;
    (used_ms * 100.0 / (over_ms as f64 * cores as f64))
        .round()
        .clamp(0.0, 100.0) as u32
}

/// A size in G, the agents pane's way: `0.0G` for nothing, `0.1G` at the
/// least for anything, tenths below ten (`4.2G`), whole above (`12G`), T
/// from a thousand G.
pub fn gb(mb: u64) -> String {
    if mb == 0 {
        return "0.0G".into();
    }
    let mut v = mb as f64 / 1024.0;
    let mut unit = 'G';
    if v >= 999.5 {
        v /= 1024.0;
        unit = 'T';
    }
    if v < 9.95 {
        format!("{:.1}{unit}", v.max(0.1))
    } else {
        format!("{:.0}{unit}", v.min(999.0))
    }
}

/// `45% CPU`, `4.2G` and, with a GPU, `gpu 1.2G`.
pub fn segments(u: &SessionUse) -> Vec<String> {
    let mut out = vec![format!("{}% CPU", u.cpu_pct.min(100)), gb(u.mem_mb)];
    if let Some(g) = u.gpu_mb {
        out.push(format!("gpu {}", gb(g)));
    }
    out
}

/// Separates the status line's segments.
pub const SEP: &str = "  ·  ";

/// The fewest blanks between the line's left part and its right-aligned
/// part, so a reader of the screen can tell the two apart (a [`SEP`] has
/// two).
pub const MIN_GAP: usize = 3;

/// How many columns Claude Code takes from the terminal's width around the
/// status line: it is drawn two columns in, and a line that reaches the
/// last two columns is cut short with `…` (measured on Claude Code 2.1,
/// whatever `padding` says).
pub const CLAUDE_MARGIN: usize = 4;

/// The status line's width out of the terminal's (`$COLUMNS`, which
/// Claude Code sets for the command).
pub fn line_width(columns: Option<&str>) -> Option<usize> {
    let cols: usize = columns?.trim().parse().ok()?;
    cols.checked_sub(CLAUDE_MARGIN).filter(|w| *w > 0)
}

/// `left` with `right` at the right edge of a `width`-column line, at
/// least [`MIN_GAP`] blanks between; just appended after a [`SEP`] when
/// the width is unknown. Columns are counted as characters: everything
/// the line holds (`·`, model names, figures) is one column wide.
pub fn align(left: &str, right: &str, width: Option<usize>) -> String {
    if right.is_empty() {
        return left.to_string();
    }
    if left.is_empty() {
        return match width {
            Some(w) => format!("{right:>w$}"),
            None => right.to_string(),
        };
    }
    let Some(width) = width else {
        return format!("{left}{SEP}{right}");
    };
    let used = left.chars().count() + right.chars().count();
    let gap = width.saturating_sub(used).max(MIN_GAP);
    format!("{left}{}{right}", " ".repeat(gap))
}

/// The session's use now, for the session named `session_id` (the key of
/// the remembered reading). `None` when this does not run under a claude
/// or the platform has no `/proc`.
#[cfg(target_os = "linux")]
pub fn measure(session_id: Option<&str>) -> Option<SessionUse> {
    let me = std::process::id();
    let root = claude_root(me)?;
    let dir = dirs::cache_dir()?.join("giverny").join("statusline");
    let key = session_id
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .map_or_else(|| format!("pid-{root}"), str::to_string);
    let cache = dir.join(format!("{key}.cpu"));
    let ticks_per_s = sysconf(libc::_SC_CLK_TCK).unwrap_or(100);
    let page_kb = sysconf(libc::_SC_PAGESIZE).unwrap_or(4096) / 1024;
    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);

    let read = || {
        let procs = proc_table(page_kb);
        let tree = subtree(&procs, root);
        let (ticks, rss_kb) = sum(&procs, &tree);
        (tree, ticks, rss_kb, now_ms())
    };
    let (tree, ticks, rss_kb, at) = read();
    let prev = read_cache(&cache).filter(|c| c.root == root && c.at_ms <= at);
    let (cpu_pct_now, write) = match prev {
        // Asked again at once (Claude Code redraws in bursts): too short
        // a stretch to measure, so the last figure stands.
        Some(c) if at - c.at_ms < MIN_SPAN_MS => (c.pct, None),
        Some(c) if at - c.at_ms <= MAX_SPAN_MS => {
            let pct = cpu_pct(c.ticks, ticks, ticks_per_s, at - c.at_ms, cores);
            (pct, Some((ticks, at, pct)))
        }
        _ => {
            std::thread::sleep(SHORT_SAMPLE);
            let (_, ticks2, _, at2) = read();
            let pct = cpu_pct(ticks, ticks2, ticks_per_s, at2.saturating_sub(at), cores);
            (pct, Some((ticks2, at2, pct)))
        }
    };
    if let Some((ticks, at_ms, pct)) = write {
        write_cache(
            &dir,
            &cache,
            &Cached {
                root,
                ticks,
                at_ms,
                pct,
            },
        );
    }
    let gpu_mb = gpu::session_mb(&dir, &tree);
    Some(SessionUse {
        cpu_pct: cpu_pct_now,
        mem_mb: rss_kb.div_ceil(1024),
        gpu_mb,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn measure(_session_id: Option<&str>) -> Option<SessionUse> {
    None
}

/// Two readings closer than this say nothing about CPU.
#[cfg(target_os = "linux")]
const MIN_SPAN_MS: u64 = 300;
/// A reading older than this says nothing about now.
#[cfg(target_os = "linux")]
const MAX_SPAN_MS: u64 = 30_000;
/// The gap between two readings taken on the spot: long enough for a few
/// clock ticks, short enough for a line Claude Code waits for.
#[cfg(target_os = "linux")]
const SHORT_SAMPLE: std::time::Duration = std::time::Duration::from_millis(60);

#[cfg(target_os = "linux")]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(target_os = "linux")]
fn sysconf(name: libc::c_int) -> Option<u64> {
    // SAFETY: sysconf only reads a configuration value.
    let v = unsafe { libc::sysconf(name) };
    u64::try_from(v).ok().filter(|v| *v > 0)
}

/// The claude this process runs under: the nearest claude among its
/// ancestors, else `$CLAUDE_PID` (which Claude Code sets for its commands)
/// when that is one of them.
#[cfg(target_os = "linux")]
fn claude_root(me: u32) -> Option<u32> {
    let mut ancestors = Vec::new();
    let mut pid = me;
    for _ in 0..64 {
        match crate::lineage::proc_parent(pid) {
            Some(p) if p != pid && p > 1 => {
                if crate::lineage::proc_is_claude(p) {
                    return Some(p);
                }
                ancestors.push(p);
                pid = p;
            }
            _ => break,
        }
    }
    std::env::var("CLAUDE_PID")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|p| ancestors.contains(p))
}

/// Every process in `/proc`.
#[cfg(target_os = "linux")]
fn proc_table(page_kb: u64) -> Vec<Proc> {
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            parse_stat(pid, &stat, page_kb)
        })
        .collect()
}

/// One `/proc/<pid>/stat` line. The fields after the parenthesized name
/// (which may hold spaces or parentheses itself) count from `state`
/// (field 3): `ppid` is field 4, `utime`…`cstime` 14–17, `rss` (pages) 24.
pub fn parse_stat(pid: u32, stat: &str, page_kb: u64) -> Option<Proc> {
    let (_, rest) = stat.rsplit_once(')')?;
    let f: Vec<&str> = rest.split_whitespace().collect();
    let n = |field: usize| f.get(field - 3)?.parse::<i64>().ok();
    let ticks = (14..=17).map(|i| n(i).unwrap_or(0).max(0) as u64).sum();
    Some(Proc {
        pid,
        ppid: n(4)? as u32,
        ticks,
        rss_kb: n(24).unwrap_or(0).max(0) as u64 * page_kb,
    })
}

/// The last reading, kept between two status lines.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cached {
    root: u32,
    ticks: u64,
    at_ms: u64,
    pct: u32,
}

#[cfg(target_os = "linux")]
fn read_cache(path: &Path) -> Option<Cached> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut f = text.split_whitespace().map(|w| w.parse::<u64>().ok());
    Some(Cached {
        root: f.next()?? as u32,
        ticks: f.next()??,
        at_ms: f.next()??,
        pct: f.next()?? as u32,
    })
}

#[cfg(target_os = "linux")]
fn write_cache(dir: &Path, path: &PathBuf, c: &Cached) {
    let _ = std::fs::create_dir_all(dir);
    let tmp = path.with_extension(format!("cpu.{}.tmp", std::process::id()));
    let body = format!("{} {} {} {}\n", c.root, c.ticks, c.at_ms, c.pct);
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
    // Sessions come and go; their readings go a day later.
    sweep(dir);
}

/// Remove readings untouched for a day, now and then.
#[cfg(target_os = "linux")]
fn sweep(dir: &Path) {
    if !now_ms().is_multiple_of(50) {
        return;
    }
    let day = std::time::Duration::from_secs(24 * 3600);
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > day);
        if old && e.path().extension().is_some_and(|x| x == "cpu") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// GPU memory per process, from a kept copy of `nvidia-smi`'s answer.
pub mod gpu {
    use super::*;

    /// `pid, used_memory` lines (MiB, `--format=csv,noheader,nounits`);
    /// a line that does not read is skipped.
    pub fn parse_compute_apps(out: &str) -> HashMap<u32, u64> {
        let mut m = HashMap::new();
        for l in out.lines() {
            let mut f = l.split(',').map(str::trim);
            let (Some(pid), Some(mb)) = (f.next(), f.next()) else {
                continue;
            };
            if let (Ok(pid), Ok(mb)) = (pid.parse::<u32>(), mb.parse::<u64>()) {
                *m.entry(pid).or_default() += mb;
            }
        }
        m
    }

    /// How old the kept answer may be before a fresh one is asked for.
    #[cfg(target_os = "linux")]
    const FRESH_MS: u64 = 5_000;

    /// The session's GPU memory, MiB: `None` with no `nvidia-smi` on the
    /// machine or no answer kept yet. Asks for a fresh answer in the
    /// background when the kept one is old, and never waits for it.
    #[cfg(target_os = "linux")]
    pub fn session_mb(dir: &Path, tree: &HashSet<u32>) -> Option<u64> {
        let smi = find_on_path("nvidia-smi")?;
        let answer = dir.join("gpu.csv");
        let asked = dir.join("gpu.asked");
        let age = |p: &Path| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| d.as_millis() as u64)
        };
        let stale = age(&answer).is_none_or(|a| a > FRESH_MS);
        if stale && age(&asked).is_none_or(|a| a > FRESH_MS) {
            refresh(dir, &smi, &answer, &asked);
        }
        let text = std::fs::read_to_string(&answer).ok()?;
        // An answer older than a minute is no longer about now.
        if age(&answer).is_some_and(|a| a > 60_000) {
            return None;
        }
        Some(
            parse_compute_apps(&text)
                .iter()
                .filter(|(pid, _)| tree.contains(pid))
                .map(|(_, mb)| mb)
                .sum(),
        )
    }

    /// Run `nvidia-smi` detached, its answer written beside the readings.
    #[cfg(target_os = "linux")]
    fn refresh(dir: &Path, smi: &Path, answer: &Path, asked: &Path) {
        use std::process::{Command, Stdio};
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(asked, b"");
        let tmp = answer.with_extension("csv.tmp");
        let q = |p: &str| format!("'{}'", p.replace('\'', r"'\''"));
        let script = format!(
            "{} --query-compute-apps=pid,used_memory --format=csv,noheader,nounits > {} && mv {} {}",
            q(&smi.to_string_lossy()),
            q(&tmp.to_string_lossy()),
            q(&tmp.to_string_lossy()),
            q(&answer.to_string_lossy()),
        );
        let _ = Command::new("sh")
            .args(["-c", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }

    #[cfg(target_os = "linux")]
    /// `name` in a `$PATH` directory. Windows drives mounted under WSL
    /// (`/mnt/c/…`, a score of them on a usual `$PATH`) are skipped: each
    /// look there costs milliseconds, and a Linux `nvidia-smi` is never
    /// there (WSL's own is in `/usr/lib/wsl/lib`).
    pub(super) fn find_on_path(name: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .filter(|d| !d.starts_with("/mnt"))
            .chain(std::iter::once(PathBuf::from("/usr/lib/wsl/lib")))
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    }
}

// ---- every session at once: the sidebar's line ----------------------------

/// The sessions in a process table: every claude with no claude above it.
/// A claude started inside another session is part of that session's tree,
/// so it is counted there and not again.
pub fn session_roots(procs: &[Proc], is_claude: impl Fn(u32) -> bool) -> Vec<u32> {
    let parent: HashMap<u32, u32> = procs.iter().map(|p| (p.pid, p.ppid)).collect();
    let mut roots: Vec<u32> = procs
        .iter()
        .filter(|p| is_claude(p.pid))
        .filter(|p| {
            let mut pid = p.ppid;
            for _ in 0..64 {
                if pid <= 1 {
                    return true;
                }
                if is_claude(pid) {
                    return false;
                }
                match parent.get(&pid) {
                    Some(&pp) if pp != pid => pid = pp,
                    _ => return true,
                }
            }
            true
        })
        .map(|p| p.pid)
        .collect();
    roots.sort_unstable();
    roots
}

/// Every session's use summed, from one reading of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AllUse {
    /// How many sessions are running.
    pub sessions: usize,
    pub total: SessionUse,
}

/// Sum `roots`' trees. CPU is each session's ticks since `prev` (the last
/// reading's ticks per session) over `over_ms`; a session not in `prev`
/// (just started, or the first reading) adds no CPU yet. Returns the sum
/// and this reading's ticks per session, for the next one. GPU memory is
/// summed only when `gpu` (MiB per pid) is given.
pub fn sum_sessions(
    procs: &[Proc],
    roots: &[u32],
    prev: &HashMap<u32, u64>,
    over_ms: u64,
    ticks_per_s: u64,
    cores: u32,
    gpu: Option<&HashMap<u32, u64>>,
) -> (AllUse, HashMap<u32, u64>) {
    let mut now = HashMap::new();
    let (mut cpu, mut rss_kb, mut gpu_mb) = (0u32, 0u64, 0u64);
    for &root in roots {
        let tree = subtree(procs, root);
        let (ticks, rss) = sum(procs, &tree);
        if let Some(&then) = prev.get(&root) {
            cpu += cpu_pct(then, ticks, ticks_per_s, over_ms, cores);
        }
        rss_kb += rss;
        if let Some(g) = gpu {
            gpu_mb += g
                .iter()
                .filter(|(pid, _)| tree.contains(pid))
                .map(|(_, mb)| mb)
                .sum::<u64>();
        }
        now.insert(root, ticks);
    }
    let all = AllUse {
        sessions: roots.len(),
        total: SessionUse {
            cpu_pct: cpu.min(100),
            mem_mb: rss_kb.div_ceil(1024),
            gpu_mb: gpu.map(|_| gpu_mb),
        },
    };
    (all, now)
}

/// Reads every session on the machine, again and again, from a thread of
/// its own (it reads all of `/proc`, and asks `nvidia-smi`, which is slow).
/// Every claude counts, in Giverny's tabs or not: the machine's load is
/// what the line is about.
#[derive(Debug, Default)]
pub struct AllSessions {
    prev: HashMap<u32, u64>,
    prev_at: Option<std::time::Instant>,
    gpu: Option<(HashMap<u32, u64>, std::time::Instant)>,
}

impl AllSessions {
    /// How often `nvidia-smi` is asked again.
    #[cfg(target_os = "linux")]
    const GPU_EVERY: std::time::Duration = std::time::Duration::from_secs(6);

    /// Every session's use now; `None` where there is no `/proc`. The
    /// first reading has no CPU yet (nothing to diff against).
    #[cfg(target_os = "linux")]
    pub fn sample(&mut self) -> Option<AllUse> {
        let ticks_per_s = sysconf(libc::_SC_CLK_TCK).unwrap_or(100);
        let page_kb = sysconf(libc::_SC_PAGESIZE).unwrap_or(4096) / 1024;
        let cores = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1);
        let procs = proc_table(page_kb);
        if procs.is_empty() {
            return None;
        }
        let claudes: HashSet<u32> = procs
            .iter()
            .map(|p| p.pid)
            .filter(|&pid| crate::lineage::proc_is_claude(pid))
            .collect();
        let roots = session_roots(&procs, |pid| claudes.contains(&pid));
        let now = std::time::Instant::now();
        if self
            .gpu
            .as_ref()
            .is_none_or(|(_, at)| at.elapsed() >= Self::GPU_EVERY)
        {
            self.gpu = gpu::find_on_path("nvidia-smi").and_then(|smi| {
                let out = std::process::Command::new(smi)
                    .args([
                        "--query-compute-apps=pid,used_memory",
                        "--format=csv,noheader,nounits",
                    ])
                    .stdin(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .output()
                    .ok()
                    .filter(|o| o.status.success())?;
                Some((
                    gpu::parse_compute_apps(&String::from_utf8_lossy(&out.stdout)),
                    now,
                ))
            });
        }
        let over_ms = self
            .prev_at
            .map_or(0, |at| now.duration_since(at).as_millis() as u64);
        let (all, ticks) = sum_sessions(
            &procs,
            &roots,
            &self.prev,
            over_ms,
            ticks_per_s,
            cores,
            self.gpu.as_ref().map(|(m, _)| m),
        );
        self.prev = ticks;
        self.prev_at = Some(now);
        Some(all)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn sample(&mut self) -> Option<AllUse> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, ticks: u64, rss_kb: u64) -> Proc {
        Proc {
            pid,
            ppid,
            ticks,
            rss_kb,
        }
    }

    #[test]
    fn the_session_is_claude_and_everything_below_it() {
        // init → shell → claude → {bash → cargo → rustc, sh → statusline},
        // and a neighbour claude with its own child.
        let procs = [
            p(1, 0, 5, 10),
            p(10, 1, 1, 100),
            p(20, 10, 100, 400_000),
            p(30, 20, 2, 4_000),
            p(31, 30, 50, 100_000),
            p(32, 31, 400, 900_000),
            p(40, 20, 0, 1_000),
            p(41, 40, 1, 2_000),
            p(50, 10, 999, 999_999),
            p(51, 50, 999, 999_999),
        ];
        let tree = subtree(&procs, 20);
        let mut got: Vec<u32> = tree.iter().copied().collect();
        got.sort();
        assert_eq!(got, [20, 30, 31, 32, 40, 41]);
        assert_eq!(sum(&procs, &tree), (553, 1_407_000));
        // A root that is gone is nothing.
        assert!(subtree(&procs, 77).is_empty());
        // A parent loop does not hang the walk.
        let looped = [p(5, 6, 1, 1), p(6, 5, 1, 1)];
        assert_eq!(subtree(&looped, 5).len(), 2);
    }

    #[test]
    fn stat_lines_are_read_past_an_awkward_name() {
        let line = "4242 (we ird) (name)) S 20 4242 4242 0 -1 4194560 100 0 0 0 \
                    150 50 7 3 20 0 1 0 12345 104857600 2048 18446744073709551615";
        let got = parse_stat(4242, line, 4).unwrap();
        assert_eq!(got.ppid, 20);
        assert_eq!(got.ticks, 150 + 50 + 7 + 3);
        assert_eq!(got.rss_kb, 2048 * 4);
        assert_eq!(parse_stat(1, "garbage", 4), None);
    }

    #[test]
    fn cpu_is_a_share_of_the_machine() {
        // 100 ticks a second; two cores' worth of a second on eight cores.
        assert_eq!(cpu_pct(0, 200, 100, 1_000, 8), 25);
        assert_eq!(cpu_pct(0, 99_999, 100, 1_000, 8), 100);
        // A total that fell (a process left the tree) is nothing, not a wrap.
        assert_eq!(cpu_pct(500, 100, 100, 1_000, 8), 0);
        assert_eq!(cpu_pct(0, 100, 100, 0, 8), 0);
    }

    #[test]
    fn figures_read_like_the_agents_pane() {
        assert_eq!(gb(0), "0.0G");
        assert_eq!(gb(1), "0.1G");
        assert_eq!(gb(4300), "4.2G");
        assert_eq!(gb(12 * 1024), "12G");
        assert_eq!(gb(1536 * 1024), "1.5T");
        let u = SessionUse {
            cpu_pct: 45,
            mem_mb: 4300,
            gpu_mb: None,
        };
        assert_eq!(segments(&u), ["45% CPU", "4.2G"]);
        let u = SessionUse {
            gpu_mb: Some(1229),
            ..u
        };
        assert_eq!(segments(&u), ["45% CPU", "4.2G", "gpu 1.2G"]);
    }

    #[test]
    fn nvidia_smi_compute_apps_are_summed_per_pid() {
        let m = gpu::parse_compute_apps("4242, 1200\n4242, 24\n77, 300\n[N/A], 5\n\n");
        assert_eq!(m.get(&4242), Some(&1224));
        assert_eq!(m.get(&77), Some(&300));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn the_use_sits_at_the_right_edge() {
        let left = "Opus 5.5  ·  session: 36.2k  ·  total: 90k";
        let right = "45% CPU  ·  4.2G";
        let line = align(left, right, Some(80));
        assert_eq!(line.chars().count(), 80, "`·` is one column: {line:?}");
        assert!(line.starts_with(left) && line.ends_with(right));
        // Unknown width: appended like any other segment.
        assert_eq!(align(left, right, None), format!("{left}{SEP}{right}"));
        // Too narrow: never glued on, the gap stays.
        let tight = align(left, right, Some(20));
        assert_eq!(tight, format!("{left}   {right}"));
        // Nothing to place: the line as it was.
        assert_eq!(align(left, "", Some(80)), left);
        assert_eq!(align("", right, Some(20)).chars().count(), 20);
    }

    #[test]
    fn the_width_is_the_terminals_less_claude_codes_margin() {
        assert_eq!(line_width(Some("120")), Some(116));
        assert_eq!(line_width(Some(" 80\n")), Some(76));
        assert_eq!(line_width(Some("4")), None);
        assert_eq!(line_width(Some("wide")), None);
        assert_eq!(line_width(None), None);
    }

    /// The real table: this test's own process is in it, with a parent.
    #[cfg(target_os = "linux")]
    #[test]
    fn this_process_is_in_its_parents_subtree() {
        let procs = proc_table(4);
        let me = std::process::id();
        let mine = procs.iter().find(|p| p.pid == me).expect("in /proc");
        let tree = subtree(&procs, mine.ppid);
        assert!(tree.contains(&me));
        assert!(sum(&procs, &tree).1 > 0, "some memory is resident");
    }

    #[test]
    fn every_session_is_counted_once() {
        // init → shell → claude A → bash → claude C (inside A)
        //      → tmux → claude B → cargo
        //      → claude D whose parent is gone
        let procs = [
            p(1, 0, 0, 0),
            p(10, 1, 0, 1_000),
            p(20, 10, 100, 300_000),
            p(21, 20, 10, 2_000),
            p(22, 21, 50, 200_000),
            p(30, 1, 0, 1_000),
            p(31, 30, 200, 400_000),
            p(32, 31, 600, 1_000_000),
            p(40, 999, 0, 100_000),
        ];
        let claude = |pid: u32| matches!(pid, 20 | 22 | 31 | 40);
        assert_eq!(session_roots(&procs, claude), [20, 31, 40]);
        let roots = session_roots(&procs, claude);
        // First reading: memory, no CPU yet.
        let (all, ticks) = sum_sessions(&procs, &roots, &HashMap::new(), 0, 100, 4, None);
        assert_eq!(all.sessions, 3);
        assert_eq!(all.total.cpu_pct, 0);
        assert_eq!(
            all.total.mem_mb,
            (502_000u64 + 1_400_000 + 100_000).div_ceil(1024)
        );
        assert_eq!(all.total.gpu_mb, None);
        assert_eq!(ticks[&20], 160);
        assert_eq!(ticks[&31], 800);
        // A second later: A spent one core-second, B two; D is new to
        // the reading after it (not in prev) and adds none.
        let prev = HashMap::from([(20, 60), (31, 600)]);
        let gpu = HashMap::from([(32, 1024), (22, 512), (777, 9999)]);
        let (all, _) = sum_sessions(&procs, &roots, &prev, 1_000, 100, 4, Some(&gpu));
        assert_eq!(all.total.cpu_pct, 25 + 50);
        assert_eq!(all.total.gpu_mb, Some(1536), "only the sessions' pids");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn all_sessions_reads_the_real_table() {
        let mut s = AllSessions::default();
        let first = s.sample().expect("a /proc");
        assert_eq!(first.total.cpu_pct, 0);
        assert!(s.sample().is_some());
    }
}

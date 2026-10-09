//! What a task's `giverny manage run` commands use right now.
//!
//! While a command runs, `run` keeps a small JSON file beside its stats
//! file, `<ledger dir>/runs/<pid>-<n>.live` (`task`, `session`, `pid`,
//! `stats`, `started_ms`), and removes it when the command ends. The shim
//! inside the command's systemd scope adds `cgroup <dir>` to the stats file
//! as it starts, so a reader finds the scope's cgroup without asking
//! systemd. The app's one sampler ([`crate::use_reading`]) lists them
//! ([`running`]) every couple of seconds and measures each scope's
//! processes (`cgroup.procs`) the way it measures everything else it
//! shows.
//!
//! A command run plain (no user systemd) has no cgroup of its own and is
//! not sampled. A `.live` file whose `run` is gone (killed) is skipped by
//! readers and swept by the next `run`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// A task's commands' use, summed over its running commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunLive {
    /// CPU over the last interval, percent of the whole machine (every
    /// core busy is 100).
    pub cpu_pct: u32,
    /// The memory its processes really use (proportional sets), MiB.
    pub mem_mb: u64,
    /// GPU memory of its processes, MiB; `None` when the machine
    /// has no NVIDIA GPU (or `nvidia-smi`'s answer is not in yet).
    pub gpu_mb: Option<u64>,
}

/// One task's use, keyed as the ledger keys leases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLive {
    pub session: String,
    pub task: String,
    pub live: RunLive,
    /// The worker whose commands the run is under, when one is: that
    /// worker's own figure already holds the run's.
    pub agent: Option<String>,
}

impl RunLive {
    /// Two disjoint sets of processes' use together.
    pub fn plus(self, other: RunLive) -> RunLive {
        RunLive {
            cpu_pct: (self.cpu_pct + other.cpu_pct).min(100),
            mem_mb: self.mem_mb + other.mem_mb,
            gpu_mb: match (self.gpu_mb, other.gpu_mb) {
                (Some(a), Some(b)) => Some(a + b),
                (a, b) => a.or(b),
            },
        }
    }
}

/// `<ledger dir>/runs`.
pub fn runs_dir(ledger: &Path) -> PathBuf {
    ledger.parent().unwrap_or(Path::new(".")).join("runs")
}

/// The `.live` file beside `stats` (`…/runs/4242-0.stats` →
/// `…/runs/4242-0.live`).
pub fn live_path(stats: &Path) -> PathBuf {
    stats.with_extension("live")
}

/// Removes its `.live` file when dropped.
pub struct Registered(PathBuf);

impl Drop for Registered {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Say that `task`'s command, measured through `stats`, is running. Sweeps
/// `.live` files whose `run` is gone first. A failure only costs the pane
/// its live figure, so it is `None`, not an error.
pub fn register(stats: &Path, task: &str, session: &str, now_ms: u64) -> Option<Registered> {
    if let Some(dir) = stats.parent() {
        sweep(dir);
    }
    let path = live_path(stats);
    let body = json!({
        "task": task,
        "session": session,
        "pid": std::process::id(),
        "stats": stats.display().to_string(),
        "started_ms": now_ms,
    });
    let tmp = path.with_extension("live.tmp");
    std::fs::write(&tmp, body.to_string()).ok()?;
    std::fs::rename(&tmp, &path).ok()?;
    Some(Registered(path))
}

/// Remove `.live` files whose `run` process is gone.
fn sweep(dir: &Path) {
    for (path, l) in read_dir(dir) {
        if !alive(l.pid) {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[derive(Debug, Clone)]
struct LiveFile {
    task: String,
    session: String,
    pid: u32,
    stats: PathBuf,
}

fn parse_live(bytes: &[u8]) -> Option<LiveFile> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(LiveFile {
        task: s("task")?,
        session: s("session")?,
        pid: v.get("pid").and_then(Value::as_u64)? as u32,
        stats: PathBuf::from(s("stats")?),
    })
}

fn read_dir(dir: &Path) -> Vec<(PathBuf, LiveFile)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "live"))
        .filter_map(|p| {
            let l = parse_live(&std::fs::read(&p).ok()?)?;
            Some((p, l))
        })
        .collect()
}

/// The process is there (Linux: `/proc/<pid>`; elsewhere assumed).
fn alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        Path::new(&format!("/proc/{pid}")).exists()
    } else {
        true
    }
}

/// The `cgroup <dir>` line the shim wrote into a stats file.
pub fn cgroup_of(stats_text: &str) -> Option<PathBuf> {
    stats_text
        .lines()
        .find_map(|l| l.strip_prefix("cgroup "))
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
}

/// The processes in a cgroup (`cgroup.procs`).
pub fn cgroup_pids(dir: &Path) -> HashSet<u32> {
    std::fs::read_to_string(dir.join("cgroup.procs"))
        .map(|t| t.lines().filter_map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_default()
}

/// Every running command under `runs_dir` with a scope of its own:
/// `(session, task, its cgroup)`. A `.live` file whose `run` is gone, or
/// whose command runs plain (no cgroup), is left out.
pub fn running(runs_dir: &Path) -> Vec<(String, String, PathBuf)> {
    read_dir(runs_dir)
        .into_iter()
        .filter(|(_, l)| alive(l.pid))
        .filter_map(|(_, l)| {
            let cg = std::fs::read_to_string(&l.stats)
                .ok()
                .and_then(|t| cgroup_of(&t))?;
            Some((l.session, l.task, cg))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("giverny-run-live-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_shims_cgroup_line_is_read() {
        assert_eq!(
            cgroup_of("started\ncgroup /sys/fs/cgroup/a/b.scope\n"),
            Some(PathBuf::from("/sys/fs/cgroup/a/b.scope"))
        );
        assert_eq!(cgroup_of("started\n"), None);
    }

    /// The pids whose GPU memory a run's cell sums.
    #[test]
    fn a_cgroups_processes_are_read() {
        let cg = scratch("procs");
        assert!(cgroup_pids(&cg).is_empty(), "no file, no processes");
        std::fs::write(cg.join("cgroup.procs"), "4242\n77\n\n").unwrap();
        assert_eq!(cgroup_pids(&cg), HashSet::from([4242, 77]));
        let _ = std::fs::remove_dir_all(&cg);
    }

    /// A running command is listed with its cgroup, a dead run's file is
    /// not, and `register`'s guard removes its file.
    #[test]
    fn a_running_command_is_listed_and_its_file_goes_with_it() {
        let d = scratch("running");
        let runs = d.join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        let cg = d.join("cg");
        let stats = runs.join("1-0.stats");
        std::fs::write(&stats, format!("started\ncgroup {}\n", cg.display())).unwrap();
        let reg = register(&stats, "demo#182", "s1", 1_000).expect("registered");
        assert!(live_path(&stats).exists());
        std::fs::write(
            runs.join("2-0.live"),
            json!({"task":"x","session":"s1","pid":u32::MAX - 1,"stats":stats.display().to_string()})
                .to_string(),
        )
        .unwrap();
        assert_eq!(
            running(&runs),
            [("s1".to_string(), "demo#182".to_string(), cg)]
        );
        drop(reg);
        assert!(!live_path(&stats).exists());
        assert!(running(&runs).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}

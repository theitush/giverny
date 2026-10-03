//! What a task's `giverny pass run` commands use right now (giverny#182).
//!
//! While a command runs, `run` keeps a small JSON file beside its stats
//! file, `<ledger dir>/runs/<pid>-<n>.live` (`task`, `session`, `pid`,
//! `stats`, `started_ms`), and removes it when the command ends. The shim
//! inside the command's systemd scope adds `cgroup <dir>` to the stats file
//! as it starts, so a reader finds the scope's cgroup without asking
//! systemd. The agents pane reads both every couple of seconds ([`Sampler`]):
//! the cgroup's `memory.current`, and `cpu.stat`'s `usage_usec` between two
//! reads for CPU, as a share of the machine's cores.
//!
//! A command run plain (no user systemd) has no cgroup of its own and is
//! not sampled. A `.live` file whose `run` is gone (killed) is skipped by
//! readers and swept by the next `run`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};

/// A task's commands' use, summed over its running commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunLive {
    /// CPU over the last interval, percent of the whole machine (every
    /// core busy is 100).
    pub cpu_pct: u32,
    /// The cgroups' `memory.current`, MiB.
    pub mem_mb: u64,
}

/// One task's use, keyed as the ledger keys leases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLive {
    pub session: String,
    pub task: String,
    pub live: RunLive,
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
    started_ms: u64,
}

fn parse_live(bytes: &[u8]) -> Option<LiveFile> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(LiveFile {
        task: s("task")?,
        session: s("session")?,
        pid: v.get("pid").and_then(Value::as_u64)? as u32,
        stats: PathBuf::from(s("stats")?),
        started_ms: v.get("started_ms").and_then(Value::as_u64).unwrap_or(0),
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

/// `memory.current` (bytes) and `cpu.stat`'s `usage_usec` of a cgroup.
pub fn read_cgroup(dir: &Path) -> Option<(u64, u64)> {
    let mem = std::fs::read_to_string(dir.join("memory.current"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let cpu = std::fs::read_to_string(dir.join("cpu.stat"))
        .ok()
        .and_then(|t| usage_usec(&t))
        .unwrap_or(0);
    Some((mem, cpu))
}

pub fn usage_usec(cpu_stat: &str) -> Option<u64> {
    cpu_stat
        .lines()
        .find_map(|l| l.strip_prefix("usage_usec "))
        .and_then(|n| n.trim().parse().ok())
}

/// CPU used between two reads, percent of `cores` cores, rounded.
pub fn cpu_pct(used_usec: u64, over_usec: u64, cores: u32) -> u32 {
    if over_usec == 0 || cores == 0 {
        return 0;
    }
    let p = used_usec as f64 * 100.0 / (over_usec as f64 * cores as f64);
    p.round().clamp(0.0, 100.0) as u32
}

/// Reads every running command's cgroup, remembering the last read of each
/// so CPU is the use since then. One read is a handful of small files per
/// command: cheap enough every couple of seconds, not every frame.
#[derive(Debug)]
pub struct Sampler {
    cores: u32,
    prev: HashMap<PathBuf, (u64, Instant)>,
}

impl Default for Sampler {
    fn default() -> Self {
        Sampler {
            cores: std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(1),
            prev: HashMap::new(),
        }
    }
}

impl Sampler {
    /// Every task's use now, from the `.live` files under `runs_dir`.
    /// `now_ms` (wall clock) gives a command seen for the first time its
    /// average since it started.
    pub fn sample(&mut self, runs_dir: &Path, now_ms: u64) -> Vec<TaskLive> {
        let now = Instant::now();
        let mut seen = HashMap::new();
        let mut out: Vec<TaskLive> = Vec::new();
        for (_, l) in read_dir(runs_dir) {
            if !alive(l.pid) {
                continue;
            }
            let Some(cg) = std::fs::read_to_string(&l.stats)
                .ok()
                .and_then(|t| cgroup_of(&t))
            else {
                continue;
            };
            let Some((mem, usec)) = read_cgroup(&cg) else {
                continue;
            };
            let pct = match self.prev.get(&cg) {
                Some(&(u, at)) => cpu_pct(
                    usec.saturating_sub(u),
                    now.duration_since(at).as_micros() as u64,
                    self.cores,
                ),
                None => cpu_pct(
                    usec,
                    now_ms.saturating_sub(l.started_ms).saturating_mul(1000),
                    self.cores,
                ),
            };
            seen.insert(cg, (usec, now));
            let mem_mb = mem.div_ceil(1024 * 1024);
            match out
                .iter_mut()
                .find(|t| t.task == l.task && t.session == l.session)
            {
                Some(t) => {
                    t.live.cpu_pct = (t.live.cpu_pct + pct).min(100);
                    t.live.mem_mb += mem_mb;
                }
                None => out.push(TaskLive {
                    session: l.session,
                    task: l.task,
                    live: RunLive {
                        cpu_pct: pct,
                        mem_mb,
                    },
                }),
            }
        }
        self.prev = seen;
        out.sort_by(|a, b| (&a.session, &a.task).cmp(&(&b.session, &b.task)));
        out
    }
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
    fn the_shims_cgroup_line_and_cpu_stat_are_read() {
        assert_eq!(
            cgroup_of("started\ncgroup /sys/fs/cgroup/a/b.scope\n"),
            Some(PathBuf::from("/sys/fs/cgroup/a/b.scope"))
        );
        assert_eq!(cgroup_of("started\n"), None);
        assert_eq!(usage_usec("usage_usec 4200\nuser_usec 4000\n"), Some(4200));
        // One core busy for a second on four: a quarter of the machine.
        assert_eq!(cpu_pct(1_000_000, 1_000_000, 4), 25);
        assert_eq!(cpu_pct(9_000_000, 1_000_000, 4), 100);
        assert_eq!(cpu_pct(5, 0, 4), 0);
    }

    /// A fake cgroup: the sampler sums a task's commands, keys them by
    /// session and task, takes CPU between reads, and skips a `.live` file
    /// whose `run` is gone; `register`'s guard removes its file.
    #[test]
    fn a_running_command_is_sampled_and_its_file_goes_with_it() {
        let d = scratch("sample");
        let runs = d.join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        let cg = d.join("cg");
        std::fs::create_dir_all(&cg).unwrap();
        std::fs::write(cg.join("memory.current"), (512u64 << 20).to_string()).unwrap();
        std::fs::write(cg.join("cpu.stat"), "usage_usec 0\n").unwrap();
        let stats = runs.join("1-0.stats");
        std::fs::write(&stats, format!("started\ncgroup {}\n", cg.display())).unwrap();
        let reg = register(&stats, "giverny#182", "s1", 1_000).expect("registered");
        assert!(live_path(&stats).exists());
        // A dead run's file is skipped.
        std::fs::write(
            runs.join("2-0.live"),
            json!({"task":"x","session":"s1","pid":u32::MAX - 1,"stats":stats.display().to_string()})
                .to_string(),
        )
        .unwrap();
        let mut s = Sampler {
            cores: 2,
            prev: HashMap::new(),
        };
        let got = s.sample(&runs, 2_000);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].task, "giverny#182");
        assert_eq!(got[0].session, "s1");
        assert_eq!(got[0].live.mem_mb, 512);
        std::fs::write(cg.join("cpu.stat"), "usage_usec 99999999999\n").unwrap();
        let got = s.sample(&runs, 3_000);
        assert_eq!(got[0].live.cpu_pct, 100);
        drop(reg);
        assert!(!live_path(&stats).exists());
        assert!(s.sample(&runs, 4_000).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}

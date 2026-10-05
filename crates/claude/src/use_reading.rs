//! One reading of what Giverny runs, at one moment, measured one way.
//!
//! The app reads `/proc` once a second ([`Sampler`]) and
//! derives every figure it shows from that one pass:
//!
//! * the **total**: the app and everything under it, every tab, Claude or
//!   not (the sidebar's line);
//! * each **session**: a claude process and everything under it, keyed by
//!   the claude's pid (its status line);
//! * each **run**: the processes in a `giverny pass run` scope's cgroup
//!   (the agents pane's rows). A scope's command is a descendant of the
//!   claude that ran it (`systemd-run --scope` execs it in place), so a run
//!   is part of its session, and a session part of the total.
//!
//! Every group is a set of processes, and every figure is a sum over its
//! processes of the same per-process figures from the same pass: CPU
//! clock ticks spent since the last pass ([`pid_ticks`]), as a share of
//! the machine; memory as the proportional set (shared pages split
//! between their sharers); GPU memory and compute per process from
//! `nvidia-smi`. So a run never reads more than its session, nor a session
//! more than the total.
//!
//! The reading is also written to a small file beside the app's socket
//! ([`snapshot_path`]), where `giverny statusline` — a process of its own,
//! run by Claude Code — finds its session's figure ([`session_now`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::session_use::{Proc, SessionUse, subtree};

/// One group's use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Use {
    /// CPU since the last pass, percent of the whole machine.
    pub cpu_pct: u32,
    /// Proportional set summed, MiB.
    pub mem_mb: u64,
    /// GPU memory, MiB; `None` without an NVIDIA GPU.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_mb: Option<u64>,
    /// Share of the GPUs' compute, percent; `None` without a GPU that
    /// reports it per process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_pct: Option<u32>,
}

impl Use {
    /// As the status line shows it.
    pub fn session(&self) -> SessionUse {
        SessionUse {
            cpu_pct: self.cpu_pct,
            mem_mb: self.mem_mb,
            gpu_mb: self.gpu_mb,
        }
    }
}

/// One `giverny pass run` task's commands' use (its scopes summed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunUse {
    pub session: String,
    pub task: String,
    #[serde(rename = "use")]
    pub used: Use,
    /// The worker whose commands the run's processes are under, when one
    /// is ([`crate::worker_pids`]): that worker's figure already holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// One pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reading {
    /// When it was taken, ms since the epoch.
    pub at_ms: u64,
    /// Which pass of the app's sampler it is (the first is 1): two figures
    /// with the same number are from the same moment.
    #[serde(default)]
    pub seq: u64,
    /// The app's pid, the root of [`Reading::total`].
    pub app: u32,
    pub total: Use,
    /// Every claude under the app, by pid.
    pub sessions: BTreeMap<u32, Use>,
    pub runs: Vec<RunUse>,
    /// Each worker's processes (the Bash commands it started, with all
    /// under them), by agent id; part of its session's.
    #[serde(default)]
    pub agents: BTreeMap<String, Use>,
}

/// What one pass knows about every process, and the GPU.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub procs: Vec<Proc>,
    /// CPU ticks each process spent since the last pass ([`pid_ticks`]).
    pub spent: HashMap<u32, u64>,
    /// GPU memory per pid, MiB; `None` without a GPU.
    pub gpu_mb: Option<HashMap<u32, u64>>,
    /// `pmon`'s compute per pid; `None` where it is not reported.
    pub pmon: Option<Pmon>,
    /// The span the ticks were spent over, ms, and the machine's clock
    /// rate and cores, to make a share of them.
    pub over_ms: u64,
    pub ticks_per_s: u64,
    pub cores: u32,
}

impl Table {
    /// The use of the processes in `pids`.
    pub fn use_of(&self, pids: &HashSet<u32>) -> Use {
        let ticks: u64 = pids.iter().filter_map(|p| self.spent.get(p)).sum();
        let mem_kb: u64 = self
            .procs
            .iter()
            .filter(|p| pids.contains(&p.pid))
            .map(|p| p.mem_kb)
            .sum();
        Use {
            cpu_pct: crate::session_use::cpu_pct(
                0,
                ticks,
                self.ticks_per_s,
                self.over_ms,
                self.cores,
            ),
            mem_mb: mem_kb.div_ceil(1024),
            gpu_mb: self
                .gpu_mb
                .as_ref()
                .map(|g| crate::session_use::gpu::of(g, pids)),
            gpu_pct: self
                .pmon
                .as_ref()
                .filter(|p| p.gpus > 0)
                .map(|p| gpu_pct(p, pids)),
        }
    }

    /// The reading: `app`'s tree, each claude in it (`is_claude`), each
    /// run's processes (`runs`: session, task, its scope's pids; several
    /// scopes of one task are summed), and each worker's (`agents`).
    pub fn reading(
        &self,
        at_ms: u64,
        app: u32,
        is_claude: impl Fn(u32) -> bool,
        runs: &[(String, String, HashSet<u32>)],
        agents: &HashMap<String, HashSet<u32>>,
    ) -> Reading {
        let tree = subtree(&self.procs, app);
        let sessions = tree
            .iter()
            .copied()
            .filter(|&p| is_claude(p))
            .map(|p| (p, self.use_of(&subtree(&self.procs, p))))
            .collect();
        let mut by_task: BTreeMap<(String, String), HashSet<u32>> = BTreeMap::new();
        for (session, task, pids) in runs {
            by_task
                .entry((session.clone(), task.clone()))
                .or_default()
                .extend(pids);
        }
        let runs = by_task
            .into_iter()
            .map(|((session, task), pids)| RunUse {
                agent: agents
                    .iter()
                    .find(|(_, a)| !a.is_disjoint(&pids))
                    .map(|(id, _)| id.clone()),
                session,
                task,
                used: self.use_of(&pids),
            })
            .collect();
        Reading {
            at_ms,
            seq: 0,
            app,
            total: self.use_of(&tree),
            sessions,
            runs,
            agents: agents
                .iter()
                .map(|(id, pids)| (id.clone(), self.use_of(pids)))
                .collect(),
        }
    }
}

/// What a process was at the last pass: its parent, start and ticks.
pub type Seen = HashMap<u32, (u32, u64, u64)>;

/// Each live process's CPU ticks since the last pass (`prev`), from
/// [`Proc::ticks`] — its own time and its reaped children's.
///
/// A process seen last time (same pid, same start) spent the difference.
/// One new since spent all it has (with no last pass at all, nothing is
/// known yet: zero). A process that ended since had its time, up to the
/// last pass already counted, added into whichever process reaped it: that
/// much is taken back off the nearest of its last-pass ancestors still
/// alive, so only its last stretch is counted, once. Never below zero.
pub fn pid_ticks(procs: &[Proc], prev: Option<&Seen>) -> HashMap<u32, u64> {
    let Some(prev) = prev else {
        return procs.iter().map(|p| (p.pid, 0)).collect();
    };
    let alive: HashMap<u32, u64> = procs.iter().map(|p| (p.pid, p.start)).collect();
    let same = |pid: u32, start: u64| alive.get(&pid) == Some(&start);
    let mut back: HashMap<u32, u64> = HashMap::new();
    for (&pid, &(ppid, start, ticks)) in prev {
        if same(pid, start) {
            continue;
        }
        let mut up = ppid;
        for _ in 0..64 {
            match prev.get(&up) {
                _ if alive.contains_key(&up) => {
                    *back.entry(up).or_default() += ticks;
                    break;
                }
                Some(&(pp, _, _)) if pp != up => up = pp,
                _ => break,
            }
        }
    }
    procs
        .iter()
        .map(|p| {
            let spent = match prev.get(&p.pid) {
                Some(&(_, start, then)) if start == p.start => p.ticks.saturating_sub(then),
                _ => p.ticks,
            };
            (
                p.pid,
                spent.saturating_sub(back.get(&p.pid).copied().unwrap_or(0)),
            )
        })
        .collect()
}

/// This pass's processes, as the next pass needs them.
pub fn seen(procs: &[Proc]) -> Seen {
    procs
        .iter()
        .map(|p| (p.pid, (p.ppid, p.start, p.ticks)))
        .collect()
}

/// One `nvidia-smi pmon -c 1 -s u` reading: each process's `sm` percent
/// (summed over GPUs), and how many GPUs answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pmon {
    pub sm: HashMap<u32, u32>,
    pub gpus: usize,
}

/// `pmon`'s table: `# …` headers, then `<gpu> <pid> <type> <sm> <mem> …`
/// rows, `-` for nothing (a GPU with no process has a row of `-`).
pub fn parse_pmon(out: &str) -> Pmon {
    let mut gpus = HashSet::new();
    let mut sm: HashMap<u32, u32> = HashMap::new();
    for l in out.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = l.split_whitespace().collect();
        let Some(gpu) = f.first().and_then(|g| g.parse::<u32>().ok()) else {
            continue;
        };
        gpus.insert(gpu);
        if let (Some(pid), Some(pct)) = (
            f.get(1).and_then(|p| p.parse::<u32>().ok()),
            f.get(3).and_then(|p| p.parse::<u32>().ok()),
        ) {
            *sm.entry(pid).or_default() += pct;
        }
    }
    Pmon {
        sm,
        gpus: gpus.len(),
    }
}

/// The share of all the GPUs' compute the processes in `pids` use,
/// percent, at most 100.
pub fn gpu_pct(pmon: &Pmon, pids: &HashSet<u32>) -> u32 {
    if pmon.gpus == 0 {
        return 0;
    }
    let used: u32 = pmon
        .sm
        .iter()
        .filter(|(pid, _)| pids.contains(pid))
        .map(|(_, pct)| pct)
        .sum();
    (used / pmon.gpus as u32).min(100)
}

// ---- the snapshot file ----------------------------------------------------

/// Where the app leaves its last reading: beside its socket, so a status
/// line run in one of its tabs (which inherits the same runtime dir)
/// finds that app's, and a test instance's stays its own.
pub fn snapshot_path() -> PathBuf {
    crate::hooks::socket_path().with_extension("use.json")
}

/// A reading older than this is not about now: the app samples every
/// second, so one tick late is still the last; two, it has stopped.
pub const FRESH_MS: u64 = 2_500;

/// Write `r` to `path` whole (a reader never sees half of it).
pub fn write_snapshot(path: &Path, r: &Reading) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(r)?)?;
    std::fs::rename(&tmp, path)
}

/// The figure for the session whose claude is `claude`, from the snapshot
/// at `path`, if it is fresh at `now_ms` and has that session: else
/// `None`, and the caller measures for itself (no Giverny running, or a
/// claude outside its tabs).
pub fn session_from(path: &Path, claude: u32, now_ms: u64) -> Option<Use> {
    let r: Reading = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if now_ms.saturating_sub(r.at_ms) > FRESH_MS || r.at_ms > now_ms + FRESH_MS {
        return None;
    }
    r.sessions.get(&claude).copied()
}

/// The status line's session figure from the running app's last reading.
#[cfg(target_os = "linux")]
pub fn session_now() -> Option<Use> {
    let claude = crate::session_use::claude_root(std::process::id())?;
    session_from(&snapshot_path(), claude, crate::session_use::now_ms())
}

#[cfg(not(target_os = "linux"))]
pub fn session_now() -> Option<Use> {
    None
}

// ---- the sampler ----------------------------------------------------------

/// Takes the passes, keeping what the next one diffs against. GPU answers
/// are slow: `nvidia-smi`'s memory per process is asked every
/// [`Sampler::GPU_EVERY`], its compute (`pmon`, which watches for a second)
/// started then and collected on a later pass, never waited for.
#[derive(Debug, Default)]
pub struct Sampler {
    prev: Option<(Seen, std::time::Instant)>,
    gpu: Option<(HashMap<u32, u64>, std::time::Instant)>,
    pmon: Option<Pmon>,
    pmon_child: Option<std::process::Child>,
    pmon_at: Option<std::time::Instant>,
    workers: crate::worker_pids::Attributor,
    seq: u64,
}

impl Sampler {
    #[cfg(target_os = "linux")]
    const GPU_EVERY: std::time::Duration = std::time::Duration::from_secs(6);

    /// One pass for the app `app`, with the runs under `runs_dir`. `None`
    /// where there is no `/proc`. The first pass has no CPU yet.
    #[cfg(target_os = "linux")]
    pub fn sample(&mut self, app: u32, runs_dir: &Path) -> Option<Reading> {
        use crate::session_use as su;
        let ticks_per_s = su::sysconf(libc::_SC_CLK_TCK).unwrap_or(100);
        let page_kb = su::sysconf(libc::_SC_PAGESIZE).unwrap_or(4096) / 1024;
        let cores = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1);
        let mut procs = su::proc_table(page_kb);
        if procs.is_empty() {
            return None;
        }
        let now = std::time::Instant::now();
        let runs: Vec<(String, String, HashSet<u32>)> = crate::run_live::running(runs_dir)
            .into_iter()
            .map(|(session, task, cg)| (session, task, crate::run_live::cgroup_pids(&cg)))
            .collect();
        // Memory is read for what is shown: the app's tree and the runs.
        let mut shown = subtree(&procs, app);
        shown.extend(runs.iter().flat_map(|r| r.2.iter().copied()));
        su::with_real_memory(&mut procs, &shown);
        let spent = pid_ticks(&procs, self.prev.as_ref().map(|p| &p.0));
        let over_ms = self
            .prev
            .as_ref()
            .map_or(0, |(_, at)| now.duration_since(*at).as_millis() as u64);
        self.poll_gpu(now);
        let table = Table {
            spent,
            gpu_mb: self.gpu.as_ref().map(|g| g.0.clone()),
            pmon: self.pmon.clone(),
            over_ms,
            ticks_per_s,
            cores,
            procs,
        };
        self.prev = Some((seen(&table.procs), now));
        let claudes: HashSet<u32> = shown
            .iter()
            .copied()
            .filter(|&p| crate::lineage::proc_is_claude(p))
            .collect();
        let agents = self
            .workers
            .attribute(&table.procs, &claudes, &crate::worker_pids::Machine);
        self.seq += 1;
        let mut r = table.reading(su::now_ms(), app, |p| claudes.contains(&p), &runs, &agents);
        r.seq = self.seq;
        Some(r)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn sample(&mut self, _app: u32, _runs_dir: &Path) -> Option<Reading> {
        None
    }

    /// Ask `nvidia-smi` again when due; collect a finished `pmon`.
    #[cfg(target_os = "linux")]
    fn poll_gpu(&mut self, now: std::time::Instant) {
        use std::io::Read;
        use std::process::{Command, Stdio};
        let Some(smi) = crate::session_use::gpu::find_on_path("nvidia-smi") else {
            return;
        };
        if let Some(child) = &mut self.pmon_child
            && let Ok(Some(status)) = child.try_wait()
        {
            let mut out = String::new();
            if let Some(so) = child.stdout.as_mut() {
                let _ = so.read_to_string(&mut out);
            }
            // Unsupported here (some drivers, WSL): no figure.
            self.pmon = status.success().then(|| parse_pmon(&out));
            self.pmon_child = None;
        }
        if self
            .pmon_at
            .is_some_and(|at| now.duration_since(at) < Self::GPU_EVERY)
        {
            return;
        }
        self.pmon_at = Some(now);
        self.gpu = Command::new(&smi)
            .args([
                "--query-compute-apps=pid,used_memory",
                "--format=csv,noheader,nounits",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                (
                    crate::session_use::gpu::parse_compute_apps(&String::from_utf8_lossy(
                        &o.stdout,
                    )),
                    now,
                )
            });
        if self.pmon_child.is_none() {
            self.pmon_child = Command::new(&smi)
                .args(["pmon", "-c", "1", "-s", "u"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, ticks: u64, mem_kb: u64) -> Proc {
        Proc {
            pid,
            ppid,
            ticks,
            mem_kb,
            start: pid as u64,
        }
    }

    /// app → shell → claude A → bash → `pass run` scope (cargo → rustc),
    /// app → shell → claude B; and a process outside the app.
    fn machine() -> Vec<Proc> {
        vec![
            p(1, 0, 0, 0),
            p(100, 1, 300, 200_000),
            p(110, 100, 10, 4_000),
            p(120, 110, 500, 300_000),
            p(130, 120, 20, 3_000),
            p(140, 130, 50, 50_000),
            p(141, 140, 900, 900_000),
            p(210, 100, 5, 4_000),
            p(220, 210, 100, 250_000),
            p(900, 1, 4_000, 2_000_000),
        ]
    }

    fn table(procs: Vec<Proc>, prev: Option<&Seen>, gpu: Option<HashMap<u32, u64>>) -> Table {
        Table {
            spent: pid_ticks(&procs, prev),
            procs,
            gpu_mb: gpu,
            pmon: None,
            over_ms: 1_000,
            ticks_per_s: 100,
            cores: 4,
        }
    }

    #[test]
    fn a_run_is_within_its_session_and_a_session_within_the_total() {
        let before = machine();
        let mut after = machine();
        for q in &mut after {
            q.ticks += match q.pid {
                141 => 150, // the build: a core and a half
                120 => 40,
                220 => 100,
                900 => 400, // outside: never counted
                _ => 5,
            };
        }
        let gpu = HashMap::from([(141, 2048), (220, 512), (900, 4096)]);
        let t = table(after, Some(&seen(&before)), Some(gpu));
        let runs = vec![("s1".into(), "demo#1".into(), HashSet::from([140, 141]))];
        let r = t.reading(42, 100, |p| p == 120 || p == 220, &runs, &HashMap::new());
        let a = r.sessions[&120];
        let b = r.sessions[&220];
        let run = r.runs[0].used;
        assert_eq!(r.runs[0].task, "demo#1");
        // CPU: 150+5 of 400 ticks a second for the run, A adds 40+5+5 more.
        assert_eq!(run.cpu_pct, 39);
        assert_eq!(a.cpu_pct, 50);
        assert_eq!(b.cpu_pct, 25);
        assert_eq!(r.total.cpu_pct, 79, "app, both shells, A and B");
        assert_eq!(run.mem_mb, 950_000u64.div_ceil(1024));
        assert_eq!(a.mem_mb, 1_253_000u64.div_ceil(1024));
        assert_eq!(r.total.mem_mb, 1_711_000u64.div_ceil(1024));
        assert_eq!(
            (run.gpu_mb, a.gpu_mb, r.total.gpu_mb),
            (Some(2048), Some(2048), Some(2560))
        );
        for (part, whole) in [(run, a), (a, r.total), (b, r.total)] {
            assert!(part.cpu_pct <= whole.cpu_pct, "{part:?} {whole:?}");
            assert!(part.mem_mb <= whole.mem_mb, "{part:?} {whole:?}");
        }
        assert!(!r.sessions.contains_key(&900));
    }

    /// A worker's commands: within its session, and a `pass run` it
    /// started within it (named as its, so a row adds it once).
    #[test]
    fn a_worker_is_within_its_session_and_its_run_within_it() {
        let before = machine();
        let mut after = machine();
        for q in &mut after {
            q.ticks += if q.pid == 141 { 150 } else { 5 };
        }
        let t = table(after, Some(&seen(&before)), None);
        // The worker started bash 130 (cargo 140 → rustc 141 under it).
        let agents = HashMap::from([("a1".to_string(), HashSet::from([130, 140, 141]))]);
        let runs = vec![("s1".into(), "demo#1".into(), HashSet::from([140, 141]))];
        let r = t.reading(42, 100, |p| p == 120, &runs, &agents);
        let (run, worker, session) = (r.runs[0].used, r.agents["a1"], r.sessions[&120]);
        assert_eq!(r.runs[0].agent.as_deref(), Some("a1"));
        assert_eq!((run.cpu_pct, worker.cpu_pct, session.cpu_pct), (39, 40, 41));
        for (part, whole) in [(run, worker), (worker, session), (session, r.total)] {
            assert!(part.cpu_pct <= whole.cpu_pct, "{part:?} {whole:?}");
            assert!(part.mem_mb <= whole.mem_mb, "{part:?} {whole:?}");
        }
    }

    #[test]
    fn the_first_pass_has_no_cpu() {
        let t = table(machine(), None, None);
        let r = t.reading(1, 100, |p| p == 120, &[], &HashMap::new());
        assert_eq!(r.total.cpu_pct, 0);
        assert!(r.total.mem_mb > 0);
        assert_eq!(r.total.gpu_mb, None);
    }

    #[test]
    fn a_process_that_ended_is_counted_once() {
        // 141 (rustc) ran 900 ticks by the last pass, then 100 more and
        // ended; cargo (140) reaped it, its ticks now hold 1000 of rustc's.
        let before = machine();
        let after: Vec<Proc> = machine()
            .into_iter()
            .filter(|q| q.pid != 141)
            .map(|mut q| {
                if q.pid == 140 {
                    q.ticks += 1_000 + 10;
                }
                q
            })
            .collect();
        let spent = pid_ticks(&after, Some(&seen(&before)));
        assert_eq!(spent[&140], 110, "its own 10 and rustc's last 100");
        // A new process spent all it has; a reused pid is new.
        let mut again = machine();
        again.push(p(150, 130, 30, 1));
        again[9].start = 7;
        let spent = pid_ticks(&again, Some(&seen(&before)));
        assert_eq!(spent[&150], 30);
        assert_eq!(spent[&900], 4_000);
        // A total that fell (time left with a process) is nothing.
        let mut fell = machine();
        fell[1].ticks = 0;
        assert_eq!(pid_ticks(&fell, Some(&seen(&before)))[&100], 0);
    }

    #[test]
    fn gpu_utilisation_is_the_share_of_every_gpu() {
        let out = "# gpu        pid  type    sm   mem   enc   dec   command\n\
                   # Idx          #   C/G     %     %     %     %   name\n\
                   \x20   0      4242     C    60    10     -     -   python\n\
                   \x20   0      5000     C    30     5     -     -   other\n\
                   \x20   1      4243     C    20     1     -     -   python\n\
                   \x20   1      6000     G     -     -     -     -   Xorg\n";
        let p = parse_pmon(out);
        assert_eq!(p.gpus, 2);
        assert_eq!(p.sm.get(&6000), None, "`-` is no figure");
        assert_eq!(gpu_pct(&p, &HashSet::from([4242, 4243])), 40);
        let idle = parse_pmon("    0          -     -     -     -     -     -   -\n");
        assert_eq!((idle.gpus, idle.sm.len()), (1, 0));
        assert_eq!(gpu_pct(&Pmon::default(), &HashSet::from([1])), 0);
    }

    #[test]
    fn the_status_line_takes_a_fresh_snapshot_or_measures_itself() {
        let dir = std::env::temp_dir().join(format!("giverny-use-reading-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("giverny.use.json");
        let t = table(machine(), None, None);
        let r = t.reading(10_000, 100, |p| p == 120, &[], &HashMap::new());
        write_snapshot(&path, &r).unwrap();
        let a = r.sessions[&120];
        assert_eq!(session_from(&path, 120, 10_500), Some(a));
        assert_eq!(session_from(&path, 120, 10_000 + FRESH_MS), Some(a));
        assert_eq!(session_from(&path, 120, 10_001 + FRESH_MS), None, "stale");
        assert_eq!(
            session_from(&path, 999, 10_500),
            None,
            "not one of its sessions"
        );
        assert_eq!(
            session_from(&dir.join("none"), 120, 10_500),
            None,
            "no Giverny"
        );
        std::fs::write(&path, "{half").unwrap();
        assert_eq!(session_from(&path, 120, 10_500), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real pass over this machine: the test process's own tree.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_real_pass_reads_this_process() {
        let mut s = Sampler::default();
        let me = std::process::id();
        let none = std::env::temp_dir().join("giverny-no-runs-here");
        let first = s.sample(me, &none).expect("a /proc");
        assert_eq!(first.total.cpu_pct, 0);
        assert!(first.total.mem_mb > 0);
        // Each pass numbered, so two figures can be told to be one moment's.
        assert_eq!(first.seq, 1);
        assert_eq!(s.sample(me, &none).map(|r| r.seq), Some(2));
    }
}

/// `GIVERNY_USE_DUMP=<app pid> cargo test -p giverny-claude -- --ignored
/// dump_a_live_tree --nocapture`: take passes over a running app's tree,
/// read-only, and print what each costs and what it credits.
#[cfg(all(test, target_os = "linux"))]
mod dump {
    #[test]
    #[ignore]
    fn dump_a_live_tree() {
        let Some(app) = std::env::var("GIVERNY_USE_DUMP")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
        else {
            return;
        };
        let runs =
            crate::run_live::runs_dir(&crate::resources::ledger_path(&crate::feed::feed_dir()));
        let mut s = super::Sampler::default();
        for pass in 0..6 {
            let t = std::time::Instant::now();
            let r = s.sample(app, &runs).expect("a /proc");
            let took = t.elapsed();
            let procs = crate::session_use::subtree(&crate::session_use::proc_table(4), app).len();
            println!(
                "pass {pass}: {:.1} ms over {procs} processes\n{}",
                took.as_secs_f64() * 1000.0,
                serde_json::to_string_pretty(&r).unwrap()
            );
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}

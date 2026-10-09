//! Every claude in a Giverny tab, held by the kernel inside the machine's
//! budget, which it cannot forget or opt out of (giverny#262).
//!
//! A tab is a plain shell: Giverny cannot know at spawn which tab will run
//! Claude Code, and a shell tab must stay as it is. So the claude itself is
//! taken, as soon as the app's sampler sees it under a tab (its pass is
//! once a second): the claude, and whatever it has started by then, is
//! moved into a transient systemd scope of its own,
//! `giverny-claude-<pid>.scope`, inside one slice, [`SLICE`]. Everything it
//! starts after that — its subagents (in-process), every Bash command,
//! every build — is born in that scope, inside the slice.
//!
//! - **The slice is the one ceiling**: hard `CPUQuota` and `MemoryMax`
//!   (with no swap) at `[manager.limits]` (cores − 2 and 70 % of RAM unless
//!   set), for all Claude tabs and every `giverny manage run` scope
//!   together ([`crate::manage_run`] puts its scopes here too). It is what
//!   keeps the desktop responsive.
//! - **A tab's scope holds guarantees, not ceilings**: `CPUWeight` from the
//!   default lease's cores (`[management_panel.lease]`, 3 unless changed;
//!   100 a core) and `MemoryLow` at its RAM. Idle cores and free memory
//!   are anyone's; under contention the CPU splits by weight, and when the
//!   slice is at its ceiling the kernel reclaims from scopes above their
//!   protection first. (The protection counts against the slice's own
//!   limit; under pressure on the whole machine it does not, as the
//!   slices above ours protect nothing.)
//! - **Commands die before claude.** When the slice runs out of memory the
//!   kernel kills the process with the highest badness in it. Every
//!   process under a claude in the slice, but no claude, gets
//!   `oom_score_adj` [`COMMAND_OOM_ADJ`] (raising it needs no privilege,
//!   and what they start inherits it), so the runaway command is the one
//!   killed, not a session. `OOMPolicy=continue` keeps systemd from
//!   stopping the scope, and the claude in it, after a kill.
//! - **Plain shells are untouched**: only a process the sampler recognises
//!   as claude is moved, and the shell that started it stays where it is.
//!
//! The move is systemd's `StartTransientUnit` with the pids (`busctl
//! --user`): `systemd-run` can only start a new command, not adopt one.
//! A claude already inside the slice (one started by a capped claude, or
//! adopted on an earlier pass) is left alone. Where there is no user
//! systemd (or `$GIVERNY_RUN_NO_SYSTEMD` is set) nothing is moved, and the
//! log says so once.
//!
//! What it does not hold: a process a claude started and that detached
//! before the first pass saw the claude, a command's first second before
//! the pass raises its `oom_score_adj`, and anything a command reaches
//! outside its own process tree (a daemon it talks to, a Windows program
//! through WSL interop). The plugin's Bash guard ([`crate::bash_guard`])
//! refuses the commands that would leave the scope on purpose.

use std::collections::{HashMap, HashSet};

use giverny_core::config::DefaultLease;
use giverny_core::limits::Resolved;

use crate::session_use::Proc;

/// The slice every Claude tab's scope and every `giverny manage run` scope
/// sits in. systemd reads the dash as nesting: it is
/// `giverny.slice/giverny-claude.slice` under the user's manager.
pub const SLICE: &str = "giverny-claude.slice";

/// The name of a claude's scope.
pub fn scope_name(pid: u32) -> String {
    format!("giverny-claude-{pid}.scope")
}

/// Is a `/proc/<pid>/cgroup` text inside [`SLICE`]?
pub fn in_slice(cgroup: &str) -> bool {
    cgroup
        .lines()
        .filter_map(|l| l.strip_prefix("0::"))
        .any(|path| path.split('/').any(|part| part == SLICE))
}

/// `systemctl --user set-property` arguments that cap the slice at the
/// limits.
pub fn slice_props(limits: &Resolved) -> Vec<String> {
    let mut a: Vec<String> = ["--user", "set-property", "--runtime", SLICE]
        .map(String::from)
        .into();
    if limits.cpu_cores > 0 {
        a.push(format!("CPUQuota={}%", limits.cpu_cores * 100));
    }
    if limits.ram.0 > 0 {
        a.push(format!("MemoryMax={}M", limits.ram.0));
        a.push("MemorySwapMax=0".into());
    }
    a
}

/// A lease's cores as a `CPUWeight`: 100 (systemd's default, one
/// unleased unit) per core.
pub fn cpu_weight(cores: u32) -> u64 {
    (u64::from(cores) * 100).clamp(1, 10_000)
}

/// What a claude's commands get in `oom_score_adj`: when the slice runs
/// out of memory the kernel kills the process with the highest badness
/// (its share of memory, plus this in thousandths), so a command goes
/// before any claude (left at its own, 0). Raising it needs no privilege.
pub const COMMAND_OOM_ADJ: i32 = 500;

/// `busctl` arguments that start `unit`, a scope in [`SLICE`] holding
/// `pids`, weighted and protected for `lease`.
pub fn adopt_args(unit: &str, pids: &[u32], lease: &DefaultLease) -> Vec<String> {
    let mut a: Vec<String> = [
        "--user",
        "call",
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
        "StartTransientUnit",
        "ssa(sv)a(sa(sv))",
        unit,
        "fail",
    ]
    .map(String::from)
    .into();
    let mut props: Vec<Vec<String>> = Vec::new();
    let mut pid_prop: Vec<String> = vec!["PIDs".into(), "au".into(), pids.len().to_string()];
    pid_prop.extend(pids.iter().map(u32::to_string));
    props.push(pid_prop);
    props.push(vec!["Slice".into(), "s".into(), SLICE.into()]);
    props.push(vec![
        "Description".into(),
        "s".into(),
        format!(
            "Giverny: claude {}, leased {} cpu, {}",
            pids.first().copied().unwrap_or(0),
            lease.cpu_cores,
            lease.ram
        ),
    ]);
    // Guarantees, not ceilings: the slice is the only ceiling.
    props.push(vec![
        "CPUWeight".into(),
        "t".into(),
        cpu_weight(lease.cpu_cores).to_string(),
    ]);
    if lease.ram.0 > 0 {
        props.push(vec![
            "MemoryLow".into(),
            "t".into(),
            (lease.ram.0 * 1024 * 1024).to_string(),
        ]);
    }
    props.push(vec!["OOMPolicy".into(), "s".into(), "continue".into()]);
    props.push(vec![
        "CollectMode".into(),
        "s".into(),
        "inactive-or-failed".into(),
    ]);
    a.push(props.len().to_string());
    a.extend(props.into_iter().flatten());
    a.push("0".into()); // no auxiliary units
    a
}

/// The claudes to adopt this pass: each claude in `claudes` with no other
/// of them above it (a claude inside a claude rides with the outer one),
/// not seen before (`seen`, by pid and start time).
pub fn outermost(procs: &[Proc], claudes: &HashSet<u32>, seen: &HashMap<u32, u64>) -> Vec<u32> {
    let parent: HashMap<u32, u32> = procs.iter().map(|p| (p.pid, p.ppid)).collect();
    let start: HashMap<u32, u64> = procs.iter().map(|p| (p.pid, p.start)).collect();
    let mut out: Vec<u32> = claudes
        .iter()
        .copied()
        .filter(|pid| seen.get(pid) != start.get(pid))
        .filter(|&pid| {
            let mut p = pid;
            for _ in 0..64 {
                match parent.get(&p) {
                    Some(&pp) if pp != p && pp > 1 => {
                        if claudes.contains(&pp) {
                            return false;
                        }
                        p = pp;
                    }
                    _ => break,
                }
            }
            true
        })
        .collect();
    out.sort_unstable();
    out
}

/// `root` and every process below it whose cgroup is `root`'s
/// (`cgroup_of`): what is moved with it. A child already in a scope of its
/// own (a `giverny manage run`) stays in it.
pub fn moved_with(
    procs: &[Proc],
    root: u32,
    cgroup_of: impl Fn(u32) -> Option<String>,
) -> Vec<u32> {
    let Some(home) = cgroup_of(root) else {
        return vec![root];
    };
    let mut out: Vec<u32> = crate::session_use::subtree(procs, root)
        .into_iter()
        .filter(|&p| p == root || cgroup_of(p).as_deref() == Some(home.as_str()))
        .collect();
    out.sort_unstable();
    // The claude first: the scope's description names it.
    if let Some(i) = out.iter().position(|&p| p == root) {
        out.swap(0, i);
    }
    out
}

/// Whether systemd was asked yet, and what it said.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Unasked,
    Usable,
    Off,
}

/// The processes under the capped claudes `roots` that are not claudes
/// themselves and not handled before (`done`, by pid → start time): the
/// ones whose `oom_score_adj` is raised to [`COMMAND_OOM_ADJ`].
pub fn commands_under(
    procs: &[Proc],
    roots: &HashSet<u32>,
    claudes: &HashSet<u32>,
    done: &HashMap<u32, u64>,
) -> Vec<u32> {
    let start: HashMap<u32, u64> = procs.iter().map(|p| (p.pid, p.start)).collect();
    let mut out: Vec<u32> = roots
        .iter()
        .flat_map(|&r| crate::session_use::subtree(procs, r))
        .filter(|p| !claudes.contains(p))
        .filter(|p| done.get(p) != start.get(p))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The app's sampler's half of the cap: [`Adopter::tick`] each pass.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Default)]
pub struct Adopter {
    state: State,
    /// Claudes handled, by pid → start time (a pid that comes back with
    /// another start is another process).
    seen: HashMap<u32, u64>,
    /// The outermost claudes that are in the slice: whose commands are
    /// put first in line for an OOM kill.
    capped: HashSet<u32>,
    /// Commands whose `oom_score_adj` was raised, by pid → start time.
    raised: HashMap<u32, u64>,
    /// The slice properties last set, so they are set again only when the
    /// limits change.
    slice_set: Option<Vec<String>>,
    failures: u32,
}

impl Adopter {
    /// Move each claude not yet in the slice into a scope of its own
    /// there, and put every command under a capped claude first in line
    /// for an OOM kill. `procs` is the pass's process table; `claudes` the
    /// claudes under the app.
    #[cfg(target_os = "linux")]
    pub fn tick(&mut self, procs: &[Proc], claudes: &HashSet<u32>) {
        if self.state == State::Off {
            return;
        }
        self.seen.retain(|pid, _| claudes.contains(pid));
        self.capped.retain(|pid| claudes.contains(pid));
        let start: HashMap<u32, u64> = procs.iter().map(|p| (p.pid, p.start)).collect();
        self.raised.retain(|pid, _| start.contains_key(pid));
        let cgroup_of = |pid: u32| std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok();
        for pid in outermost(procs, claudes, &self.seen) {
            self.seen
                .insert(pid, start.get(&pid).copied().unwrap_or_default());
            let Some(cg) = cgroup_of(pid) else { continue };
            if in_slice(&cg) {
                self.capped.insert(pid);
                continue;
            }
            if self.state == State::Unasked {
                self.state = match crate::manage_run::detect_mode() {
                    crate::manage_run::Mode::Systemd { .. } => State::Usable,
                    crate::manage_run::Mode::Plain => {
                        tracing::warn!(
                            "Claude tabs run uncapped: no user systemd here (or ${} set), \
                             so their claude is not put in {SLICE}",
                            crate::manage_run::NO_SYSTEMD_ENV
                        );
                        State::Off
                    }
                };
                if self.state == State::Off {
                    return;
                }
            }
            self.cap_slice();
            let lease = DefaultLease::load();
            let pids = moved_with(procs, pid, cgroup_of);
            let unit = scope_name(pid);
            match run_quiet("busctl", &adopt_args(&unit, &pids, &lease)) {
                Ok(()) => {
                    self.capped.insert(pid);
                    tracing::info!(
                        "claude {pid} in {SLICE}/{unit}: weight {}, {} protected \
                         ({} process(es) moved)",
                        cpu_weight(lease.cpu_cores),
                        lease.ram,
                        pids.len()
                    );
                }
                Err(err) => {
                    self.failures += 1;
                    if self.failures == 1 {
                        tracing::warn!("claude {pid} runs uncapped: {unit} not started: {err}");
                    } else {
                        tracing::debug!("claude {pid} runs uncapped: {unit} not started: {err}");
                    }
                }
            }
        }
        for pid in commands_under(procs, &self.capped, claudes, &self.raised) {
            self.raised
                .insert(pid, start.get(&pid).copied().unwrap_or_default());
            raise_oom_adj(pid);
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn tick(&mut self, _procs: &[Proc], _claudes: &HashSet<u32>) {}

    /// Cap the slice at the limits, when they are not what was set last.
    #[cfg(target_os = "linux")]
    fn cap_slice(&mut self) {
        let args = slice_args_now();
        if self.slice_set.as_ref() == Some(&args) {
            return;
        }
        match run_quiet("systemctl", &args) {
            Ok(()) => {
                tracing::info!("{SLICE} capped: {}", args[4..].join(" "));
                self.slice_set = Some(args);
            }
            Err(err) => tracing::warn!("{SLICE}: not capped: {err}"),
        }
    }
}

/// [`slice_props`] for this machine under the limits in Giverny's
/// `config.toml` (`auto` where unreadable).
#[cfg(target_os = "linux")]
pub fn slice_args_now() -> Vec<String> {
    let limits = giverny_core::limits::Limits::load().unwrap_or_default();
    // Cores and RAM only: the GPUs (`nvidia-smi`, slow) are no cgroup's.
    // The machine's cores, not this process's cgroup share.
    let cores = crate::session_use::sysconf(libc::_SC_NPROCESSORS_ONLN).unwrap_or(1) as u32;
    let ram_kb = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("MemTotal:"))
                .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        })
        .unwrap_or(0);
    let machine = giverny_core::limits::Machine {
        cores,
        ram: giverny_core::limits::Mem(ram_kb / 1024),
        gpus: Vec::new(),
    };
    slice_props(&limits.resolve(&machine))
}

/// Cap the slice at the limits now (`giverny manage run`, before it puts a
/// scope in it, so the slice is capped even where no Giverny adopted a
/// claude yet).
#[cfg(target_os = "linux")]
pub fn cap_slice_now() -> Result<(), String> {
    run_quiet("systemctl", &slice_args_now())
}

#[cfg(not(target_os = "linux"))]
pub fn cap_slice_now() -> Result<(), String> {
    Ok(())
}

/// Put `pid` first in line for an OOM kill ([`COMMAND_OOM_ADJ`]), unless
/// it is there already. A process that is gone, or not ours, is left.
#[cfg(target_os = "linux")]
fn raise_oom_adj(pid: u32) {
    let path = format!("/proc/{pid}/oom_score_adj");
    let now = std::fs::read_to_string(&path)
        .ok()
        .and_then(|v| v.trim().parse::<i32>().ok());
    if now.is_some_and(|v| v < COMMAND_OOM_ADJ) {
        let _ = std::fs::write(&path, COMMAND_OOM_ADJ.to_string());
    }
}

/// Run `prog args`, its stderr the error when it fails.
#[cfg(target_os = "linux")]
fn run_quiet(prog: &str, args: &[String]) -> Result<(), String> {
    let out = std::process::Command::new(prog)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("{prog}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{prog} exit {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use giverny_core::limits::Mem;

    fn p(pid: u32, ppid: u32) -> Proc {
        Proc {
            pid,
            ppid,
            ticks: 0,
            mem_kb: 0,
            start: u64::from(pid) * 10,
        }
    }

    #[test]
    fn the_slice_is_read_from_the_cgroup_line() {
        assert!(in_slice(
            "0::/user.slice/user-1000.slice/user@1000.service/giverny.slice/\
             giverny-claude.slice/giverny-claude-42.scope\n"
        ));
        assert!(!in_slice("0::/init.scope\n"));
        assert!(!in_slice(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/x.scope\n"
        ));
        assert!(!in_slice(""));
    }

    #[test]
    fn the_slice_is_capped_at_the_limits() {
        let r = Resolved {
            cpu_cores: 12,
            ram: Mem::gb(16),
            gpus: vec![],
        };
        assert_eq!(
            slice_props(&r).join(" "),
            "--user set-property --runtime giverny-claude.slice CPUQuota=1200% \
             MemoryMax=16384M MemorySwapMax=0"
        );
    }

    #[test]
    fn the_scope_is_weighted_and_protected_for_the_lease_never_capped() {
        let lease = DefaultLease {
            cpu_cores: 3,
            ram: Mem::gb(3),
        };
        let a = adopt_args("giverny-claude-7.scope", &[7, 9], &lease);
        let s = a.join(" ");
        assert!(
            s.starts_with(
                "--user call org.freedesktop.systemd1 /org/freedesktop/systemd1 \
                 org.freedesktop.systemd1.Manager StartTransientUnit ssa(sv)a(sa(sv)) \
                 giverny-claude-7.scope fail 7 PIDs au 2 7 9 Slice s giverny-claude.slice \
                 Description s "
            ),
            "{s}"
        );
        assert!(
            s.ends_with(
                "CPUWeight t 300 MemoryLow t 3221225472 \
                 OOMPolicy s continue CollectMode s inactive-or-failed 0"
            ),
            "{s}"
        );
        assert!(!s.contains("MemoryMax") && !s.contains("CPUQuota"), "{s}");
        assert_eq!(cpu_weight(0), 1);
        assert_eq!(cpu_weight(500), 10_000);
        // The description is one argument, however many words it has.
        let d = a.iter().position(|x| x == "Description").unwrap();
        assert_eq!(a[d + 2], "Giverny: claude 7, leased 3 cpu, 3G");
    }

    #[test]
    fn a_claude_inside_a_claude_rides_with_it_and_each_is_taken_once() {
        // app 1 → shell 10 → claude 20 → bash 30 → claude 40;
        // app 1 → shell 11 → claude 21.
        let procs = vec![
            p(10, 1),
            p(20, 10),
            p(30, 20),
            p(40, 30),
            p(11, 1),
            p(21, 11),
        ];
        let claudes: HashSet<u32> = [20, 40, 21].into();
        let mut seen = HashMap::new();
        assert_eq!(outermost(&procs, &claudes, &seen), vec![20, 21]);
        seen.insert(20, 200);
        assert_eq!(outermost(&procs, &claudes, &seen), vec![21]);
        // Pid 20 again with another start: another claude.
        seen.insert(20, 999);
        seen.insert(21, 210);
        assert_eq!(outermost(&procs, &claudes, &seen), vec![20]);
    }

    #[test]
    fn a_capped_claudes_commands_go_first_in_an_oom_and_claudes_never() {
        // claude 20 → bash 30 → cargo 31; claude 20 → bash 32 → claude 40
        // → bash 41; claude 21 is not capped.
        let procs = vec![
            p(20, 10),
            p(30, 20),
            p(31, 30),
            p(32, 20),
            p(40, 32),
            p(41, 40),
            p(21, 11),
            p(50, 21),
        ];
        let claudes: HashSet<u32> = [20, 40, 21].into();
        let roots: HashSet<u32> = [20].into();
        let mut done = HashMap::new();
        assert_eq!(
            commands_under(&procs, &roots, &claudes, &done),
            vec![30, 31, 32, 41]
        );
        done.insert(30, 300);
        done.insert(31, 1);
        assert_eq!(
            commands_under(&procs, &roots, &claudes, &done),
            vec![31, 32, 41],
            "a pid back with another start is another process"
        );
    }

    #[test]
    fn what_moves_with_a_claude_is_what_shares_its_cgroup() {
        // claude 20 → mcp 21, bash 22 → `manage run` 23 in its own scope.
        let procs = vec![p(20, 10), p(21, 20), p(22, 20), p(23, 22), p(24, 23)];
        let cg = |pid: u32| {
            Some(match pid {
                23 | 24 => "0::/u/giverny-claude.slice/giverny-run-x.scope\n".to_string(),
                _ => "0::/init.scope\n".to_string(),
            })
        };
        assert_eq!(moved_with(&procs, 20, cg), vec![20, 21, 22]);
    }
}

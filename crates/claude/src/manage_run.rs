//! `giverny manage run <task> -- <cmd…>`: a worker's heavy command, held to
//! its task's granted lease and measured.
//!
//! **The cap.** On Linux with a user systemd the command runs in a scope of
//! its own: `systemd-run --user --scope -p MemoryMax=<ram> -p
//! MemorySwapMax=0 -p CPUQuota=<cpu×100>%` (and `OOMPolicy=continue`, so the
//! scope outlives a kill and can be read), visible in `systemctl --user
//! status giverny-run-…` while it runs. `CARGO_BUILD_JOBS=<cpu>` is exported
//! unless set. Anywhere else (no user systemd, macOS, Windows,
//! `$GIVERNY_RUN_NO_SYSTEMD`) it runs plain: the lease is advisory.
//!
//! **The lease.** The task's lease in the ledger ([`resources`]). With none,
//! `run` claims one itself — the default lease (`[management_panel.lease]`,
//! Settings → Management panel; 3 cores and 3G unless changed there) unless
//! `--cpu`/`--ram` say —
//! waits while that is queued (the row waiting, as `eta --why wait`), and
//! releases it when the command ends. Refused (larger than the limits): the
//! command does not run, exit 5.
//!
//! **Slots.** For the command's life `run` holds an exclusive `flock` on
//! each slot its lease holds (`<ledger dir>/slots/<name>.lock`), so two
//! `run`s under one lease take a slot in turn; the second says so and marks
//! its row waiting until it has it.
//!
//! **Heartbeat.** Leases expire 20 minutes after their session's last
//! `giverny manage` command; while the command runs, `run` beats the
//! session's leases every [`HEARTBEAT_EVERY`].
//!
//! **Measured.** Peak memory: the scope cgroup's `memory.peak` (read by a
//! small `sh` shim inside the scope after the command exits, while the
//! cgroup still exists, along with `memory.events`' `oom_kill`), else the
//! largest process's RSS (`wait4`'s `ru_maxrss`). CPU time: `wait4`'s user +
//! system time of the command and every descendant it waited for. Both go
//! on the task's feed row (`usage`), and from there into the history when
//! the task lands, so a later `claim` can say what tasks like it peaked at.
//! A command the memory cap killed is reported as such.
//!
//! The exit code is the command's (128 + the signal when one killed it).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use giverny_core::limits::Mem;
use serde_json::{Map, Value, json};

use crate::manage::{self, Flags};
use crate::{manage_history, resources, run_live};

/// How often a running command's session leases are beaten (the TTL is 20m).
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(5 * 60);
/// How often a queued claim is asked again.
const QUEUED_POLL: Duration = Duration::from_secs(15);
/// Set (non-empty): never use systemd; run plain.
pub const NO_SYSTEMD_ENV: &str = "GIVERNY_RUN_NO_SYSTEMD";
/// Where the shim writes what it read of the scope.
const STATS_ENV: &str = "GIVERNY_RUN_STATS";

/// Runs the command inside its scope, then reads the scope's cgroup while
/// it still exists. `started` first, so an empty file means the scope never
/// ran (systemd-run failed) and the command can be run plain instead.
const SHIM: &str = r#"# giverny manage run: run the command in its systemd scope, then
# record the scope's peak memory and OOM kills while the scope still exists.
s="$GIVERNY_RUN_STATS"
cg=$(sed -n 's/^0:://p' /proc/self/cgroup 2>/dev/null)
[ -n "$cg" ] || cg=/nonexistent
d="/sys/fs/cgroup$cg"
[ -n "$s" ] && printf 'started\ncgroup %s\n' "$d" > "$s" 2>/dev/null
"$@"
rc=$?
if [ -n "$s" ]; then
  {
    echo started
    [ -r "$d/memory.peak" ] && echo "peak $(cat "$d/memory.peak")"
    grep '^oom_kill ' "$d/memory.events" 2>/dev/null
  } > "$s" 2>/dev/null
fi
exit $rc
"#;

/// How the command is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// In a `systemd-run --user --scope`; `expand_flag` when systemd is new
    /// enough (254) to take `--expand-environment=no`, so a `$` in the
    /// command is never expanded by systemd.
    Systemd { expand_flag: bool },
    /// Plain, uncapped: the lease is advisory.
    Plain,
}

/// A user systemd that `systemd-run --user --scope` can use, else plain.
pub fn detect_mode() -> Mode {
    if std::env::var_os(NO_SYSTEMD_ENV).is_some_and(|v| !v.is_empty()) {
        return Mode::Plain;
    }
    #[cfg(target_os = "linux")]
    {
        let quiet = |c: &mut Command| {
            c.stdin(Stdio::null()).stderr(Stdio::null());
        };
        let mut show = Command::new("systemctl");
        show.args(["--user", "show", "-p", "Version", "--value"]);
        quiet(&mut show);
        let major = show
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|n| n.parse::<u32>().ok())
            });
        let mut run = Command::new("systemd-run");
        run.arg("--version").stdout(Stdio::null());
        quiet(&mut run);
        if let Some(major) = major
            && run.status().is_ok_and(|s| s.success())
        {
            return Mode::Systemd {
                expand_flag: major >= 254,
            };
        }
    }
    Mode::Plain
}

/// What the command is held to: the lease's cores and RAM (0 = no cap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap {
    pub cpu: u32,
    pub ram_mb: u64,
}

impl Cap {
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.cpu > 0 {
            parts.push(format!("{} cpu", self.cpu));
        }
        if self.ram_mb > 0 {
            parts.push(Mem(self.ram_mb).to_string());
        }
        if parts.is_empty() {
            "nothing".into()
        } else {
            parts.join(", ")
        }
    }
}

/// A unit name from `task`: `giverny-run-giverny_161-4242`.
pub fn unit_name(task: &str, pid: u32) -> String {
    let t: String = task
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    format!("giverny-run-{t}-{pid}")
}

/// `systemd-run`'s arguments for `cmd` in a capped scope, through `shim`.
pub fn scope_args(
    expand_flag: bool,
    unit: &str,
    task: &str,
    cap: &Cap,
    shim: &Path,
    cmd: &[String],
) -> Vec<String> {
    let mut a: Vec<String> = ["--user", "--scope", "--quiet", "--collect"]
        .map(String::from)
        .into();
    if expand_flag {
        a.push("--expand-environment=no".into());
    }
    a.push(format!("--unit={unit}"));
    a.push(format!(
        "--description=giverny manage run {}",
        task.replace('%', "%%")
    ));
    let mut prop = |p: String| {
        a.push("-p".into());
        a.push(p);
    };
    if cap.ram_mb > 0 {
        prop(format!("MemoryMax={}M", cap.ram_mb));
        prop("MemorySwapMax=0".into());
    }
    if cap.cpu > 0 {
        prop(format!("CPUQuota={}%", cap.cpu * 100));
    }
    // A kill by the cap stops only the command: the shim lives on to read
    // the cgroup, and the scope is not torn down under it.
    prop("OOMPolicy=continue".into());
    a.push("--".into());
    a.push("sh".into());
    a.push(shim.display().to_string());
    a.extend(cmd.iter().cloned());
    a
}

/// What the shim read of the scope.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeStats {
    /// The shim ran: the scope was made.
    pub started: bool,
    pub peak_bytes: Option<u64>,
    pub oom_kills: u64,
}

pub fn parse_stats(text: &str) -> ScopeStats {
    let mut s = ScopeStats::default();
    for line in text.lines() {
        let mut w = line.split_whitespace();
        match (w.next(), w.next().and_then(|n| n.parse::<u64>().ok())) {
            (Some("started"), _) => s.started = true,
            (Some("peak"), Some(n)) => s.peak_bytes = Some(n),
            (Some("oom_kill"), Some(n)) => s.oom_kills = n,
            _ => {}
        }
    }
    s
}

/// One run, measured.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Measured {
    pub exit: i32,
    /// The signal that killed the command, when `wait4` saw one.
    pub signal: Option<i32>,
    pub peak_mb: Option<u64>,
    pub cpu_ms: Option<u64>,
    pub wall_ms: u64,
    pub oom_kills: u64,
    /// Held by a systemd scope.
    pub capped: bool,
}

/// Knobs a test turns.
#[derive(Debug, Clone, Copy)]
pub struct Opts {
    pub mode: Mode,
    pub heartbeat_every: Duration,
    /// Ignore Ctrl-C and Ctrl-\ while the command runs, as a shell does, so
    /// the command's usage is still recorded (the CLI only: it is
    /// process-wide).
    pub ignore_signals: bool,
}

/// `giverny manage run`, from [`manage::run_in_code`].
pub fn run(
    dir: &Path,
    ledger: &Path,
    session: &str,
    task: &str,
    flags: &Flags,
    cap: Option<&resources::Capacity>,
) -> Result<(String, i32), String> {
    let opts = Opts {
        mode: detect_mode(),
        heartbeat_every: HEARTBEAT_EVERY,
        ignore_signals: true,
    };
    run_with(dir, ledger, session, task, flags, cap, opts)
}

fn say(task: &str, s: &str) {
    eprintln!("giverny manage run {task}: {s}");
}

/// [`run`] with its knobs.
pub fn run_with(
    dir: &Path,
    ledger: &Path,
    session: &str,
    task: &str,
    flags: &Flags,
    cap: Option<&resources::Capacity>,
    opts: Opts,
) -> Result<(String, i32), String> {
    let Some((lease, claimed)) = lease_for(dir, ledger, session, task, flags, cap)? else {
        return Ok((String::new(), resources::exit::REFUSED));
    };
    let held = (|| {
        let _slots = lock_slots(dir, ledger, session, task, &lease.slots)?;
        let cap = Cap {
            cpu: lease.cpu,
            ram_mb: lease.ram_mb,
        };
        let hb = Heartbeat::start(
            ledger.to_path_buf(),
            session.to_string(),
            opts.heartbeat_every,
        );
        let m = execute(ledger, session, task, &cap, &flags.command, opts);
        drop(hb);
        m.map(|m| (m, cap))
    })();
    // Whatever happened, a lease `run` claimed goes back.
    if claimed {
        let now = manage::now_ms();
        match resources::release_at(ledger, session, task, now) {
            Ok(_) => resources::annotate_row(dir, session, task, None),
            Err(e) => say(
                task,
                &format!("could not release the lease it claimed: {e}"),
            ),
        }
    }
    let (m, cap) = held?;
    let _ = resources::heartbeat(ledger, None, session, manage::now_ms());
    let recorded = manage::edit_row(dir, session, task, |row| {
        record_usage(row, &m, &cap, &flags.command, manage::now_ms())
    });
    say(task, &summary(&m, &cap));
    if m.oom_kills > 0 {
        let more = Mem((cap.ram_mb * 2).max(1024));
        say(
            task,
            &format!(
                "the {} memory cap killed it (OOM). Ask for more: `giverny manage release {task}` \
                 then `giverny manage claim {task} --ram {more}` (or have the manager do it), \
                 and run it again",
                Mem(cap.ram_mb)
            ),
        );
    } else if m.capped && m.signal == Some(9) && cap.ram_mb > 0 {
        say(
            task,
            "killed by SIGKILL: perhaps the memory cap, perhaps someone else",
        );
    }
    if !recorded {
        say(
            task,
            "no row for it in this manager session, so the usage is on no row and will not reach the history",
        );
    }
    Ok((String::new(), m.exit))
}

/// `exit 0; peak 1.2G of 3G, 45.1s CPU in 30.2s (3 cpu cap)`.
pub fn summary(m: &Measured, cap: &Cap) -> String {
    let secs = |ms: u64| format!("{:.1}s", ms as f64 / 1000.0);
    let mut s = match m.signal {
        Some(sig) => format!("exit {} (signal {sig})", m.exit),
        None => format!("exit {}", m.exit),
    };
    let peak = m
        .peak_mb
        .map(|p| Mem(p).to_string())
        .unwrap_or_else(|| "?".into());
    if m.capped && cap.ram_mb > 0 {
        s.push_str(&format!("; peak {peak} of {}", Mem(cap.ram_mb)));
    } else {
        s.push_str(&format!("; peak {peak}"));
    }
    if let Some(c) = m.cpu_ms {
        s.push_str(&format!(", {} CPU", secs(c)));
    }
    s.push_str(&format!(" in {}", secs(m.wall_ms)));
    if !m.capped {
        s.push_str(" (uncapped)");
    }
    s
}

/// The task's lease, or one claimed now (true: `run` claimed it). `None`:
/// refused, already said.
fn lease_for(
    dir: &Path,
    ledger: &Path,
    session: &str,
    task: &str,
    flags: &Flags,
    cap: Option<&resources::Capacity>,
) -> Result<Option<(resources::Lease, bool)>, String> {
    let now = manage::now_ms();
    if let Some(l) = resources::with_ledger(ledger, now, |l| l.lease(session, task).cloned())? {
        return Ok(Some((l, false)));
    }
    let default = match cap {
        Some(c) => c.default_lease,
        None => giverny_core::config::DefaultLease::load(),
    };
    let req = resources::Request {
        cpu: flags.cpu.unwrap_or(default.cpu_cores),
        ram_mb: flags.ram_mb.unwrap_or(default.ram.0),
        ..flags.request()
    };
    let repo = flags.repo.clone().or_else(|| {
        let cwd = std::env::current_dir().unwrap_or_default();
        manage_history::repo_of(task, &cwd)
    });
    let mut waiting = false;
    let mut said = false;
    loop {
        let capacity = match cap {
            Some(c) => c.clone(),
            None => resources::Capacity::detect()?,
        };
        let now = manage::now_ms();
        let out = resources::with_ledger(ledger, now, |l| {
            l.claim(&capacity, session, task, repo.as_deref(), &req, now)
        })?;
        resources::annotate_row(dir, session, task, resources::row_lease(&out, &req));
        let line =
            resources::outcome_line(task, &out, &|s, t| resources::eta_left_s(dir, s, t, now));
        let got = match out {
            resources::Outcome::Granted(l)
            | resources::Outcome::GrantedSmaller { lease: l, .. } => {
                say(task, &format!("held no lease, so claimed one: {line}"));
                Some((l, true))
            }
            resources::Outcome::Held(l) => Some((l, false)),
            resources::Outcome::Refused(why) => {
                say(task, &format!("held no lease and cannot claim one: {why}"));
                if waiting {
                    manage::mark_waiting(dir, session, task, None, now);
                }
                return Ok(None);
            }
            resources::Outcome::Queued { .. } => {
                if !said {
                    say(
                        task,
                        &format!("held no lease, claimed one: {line}; waiting"),
                    );
                    said = true;
                }
                if !waiting {
                    waiting = manage::mark_waiting(
                        dir,
                        session,
                        task,
                        Some("giverny manage run: queued for a lease"),
                        now,
                    );
                }
                None
            }
        };
        if let Some(got) = got {
            if waiting {
                manage::mark_waiting(dir, session, task, None, manage::now_ms());
            }
            return Ok(Some(got));
        }
        std::thread::sleep(QUEUED_POLL);
    }
}

/// `cargo:/x/target` → `cargo_3a_2fx_2ftarget.lock`: every character a
/// file name may not safely hold, escaped, so two slots never share a file.
pub fn slot_file(slot: &str) -> String {
    let mut s = String::new();
    for c in slot.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '.') {
            s.push(c);
        } else {
            let mut b = [0u8; 4];
            for byte in c.encode_utf8(&mut b).bytes() {
                s.push_str(&format!("_{byte:02x}"));
            }
        }
    }
    format!("{s}.lock")
}

/// An exclusive hold on one slot's lock file, let go when dropped (or when
/// the process dies: `flock`).
pub struct SlotLock {
    #[cfg(unix)]
    _file: std::fs::File,
}

/// Take `slot`'s lock under `ledger`'s directory: `Ok(Some)` at once,
/// `Ok(None)` when another holds it and `wait` is false.
pub fn lock_slot(ledger: &Path, slot: &str, wait: bool) -> Result<Option<SlotLock>, String> {
    let dir = ledger.parent().unwrap_or(Path::new(".")).join("slots");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(slot_file(slot));
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let op = if wait {
            libc::LOCK_EX
        } else {
            libc::LOCK_EX | libc::LOCK_NB
        };
        loop {
            // SAFETY: a valid open descriptor; flock only blocks.
            if unsafe { libc::flock(file.as_raw_fd(), op) } == 0 {
                return Ok(Some(SlotLock { _file: file }));
            }
            let e = std::io::Error::last_os_error();
            match e.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EWOULDBLOCK) if !wait => return Ok(None),
                _ => return Err(format!("{}: {e}", path.display())),
            }
        }
    }
    #[cfg(not(unix))]
    {
        // No flock: the ledger's own exclusivity is all there is.
        let _ = (path, wait);
        Ok(Some(SlotLock {}))
    }
}

/// Every slot of the lease, in name order (so two runs never deadlock),
/// waiting for each that another `run` holds — the row waiting meanwhile.
fn lock_slots(
    dir: &Path,
    ledger: &Path,
    session: &str,
    task: &str,
    slots: &[String],
) -> Result<Vec<SlotLock>, String> {
    let mut names: Vec<&String> = slots.iter().collect();
    names.sort();
    names.dedup();
    let mut held = Vec::new();
    let mut waiting = false;
    for s in names {
        let lock = match lock_slot(ledger, s, false)? {
            Some(l) => l,
            None => {
                say(
                    task,
                    &format!("slot {s} is held by another `giverny manage run`; waiting for it"),
                );
                if !waiting {
                    waiting = manage::mark_waiting(
                        dir,
                        session,
                        task,
                        Some(&format!("giverny manage run: waiting for slot {s}")),
                        manage::now_ms(),
                    );
                }
                lock_slot(ledger, s, true)?.ok_or("slot lock not taken")?
            }
        };
        held.push(lock);
    }
    if waiting {
        manage::mark_waiting(dir, session, task, None, manage::now_ms());
    }
    Ok(held)
}

/// Beats `session`'s leases every `every` until dropped.
struct Heartbeat {
    stop: Option<std::sync::mpsc::Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Heartbeat {
    fn start(ledger: PathBuf, session: String, every: Duration) -> Heartbeat {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(every) {
                if let Err(e) = resources::heartbeat(&ledger, None, &session, manage::now_ms()) {
                    eprintln!("giverny manage run: heartbeat: {e}");
                }
            }
        });
        Heartbeat {
            stop: Some(tx),
            handle: Some(handle),
        }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Write the shim beside the ledger (only when its bytes differ).
fn shim_path(ledger: &Path) -> Result<PathBuf, String> {
    let dir = ledger.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join("run-shim.sh");
    if std::fs::read(&path).is_ok_and(|b| b == SHIM.as_bytes()) {
        return Ok(path);
    }
    let tmp = dir.join(format!("run-shim.sh.{}.tmp", std::process::id()));
    std::fs::write(&tmp, SHIM)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

fn stats_path(ledger: &Path) -> Result<PathBuf, String> {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = run_live::runs_dir(ledger);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir.join(format!(
        "{}-{}.stats",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )))
}

/// Run `cmd` under `cap` and measure it. A scope systemd would not make
/// falls back to running plain.
fn execute(
    ledger: &Path,
    session: &str,
    task: &str,
    cap: &Cap,
    cmd: &[String],
    opts: Opts,
) -> Result<Measured, String> {
    if let Mode::Systemd { expand_flag } = opts.mode {
        let shim = shim_path(ledger)?;
        let stats = stats_path(ledger)?;
        let unit = unit_name(task, std::process::id());
        let mut c = Command::new("systemd-run");
        c.args(scope_args(expand_flag, &unit, task, cap, &shim, cmd))
            .env(STATS_ENV, &stats);
        jobs_env(&mut c, cap);
        say(
            task,
            &format!(
                "capped at {} in {unit}.scope (`systemctl --user status {unit}.scope`)",
                cap.describe()
            ),
        );
        // Seen by the management panel while it runs.
        let live = run_live::register(&stats, task, session, manage::now_ms());
        let w = spawn_wait(&mut c, "systemd-run", opts.ignore_signals);
        drop(live);
        let st = std::fs::read_to_string(&stats)
            .map(|t| parse_stats(&t))
            .unwrap_or_default();
        let _ = std::fs::remove_file(&stats);
        let w = w?;
        if st.started {
            let cg_mb = st.peak_bytes.map(|b| b.div_ceil(1024 * 1024));
            return Ok(Measured {
                exit: w.exit,
                signal: w.signal,
                // What the cap measures; RSS only where the kernel has no
                // `memory.peak`.
                peak_mb: cg_mb.or(w.maxrss_mb),
                cpu_ms: w.cpu_ms,
                wall_ms: w.wall_ms,
                oom_kills: st.oom_kills,
                capped: true,
            });
        }
        say(
            task,
            &format!(
                "systemd-run did not make the scope (exit {}); running it plain, uncapped",
                w.exit
            ),
        );
    } else {
        say(
            task,
            &format!(
                "not capped (no user systemd here, or ${NO_SYSTEMD_ENV} set): \
                 running it plain, the lease ({}) advisory",
                cap.describe()
            ),
        );
    }
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]);
    jobs_env(&mut c, cap);
    let w = spawn_wait(&mut c, &cmd[0], opts.ignore_signals)?;
    Ok(Measured {
        exit: w.exit,
        signal: w.signal,
        peak_mb: w.maxrss_mb,
        cpu_ms: w.cpu_ms,
        wall_ms: w.wall_ms,
        oom_kills: 0,
        capped: false,
    })
}

/// `CARGO_BUILD_JOBS=<cpu>` unless it is set already.
fn jobs_env(c: &mut Command, cap: &Cap) {
    if cap.cpu > 0 && std::env::var_os("CARGO_BUILD_JOBS").is_none() {
        c.env("CARGO_BUILD_JOBS", cap.cpu.to_string());
    }
}

struct Waited {
    exit: i32,
    signal: Option<i32>,
    cpu_ms: Option<u64>,
    maxrss_mb: Option<u64>,
    wall_ms: u64,
}

/// Spawn and reap `c`, with its rusage on Unix.
fn spawn_wait(c: &mut Command, what: &str, ignore_signals: bool) -> Result<Waited, String> {
    let t0 = Instant::now();
    let mut child = c.spawn().map_err(|e| format!("{what}: {e}"))?;
    #[cfg(unix)]
    {
        let _ = &mut child;
        let _quiet = ignore_signals.then(IgnoreSignals::new);
        let pid = child.id() as libc::pid_t;
        let mut status: libc::c_int = 0;
        // SAFETY: zeroed rusage is a valid value; wait4 fills it.
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        loop {
            // SAFETY: our own child's pid, valid out-pointers.
            let r = unsafe { libc::wait4(pid, &mut status, 0, &mut ru) };
            if r == pid {
                break;
            }
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::EINTR) {
                return Err(format!("{what}: wait: {e}"));
            }
        }
        let wall_ms = t0.elapsed().as_millis() as u64;
        let (exit, signal) = if libc::WIFEXITED(status) {
            (libc::WEXITSTATUS(status), None)
        } else if libc::WIFSIGNALED(status) {
            let s = libc::WTERMSIG(status);
            (128 + s, Some(s))
        } else {
            (1, None)
        };
        let tv = |t: libc::timeval| t.tv_sec as u64 * 1000 + t.tv_usec as u64 / 1000;
        let cpu_ms = tv(ru.ru_utime) + tv(ru.ru_stime);
        // Linux counts KiB, macOS bytes.
        let rss = ru.ru_maxrss.max(0) as u64;
        let maxrss_mb = if cfg!(target_os = "macos") {
            rss.div_ceil(1024 * 1024)
        } else {
            rss.div_ceil(1024)
        };
        Ok(Waited {
            exit,
            signal,
            cpu_ms: Some(cpu_ms),
            maxrss_mb: (maxrss_mb > 0).then_some(maxrss_mb),
            wall_ms,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = ignore_signals;
        let st = child.wait().map_err(|e| format!("{what}: wait: {e}"))?;
        Ok(Waited {
            exit: st.code().unwrap_or(1),
            signal: None,
            cpu_ms: None,
            maxrss_mb: None,
            wall_ms: t0.elapsed().as_millis() as u64,
        })
    }
}

/// SIGINT and SIGQUIT ignored in this process until dropped. Taken after
/// the spawn, so the command keeps its own defaults.
#[cfg(unix)]
struct IgnoreSignals {
    int: libc::sighandler_t,
    quit: libc::sighandler_t,
}

#[cfg(unix)]
impl IgnoreSignals {
    fn new() -> IgnoreSignals {
        // SAFETY: setting a disposition to SIG_IGN is always sound.
        unsafe {
            IgnoreSignals {
                int: libc::signal(libc::SIGINT, libc::SIG_IGN),
                quit: libc::signal(libc::SIGQUIT, libc::SIG_IGN),
            }
        }
    }
}

#[cfg(unix)]
impl Drop for IgnoreSignals {
    fn drop(&mut self) {
        // SAFETY: restoring the dispositions `new` replaced.
        unsafe {
            libc::signal(libc::SIGINT, self.int);
            libc::signal(libc::SIGQUIT, self.quit);
        }
    }
}

/// Add one run to the row's `usage` (see `docs/management-panel.md`, **Row**).
pub fn record_usage(
    row: &mut Map<String, Value>,
    m: &Measured,
    cap: &Cap,
    cmd: &[String],
    now: u64,
) {
    let mut u = row
        .get("usage")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let get = |u: &Map<String, Value>, k: &str| manage::u64_of(u, k).unwrap_or(0);
    let runs = get(&u, "runs") + 1;
    u.insert("runs".into(), json!(runs));
    match m.peak_mb {
        Some(p) => {
            let peak = p.max(get(&u, "peak_mb"));
            u.insert("peak_mb".into(), json!(peak));
            u.insert("last_peak_mb".into(), json!(p));
        }
        None => {
            u.remove("last_peak_mb");
        }
    }
    let cpu_s = get(&u, "cpu_s") + m.cpu_ms.unwrap_or(0).div_ceil(1000);
    u.insert("cpu_s".into(), json!(cpu_s));
    let wall_s = get(&u, "wall_s") + m.wall_ms.div_ceil(1000);
    u.insert("wall_s".into(), json!(wall_s));
    u.insert(
        "oom_kills".into(),
        json!(get(&u, "oom_kills") + m.oom_kills),
    );
    let mut line = cmd.join(" ");
    if line.chars().count() > 200 {
        line = line.chars().take(199).collect::<String>() + "…";
    }
    u.insert("last_cmd".into(), json!(line));
    u.insert("last_exit".into(), json!(m.exit));
    u.insert("capped".into(), json!(m.capped));
    for (k, v) in [("cap_cpu", cap.cpu as u64), ("cap_ram_mb", cap.ram_mb)] {
        if m.capped && v > 0 {
            u.insert(k.into(), json!(v));
        } else {
            u.remove(k);
        }
    }
    u.insert("last_at".into(), json!(manage::stamp(now)));
    row.insert("usage".into(), Value::Object(u));
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::feed;
    use crate::manage::{Cmd, parse_args, run_in_code};

    const MIN: u64 = 60_000;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("giverny-run-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// 14 cores, 23 G; limits 12 cores, 16 G; idle.
    fn machine() -> resources::Capacity {
        use giverny_core::limits::{Limits, Load, Machine, Resolved};
        resources::Capacity {
            machine: Machine {
                cores: 14,
                ram: Mem::gb(23),
                gpus: vec![],
            },
            limits: Resolved {
                cpu_cores: 12,
                ram: Mem::gb(16),
                gpus: vec![],
            },
            configured: Limits::default(),
            load: Load {
                mem_available: Some(Mem::gb(20)),
                load1: Some(0.0),
            },
            default_lease: Default::default(),
        }
    }

    fn cmd(dir: &Path, line: &str) -> (String, i32) {
        let a: Vec<String> = line.split_whitespace().map(String::from).collect();
        let (c, f) = parse_args(&a).unwrap();
        run_in_code(dir, "s1", &c, &f, manage::now_ms(), Some(&machine())).unwrap()
    }

    fn plain(every_ms: u64) -> Opts {
        Opts {
            mode: Mode::Plain,
            heartbeat_every: Duration::from_millis(every_ms),
            ignore_signals: false,
        }
    }

    /// `run <task> [flags] -- sh -c <script>` in plain mode.
    fn run(dir: &Path, task: &str, extra: &[&str], script: &str, opts: Opts) -> i32 {
        let mut a: Vec<String> = vec!["run".into(), task.into()];
        a.extend(extra.iter().map(|s| s.to_string()));
        a.extend(["--", "sh", "-c", script].map(String::from));
        let (c, f) = parse_args(&a).unwrap();
        assert_eq!(c, Cmd::Run(task.into()));
        let ledger = resources::ledger_path(dir);
        run_with(dir, &ledger, "s1", task, &f, Some(&machine()), opts)
            .unwrap()
            .1
    }

    /// `CARGO_BUILD_JOBS` a command sees: the cap's cores, unless this
    /// test itself runs with it set (as under `giverny manage run`).
    fn jobs(cpu: u32) -> String {
        std::env::var("CARGO_BUILD_JOBS").unwrap_or_else(|_| cpu.to_string())
    }

    fn row(dir: &Path, key: &str) -> feed::FeedRow {
        feed::read(&feed::feed_path(dir, "s1"))
            .unwrap()
            .rows
            .into_iter()
            .find(|r| r.key == key)
            .unwrap()
    }

    fn ledger(dir: &Path) -> resources::Ledger {
        resources::Ledger::parse(&std::fs::read(resources::ledger_path(dir)).unwrap()).unwrap()
    }

    #[test]
    fn the_command_is_everything_after_the_double_dash() {
        let a: Vec<String> = "run t --cpu 2 -- cargo test --release -p x -- --nocapture"
            .split_whitespace()
            .map(String::from)
            .collect();
        let (c, f) = parse_args(&a).unwrap();
        assert_eq!(c, Cmd::Run("t".into()));
        assert_eq!(f.cpu, Some(2));
        assert_eq!(
            f.command,
            ["cargo", "test", "--release", "-p", "x", "--", "--nocapture"]
        );
        let none: Vec<String> = ["run", "t"].map(String::from).into();
        assert!(parse_args(&none).unwrap_err().contains("needs a command"));
        let no_task: Vec<String> = ["run", "--", "ls"].map(String::from).into();
        assert!(parse_args(&no_task).is_err());
    }

    #[test]
    fn the_scope_carries_the_cap() {
        let cap = Cap {
            cpu: 3,
            ram_mb: 3072,
        };
        let cmd: Vec<String> = ["cargo", "test", "$HOME"].map(String::from).into();
        let a = scope_args(
            true,
            &unit_name("demo#161", 42),
            "demo#161",
            &cap,
            Path::new("/l/run-shim.sh"),
            &cmd,
        );
        let s = a.join(" ");
        assert!(s.starts_with("--user --scope --quiet --collect --expand-environment=no"));
        assert!(s.contains("--unit=giverny-run-demo_161-42"), "{s}");
        assert!(
            s.contains("-p MemoryMax=3072M -p MemorySwapMax=0 -p CPUQuota=300%"),
            "{s}"
        );
        assert!(s.contains("-p OOMPolicy=continue"), "{s}");
        assert!(s.ends_with("-- sh /l/run-shim.sh cargo test $HOME"), "{s}");
        // A lease of no RAM sets no memory cap; old systemd gets no flag.
        let a = scope_args(
            false,
            "u",
            "t",
            &Cap { cpu: 1, ram_mb: 0 },
            Path::new("/s"),
            &cmd,
        )
        .join(" ");
        assert!(!a.contains("MemoryMax") && !a.contains("expand"), "{a}");
        assert_eq!(slot_file("cargo:/x/t"), "cargo_3a_2fx_2ft.lock");
        assert_eq!(
            parse_stats("started\npeak 3221225472\noom_kill 2\n"),
            ScopeStats {
                started: true,
                peak_bytes: Some(3 << 30),
                oom_kills: 2
            }
        );
        assert!(!parse_stats("").started);
    }

    #[test]
    fn a_leased_run_keeps_its_exit_and_lands_its_usage_on_the_row_and_in_the_history() {
        let dir = scratch("leased");
        cmd(&dir, "start demo#7 --eta 30 --title BUG:x");
        let (_, code) = cmd(&dir, "claim demo#7 --cpu 2 --ram 1G");
        assert_eq!(code, 0);
        let code = run(
            &dir,
            "demo#7",
            &[],
            &format!("test \"$CARGO_BUILD_JOBS\" = {} && exit 3", jobs(2)),
            plain(60_000),
        );
        assert_eq!(
            code, 3,
            "the command's exit, with CARGO_BUILD_JOBS from the lease"
        );
        let u = row(&dir, "demo#7").usage.unwrap();
        assert_eq!((u.runs, u.last_exit, u.capped), (1, Some(3), false));
        assert!(u.peak_mb.is_some_and(|p| p > 0), "{u:?}");
        assert_eq!(u.cap_ram_mb, None, "uncapped runs record no cap");
        assert!(
            u.last_cmd
                .as_deref()
                .is_some_and(|c| c.starts_with("sh -c test \"$CARGO_BUILD_JOBS\" = ")),
            "{u:?}"
        );
        // The lease was the dispatcher's: it stays.
        assert!(ledger(&dir).lease("s1", "demo#7").is_some());
        // A command killed by a signal exits 128 + it.
        let code = run(&dir, "demo#7", &[], "kill -9 $$", plain(60_000));
        assert_eq!(code, 137);
        let u = row(&dir, "demo#7").usage.unwrap();
        assert_eq!((u.runs, u.last_exit), (2, Some(137)));
        // Landing carries the peak into the history.
        cmd(&dir, "land demo#7");
        let h = manage_history::load(&dir.join(manage_history::FILE));
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].peak_mb, u.peak_mb);
        assert!(
            row(&dir, "demo#7").usage.is_some(),
            "a Done row keeps its usage"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_lease_run_claims_one_and_gives_it_back_and_a_refusal_runs_nothing() {
        let dir = scratch("claims");
        cmd(&dir, "start t --eta 30");
        let seen = dir.join("seen");
        let script = format!(
            "test \"$CARGO_BUILD_JOBS\" = {} && touch {}",
            jobs(3),
            seen.display()
        );
        assert_eq!(run(&dir, "t", &[], &script, plain(60_000)), 0);
        assert!(seen.exists(), "the default claim is 3 cpu");
        let l = ledger(&dir);
        assert!(l.leases.is_empty(), "released after: {l:?}");
        assert_eq!(row(&dir, "t").lease, None);
        // Too big for the limits: refused, the command never runs.
        let never = dir.join("never");
        let code = run(
            &dir,
            "t",
            &["--cpu", "99"],
            &format!("touch {}", never.display()),
            plain(60_000),
        );
        assert_eq!(code, resources::exit::REFUSED);
        assert!(!never.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_lease_run_claims_the_configured_default() {
        let dir = scratch("default-lease");
        cmd(&dir, "start t --eta 30");
        let mut cap = machine();
        cap.default_lease = giverny_core::config::DefaultLease {
            cpu_cores: 2,
            ram: Mem::gb(1),
        };
        let seen = dir.join("seen");
        let script = format!(
            "test \"$CARGO_BUILD_JOBS\" = {} && touch {}",
            jobs(2),
            seen.display()
        );
        let a: Vec<String> = ["run", "t", "--", "sh", "-c", &script]
            .map(String::from)
            .to_vec();
        let (_, f) = parse_args(&a).unwrap();
        let ledger = resources::ledger_path(&dir);
        let (said, code) =
            run_with(&dir, &ledger, "s1", "t", &f, Some(&cap), plain(60_000)).unwrap();
        assert_eq!(code, 0, "{said}");
        assert!(seen.exists(), "the default lease's cores are the cap");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_running_command_keeps_its_lease_alive() {
        let dir = scratch("beat");
        let ledger_path = resources::ledger_path(&dir);
        let t0 = manage::now_ms() - 10 * MIN;
        resources::with_ledger(&ledger_path, t0, |l| {
            l.claim(
                &machine(),
                "s1",
                "t",
                None,
                &resources::Request {
                    cpu: 1,
                    ..Default::default()
                },
                t0,
            )
        })
        .unwrap();
        // Stamps are whole seconds.
        let before = manage::now_ms() / 1000 * 1000;
        assert_eq!(run(&dir, "t", &[], "sleep 0.5", plain(50)), 0);
        let hb = ledger(&dir).lease("s1", "t").unwrap().heartbeat_at;
        assert!(hb >= before, "beaten while it ran: {hb} < {before}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_run_on_a_held_slot_waits_and_marks_the_row_waiting() {
        let dir = scratch("slot");
        cmd(&dir, "start t --eta 30");
        assert_eq!(cmd(&dir, "claim t --cpu 1 --slot cargo:/x").1, 0);
        let ledger_path = resources::ledger_path(&dir);
        let held = lock_slot(&ledger_path, "cargo:/x", false).unwrap().unwrap();
        assert!(
            lock_slot(&ledger_path, "cargo:/x", false)
                .unwrap()
                .is_none()
        );
        let d = dir.clone();
        let worker = std::thread::spawn(move || run(&d, "t", &[], "exit 0", plain(60_000)));
        let mut waited = false;
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(20));
            let f = std::fs::read_to_string(feed::feed_path(&dir, "s1")).unwrap();
            if f.contains("waiting_since") {
                waited = true;
                break;
            }
        }
        assert!(waited, "the row says it waits");
        assert!(!worker.is_finished());
        drop(held);
        assert_eq!(worker.join().unwrap(), 0);
        let f = std::fs::read_to_string(feed::feed_path(&dir, "s1")).unwrap();
        assert!(!f.contains("waiting_since"), "the wait closed: {f}");
        assert!(f.contains("\"wait_s\""), "{f}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real thing, where this machine has a user systemd: the shim
    /// reads the scope's peak and the cap is recorded.
    #[test]
    fn under_systemd_the_scope_is_measured() {
        let Mode::Systemd { .. } = detect_mode() else {
            return;
        };
        let dir = scratch("scope");
        cmd(&dir, "start t --eta 30");
        assert_eq!(cmd(&dir, "claim t --cpu 1 --ram 256M").1, 0);
        let opts = Opts {
            mode: detect_mode(),
            ..plain(60_000)
        };
        assert_eq!(run(&dir, "t", &[], "exit 4", opts), 4);
        let u = row(&dir, "t").usage.unwrap();
        if u.capped {
            assert_eq!((u.cap_cpu, u.cap_ram_mb), (Some(1), Some(256)));
            assert!(u.peak_mb.is_some(), "{u:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

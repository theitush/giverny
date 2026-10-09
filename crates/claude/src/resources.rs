//! The machine ledger: what every manager on this machine holds.
//!
//! Before a worker starts, its manager *claims* what the worker needs
//! (`giverny manage claim <task> --cpu 3 --ram 3G --slot cargo:/x/target`) and
//! is answered *granted*, *granted smaller* (less RAM, down to `--min-ram`)
//! or *queued* behind whoever holds what it needs. The answer is a
//! [`Lease`] in one JSON file shared by every session on the machine; it is
//! the "hello" managers say to each other, and the source of truth when
//! they talk or when a worker's commands are capped to its
//! grant.
//!
//! **The file.** `<feed dir>/resources/ledger.json` (`$GIVERNY_LEDGER`
//! overrides), every read-modify-write under an exclusive `flock` on the
//! sibling `ledger.lock`, the write itself atomic (temp name, rename). Its
//! format is [`Ledger`]; `docs/management-panel.md` (**Resources**) describes it
//! for other writers.
//!
//! **Liveness.** A lease or a queued request lives [`TTL_MS`] past its
//! `heartbeat_at`; every `giverny manage` command from its session refreshes
//! it, and every read drops what has expired, so a dead manager frees
//! what it held without anyone cleaning up.
//!
//! **The grant rule** ([`Ledger::try_fit`]): a request fits when
//! - its CPU ≤ the limit − Σ live leases, and ≤ the machine's cores − Σ
//!   leases − the 1-minute load average that the leases do not explain;
//! - its memory fits by **expected peaks**, not by the sum of leases
//!   (giverny#281): every task, running or about to, counts at its expected
//!   peak — the high end of its kind's measured history (p90, as `claim`
//!   summarises it, [`crate::manage_history::expected_peak_mb`]), else its
//!   lease, else the default lease — or at what it really uses now when
//!   that is more ([`MemUse`]). The new task's expected peak must fit under
//!   the limit (the slice's `MemoryMax`) less a headroom
//!   ([`ram_headroom`]), the claudes' own memory in the slice, and every
//!   other task so counted; and under `MemAvailable` less the headroom and
//!   what the running tasks may still grow by before their peaks: how
//!   other programs' load counts;
//! - each GPU it asks for has the VRAM free under that GPU's limit;
//! - none of its slots (`cargo:/path/to/target`, any name) is held: slots
//!   are exclusive.
//!
//! **The queue** is FIFO, a request with a task Priority ahead of one
//! without (`asap` › `high` › `medium` › `low` › none). A request is
//! granted only when it fits in what is left after every request queued
//! ahead of it is set aside ([`Ledger::with_reserved`]): a small task may
//! go past a big one that is waiting, never take what the big one waits
//! for. A queued manager polls by running the same `claim` again,
//! which also keeps its place alive.
//!
//! **Leases are protections.** Since giverny#262 the kernel holds the one
//! ceiling (the slice's `MemoryMax`) and a lease's RAM is its commands'
//! `MemoryLow`. Admitting by expected peaks lets the leases' sum exceed the
//! slice; that is safe because a protection only covers memory in use, so
//! what the kernel is asked to protect is at most Σ min(lease, use) ≤
//! Σ max(expected peak, use), which admission keeps under the limit. Only
//! a task that outgrows its history can push the slice to its ceiling; then
//! the kernel reclaims from whoever is above its protection, and if it
//! must kill, kills a command (never a claude, [`crate::tab_cap`]), which
//! `run` reports and the history learns from.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use giverny_core::limits::{Limits, Load, Machine, Mem, Resolved};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{feed, manage};

/// The ledger format this build reads and writes.
pub const LEDGER_VERSION: u64 = 1;
/// A lease or queued request with no heartbeat for this long is gone.
pub const TTL_MS: u64 = 20 * 60 * 1000;
/// Heartbeats closer together than this are not written: every `giverny
/// manage` command beats, and the ledger need not be rewritten for each.
pub const BEAT_EVERY_MS: u64 = 30 * 1000;
/// Overrides where the ledger lives.
pub const LEDGER_ENV: &str = "GIVERNY_LEDGER";
/// A granted-smaller RAM figure is rounded down to this (MiB).
const RAM_STEP_MB: u64 = 256;

/// Exit codes of `giverny manage claim`.
pub mod exit {
    pub const GRANTED: i32 = 0;
    pub const ERROR: i32 = 1;
    pub const USAGE: i32 = 2;
    pub const GRANTED_SMALLER: i32 = 3;
    pub const QUEUED: i32 = 4;
    /// It could never fit under the limits, so it was not queued.
    pub const REFUSED: i32 = 5;
}

/// `$GIVERNY_LEDGER` when set and non-empty, else
/// `<feed dir>/resources/ledger.json`.
pub fn ledger_path(feed_dir: &Path) -> PathBuf {
    std::env::var_os(LEDGER_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| feed_dir.join("resources").join("ledger.json"))
}

/// RAM kept free of grants on top of what other programs use: 5 % of the
/// machine, at least 1 GiB.
pub fn ram_headroom(machine: &Machine) -> Mem {
    Mem((machine.ram.0 / 20).max(1024))
}

/// Epoch milliseconds, written as RFC 3339 like the feed's stamps.
pub(crate) mod ts {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(ms: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&crate::manage::stamp(*ms))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            N(u64),
            S(String),
        }
        match Raw::deserialize(d)? {
            Raw::N(n) => Ok(n),
            Raw::S(s) => s
                .trim()
                .parse::<jiff::Timestamp>()
                .map(|t| t.as_millisecond().max(0) as u64)
                .map_err(serde::de::Error::custom),
        }
    }
}

/// What a claim asks for.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Request {
    /// Cores.
    pub cpu: u32,
    pub ram_mb: u64,
    /// How many GPUs, each with `vram_mb` free.
    pub gpu: u32,
    pub vram_mb: u64,
    /// Exclusive named slots, e.g. `cargo:/home/me/repo/target`.
    pub slots: Vec<String>,
    /// Grant less RAM than `ram_mb`, down to this, rather than queue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_ram_mb: Option<u64>,
    /// The task's Priority (`asap`, `high`, `medium`, `low`): queued ahead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    /// What tasks like this one peak at (the high end of their history),
    /// MiB: what admission counts it at. `None`: too little history, so it
    /// counts at its RAM (or the default lease when it asks none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_mb: Option<u64>,
}

/// What one session's task holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    /// `<session>:<task>`: one lease per task per session.
    pub id: String,
    /// The Claude session that claimed it — whom to ask for it.
    pub session: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default)]
    pub cpu: u32,
    #[serde(default)]
    pub ram_mb: u64,
    /// The GPUs granted, by index; each holds `vram_mb` of it.
    #[serde(default)]
    pub gpus: Vec<u32>,
    #[serde(default)]
    pub vram_mb: u64,
    #[serde(default)]
    pub slots: Vec<String>,
    /// The expected peak it was admitted at, MiB, when its kind's history
    /// gave one; else it counts at `ram_mb` (or the default lease).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_mb: Option<u64>,
    #[serde(with = "ts")]
    pub granted_at: u64,
    #[serde(with = "ts")]
    pub heartbeat_at: u64,
}

/// A request that did not fit yet, in line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiter {
    pub id: String,
    pub session: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(flatten)]
    pub request: Request,
    #[serde(with = "ts")]
    pub queued_at: u64,
    #[serde(with = "ts")]
    pub heartbeat_at: u64,
}

/// The ledger file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    pub version: u64,
    #[serde(default)]
    pub leases: Vec<Lease>,
    #[serde(default)]
    pub queue: Vec<Waiter>,
}

impl Default for Ledger {
    fn default() -> Self {
        Ledger {
            version: LEDGER_VERSION,
            leases: Vec::new(),
            queue: Vec::new(),
        }
    }
}

/// The machine, its limits, and what it is doing now.
#[derive(Debug, Clone, PartialEq)]
pub struct Capacity {
    pub machine: Machine,
    pub limits: Resolved,
    /// Which limits were `auto` (for `resources` to say so).
    pub configured: Limits,
    pub load: Load,
    /// `[management_panel.lease]`: what a task gets when nothing says otherwise.
    pub default_lease: giverny_core::config::DefaultLease,
    /// What the slice really holds now.
    pub mem_use: MemUse,
}

impl Capacity {
    /// This machine now, under the limits in Giverny's `config.toml`
    /// (`[manager.limits]`), all `auto` when it has none, with what the
    /// running commands of the ledger at `ledger` use.
    pub fn detect(ledger: &Path) -> Result<Capacity, String> {
        let machine = Machine::detect();
        let configured = Limits::load()?;
        Ok(Capacity {
            limits: configured.resolve(&machine),
            configured,
            load: Load::sample(),
            machine,
            default_lease: giverny_core::config::DefaultLease::load(),
            mem_use: MemUse::sample(&crate::run_live::runs_dir(ledger)),
        })
    }

    /// What admission counts `l` at: its expected peak (else its RAM, else
    /// the default lease), or what it uses now when that is more.
    pub fn counted_mb(&self, l: &Lease) -> u64 {
        let expected = l.peak_mb.unwrap_or(if l.ram_mb > 0 {
            l.ram_mb
        } else {
            self.default_lease.ram.0
        });
        expected.max(self.mem_use.of(&l.id))
    }
}

/// Memory the slice's processes cannot give back (no swap): a cgroup's
/// `memory.current` less its page cache (`file` in `memory.stat`), MiB.
/// The page cache is left out because the kernel reclaims it first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemUse {
    /// Each lease's running `giverny manage run` commands, summed, by
    /// lease id (`<session>:<task>`).
    pub by_lease: HashMap<String, u64>,
    /// The rest of the slice: the claudes themselves (and their plain
    /// commands), which no lease counts.
    pub other_mb: u64,
}

impl MemUse {
    pub fn of(&self, lease_id: &str) -> u64 {
        self.by_lease.get(lease_id).copied().unwrap_or(0)
    }

    /// The running commands under `runs_dir` (their scopes' cgroups, from
    /// [`crate::run_live::running`]) and [`crate::tab_cap::SLICE`] now.
    /// Nothing where there is no such cgroup (no user systemd, not Linux).
    pub fn sample(runs_dir: &Path) -> MemUse {
        let mut by_lease: HashMap<String, u64> = HashMap::new();
        let mut in_runs = 0;
        for (session, task, cg) in crate::run_live::running(runs_dir) {
            if let Some(mb) = held_mb(&cg) {
                *by_lease.entry(lease_id(&session, &task)).or_default() += mb;
                in_runs += mb;
            }
        }
        let other_mb = slice_dir()
            .and_then(|d| held_mb(&d))
            .map_or(0, |mb| mb.saturating_sub(in_runs));
        MemUse { by_lease, other_mb }
    }
}

/// `memory.current` − `file` of the cgroup at `dir`, MiB.
fn held_mb(dir: &Path) -> Option<u64> {
    let current: u64 = std::fs::read_to_string(dir.join("memory.current"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let file = std::fs::read_to_string(dir.join("memory.stat"))
        .ok()
        .and_then(|t| stat_field(&t, "file"))
        .unwrap_or(0);
    Some(current.saturating_sub(file) / (1024 * 1024))
}

/// One `memory.stat` field, bytes.
fn stat_field(text: &str, name: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once(' ')?;
        (k == name).then(|| v.trim().parse().ok()).flatten()
    })
}

/// The slice's cgroup under this user's systemd manager, when it exists.
fn slice_dir() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        let d = PathBuf::from(format!(
            "/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/giverny.slice/{}",
            crate::tab_cap::SLICE
        ));
        d.is_dir().then_some(d)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// What is free now, after the leases and other programs' load.
#[derive(Debug, Clone, PartialEq)]
pub struct Free {
    pub cpu: u32,
    pub ram_mb: u64,
    /// `(gpu index, free MiB of VRAM)` for each GPU under the limits.
    pub vram_mb: Vec<(u32, u64)>,
    /// Load average beyond what the leases explain.
    pub foreign_cpu: f64,
    /// RAM grantable by `MemAvailable` alone, when known: less the
    /// headroom and what the running tasks may still grow by.
    pub mem_room_mb: Option<u64>,
    /// What every lease counts at, summed ([`Capacity::counted_mb`]).
    pub counted_mb: u64,
}

/// A fitting grant: the RAM and GPUs it gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fit {
    pub ram_mb: u64,
    pub gpus: Vec<u32>,
    /// What admission counted it at, MiB.
    pub counted_mb: u64,
}

/// The answer to a claim.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Granted(Lease),
    /// Less RAM than asked (`wanted_mb`), at least `--min-ram`.
    GrantedSmaller {
        lease: Lease,
        wanted_mb: u64,
    },
    /// The task already holds this lease (a re-claim heartbeats it).
    Held(Lease),
    /// In line at `position` (1-based); `blockers` are the leases holding
    /// what it needs, `ahead` the queued tasks before it.
    Queued {
        position: usize,
        blockers: Vec<Lease>,
        ahead: Vec<Waiter>,
        short: Vec<String>,
    },
    /// Larger than the limits allow: never queued.
    Refused(String),
}

impl Outcome {
    pub fn exit_code(&self) -> i32 {
        match self {
            Outcome::Granted(_) | Outcome::Held(_) => exit::GRANTED,
            Outcome::GrantedSmaller { .. } => exit::GRANTED_SMALLER,
            Outcome::Queued { .. } => exit::QUEUED,
            Outcome::Refused(_) => exit::REFUSED,
        }
    }
}

fn priority_rank(p: Option<&str>) -> u8 {
    match p.map(|p| p.trim().to_ascii_lowercase()).as_deref() {
        Some("asap" | "urgent" | "p0") => 0,
        Some("high" | "p1") => 1,
        Some("medium" | "normal" | "p2") => 2,
        Some("low" | "p3") => 3,
        _ => 4,
    }
}

/// `<session>:<task>`.
pub fn lease_id(session: &str, task: &str) -> String {
    format!("{session}:{task}")
}

impl Ledger {
    /// Parse a ledger; a missing or unreadable one is empty, a newer
    /// version is an error (never overwritten).
    pub fn parse(bytes: &[u8]) -> Result<Ledger, String> {
        let l: Ledger = serde_json::from_slice(bytes)
            .map_err(|e| format!("the ledger is not readable: {e}"))?;
        if l.version > LEDGER_VERSION {
            return Err(format!(
                "the ledger is version {}; this Giverny reads {LEDGER_VERSION}",
                l.version
            ));
        }
        Ok(l)
    }

    /// Drop every lease and queued request whose heartbeat is older than
    /// [`TTL_MS`]. Returns the expired leases.
    pub fn expire(&mut self, now: u64) -> Vec<Lease> {
        let live = |hb: u64| hb.saturating_add(TTL_MS) > now;
        let (keep, gone): (Vec<_>, Vec<_>) =
            self.leases.drain(..).partition(|l| live(l.heartbeat_at));
        self.leases = keep;
        self.queue.retain(|w| live(w.heartbeat_at));
        gone
    }

    /// Refresh `session`'s leases and queued requests. True when anything
    /// was older than [`BEAT_EVERY_MS`] (and so worth writing).
    pub fn heartbeat(&mut self, session: &str, now: u64) -> bool {
        let mut moved = false;
        let mut beat = |hb: &mut u64| {
            if now.saturating_sub(*hb) >= BEAT_EVERY_MS {
                moved = true;
            }
            *hb = (*hb).max(now);
        };
        for l in self.leases.iter_mut().filter(|l| l.session == session) {
            beat(&mut l.heartbeat_at);
        }
        for w in self.queue.iter_mut().filter(|w| w.session == session) {
            beat(&mut w.heartbeat_at);
        }
        moved
    }

    /// Drop `session`'s leases and queued requests for tasks that have
    /// `landed`: a lease a landing missed would otherwise be renewed by
    /// every heartbeat of a busy session and never expire. The leases
    /// dropped.
    pub fn drop_landed(&mut self, session: &str, landed: &[String]) -> Vec<Lease> {
        let gone_task = |s: &str, t: &str| s == session && landed.iter().any(|k| k == t);
        self.queue.retain(|w| !gone_task(&w.session, &w.task));
        let (gone, keep): (Vec<_>, Vec<_>) = self
            .leases
            .drain(..)
            .partition(|l| gone_task(&l.session, &l.task));
        self.leases = keep;
        gone
    }

    /// The queue in grant order: Priority first, then first come.
    pub fn ordered_queue(&self) -> Vec<&Waiter> {
        let mut q: Vec<&Waiter> = self.queue.iter().collect();
        q.sort_by_key(|w| (priority_rank(w.request.priority.as_deref()), w.queued_at));
        q
    }

    pub fn lease(&self, session: &str, task: &str) -> Option<&Lease> {
        self.leases
            .iter()
            .find(|l| l.session == session && l.task == task)
    }

    /// What is free now under `cap`.
    pub fn free(&self, cap: &Capacity) -> Free {
        let leased_cpu: u32 = self.leases.iter().map(|l| l.cpu).sum();
        let counted_mb: u64 = self.leases.iter().map(|l| cap.counted_mb(l)).sum();
        // What the leases may still grow by before their peaks: not yet
        // taken out of `MemAvailable`.
        let growth_mb: u64 = self
            .leases
            .iter()
            .map(|l| cap.counted_mb(l).saturating_sub(cap.mem_use.of(&l.id)))
            .sum();
        let headroom = ram_headroom(&cap.machine).0;
        let foreign_cpu = cap
            .load
            .load1
            .map(|l| (l - leased_cpu as f64).max(0.0))
            .unwrap_or(0.0);
        let by_limit = cap.limits.cpu_cores.saturating_sub(leased_cpu) as f64;
        let by_machine = cap.machine.cores as f64 - leased_cpu as f64 - foreign_cpu;
        let cpu = by_limit.min(by_machine).floor().max(0.0) as u32;
        let mem_room_mb = cap
            .load
            .mem_available
            .map(|a| a.0.saturating_sub(headroom).saturating_sub(growth_mb));
        let ram_mb = cap
            .limits
            .ram
            .0
            .saturating_sub(headroom)
            .saturating_sub(cap.mem_use.other_mb)
            .saturating_sub(counted_mb)
            .min(mem_room_mb.unwrap_or(u64::MAX));
        let vram_mb = cap
            .limits
            .gpus
            .iter()
            .map(|g| {
                let used: u64 = self
                    .leases
                    .iter()
                    .filter(|l| l.gpus.contains(&g.index))
                    .map(|l| l.vram_mb)
                    .sum();
                (g.index, g.vram.0.saturating_sub(used))
            })
            .collect();
        Free {
            cpu,
            ram_mb,
            vram_mb,
            foreign_cpu,
            mem_room_mb,
            counted_mb,
        }
    }

    /// What `req` counts at when it holds `ram_mb`: its expected peak, else
    /// that RAM, else the default lease — never more than an empty slice
    /// has room for, so a task whose kind once peaked near the limit still
    /// starts on an idle box.
    fn counted_for(cap: &Capacity, req: &Request, ram_mb: u64) -> u64 {
        let expected = req.peak_mb.unwrap_or(if ram_mb > 0 {
            ram_mb
        } else {
            cap.default_lease.ram.0
        });
        expected.min(
            cap.limits
                .ram
                .0
                .saturating_sub(ram_headroom(&cap.machine).0),
        )
    }

    /// Does `req` fit now? `Ok(Fit)` with what it would get, else what is
    /// short (`"cpu"`, `"ram"`, `"gpu"`, `"slot <name>"`).
    pub fn try_fit(&self, cap: &Capacity, req: &Request) -> Result<Fit, Vec<String>> {
        let free = self.free(cap);
        let mut short = Vec::new();
        if req.cpu > free.cpu {
            short.push("cpu".to_string());
        }
        // The lease's RAM is the commands' protection; what must fit is
        // the task's expected peak. A smaller grant helps only a task with
        // no history, which counts at its RAM.
        let ram_mb = if Ledger::counted_for(cap, req, req.ram_mb) <= free.ram_mb {
            req.ram_mb
        } else {
            let floor = req
                .min_ram_mb
                .filter(|_| req.peak_mb.is_none() && req.ram_mb > 0)
                .unwrap_or(u64::MAX);
            let room = free.ram_mb / RAM_STEP_MB * RAM_STEP_MB;
            if floor <= room {
                room
            } else if floor <= free.ram_mb {
                free.ram_mb
            } else {
                short.push("ram".to_string());
                0
            }
        };
        let mut gpus: Vec<(u32, u64)> = free
            .vram_mb
            .iter()
            .copied()
            .filter(|&(_, f)| f >= req.vram_mb)
            .collect();
        gpus.sort_by_key(|&(i, f)| (std::cmp::Reverse(f), i));
        if (gpus.len() as u32) < req.gpu {
            short.push("gpu".to_string());
        }
        for s in &req.slots {
            if self.leases.iter().any(|l| l.slots.contains(s)) {
                short.push(format!("slot {s}"));
            }
        }
        if short.is_empty() {
            Ok(Fit {
                ram_mb,
                counted_mb: Ledger::counted_for(cap, req, ram_mb),
                gpus: gpus
                    .into_iter()
                    .take(req.gpu as usize)
                    .map(|(i, _)| i)
                    .collect(),
            })
        } else {
            Err(short)
        }
    }

    /// This ledger with what the queued requests in `ahead` ask for set
    /// aside, as if granted: a later request may go first only with what
    /// is left after them, so nothing ahead of it is pushed back.
    pub fn with_reserved(&self, cap: &Capacity, ahead: &[Waiter]) -> Ledger {
        let mut v = self.clone();
        v.queue.clear();
        for w in ahead {
            let r = &w.request;
            let gpus = v
                .try_fit(
                    cap,
                    &Request {
                        cpu: 0,
                        ram_mb: 0,
                        slots: vec![],
                        ..r.clone()
                    },
                )
                .map(|f| f.gpus)
                .unwrap_or_default();
            v.leases.push(Lease {
                id: w.id.clone(),
                session: w.session.clone(),
                task: w.task.clone(),
                repo: None,
                cpu: r.cpu,
                ram_mb: r.ram_mb,
                gpus,
                vram_mb: r.vram_mb,
                slots: r.slots.clone(),
                peak_mb: Some(Ledger::counted_for(cap, r, r.ram_mb)),
                granted_at: w.queued_at,
                heartbeat_at: w.heartbeat_at,
            });
        }
        v
    }

    /// Why `req` can never fit under the limits, if it cannot.
    pub fn never_fits(cap: &Capacity, req: &Request) -> Option<String> {
        let l = &cap.limits;
        if req.cpu > l.cpu_cores {
            return Some(format!(
                "{} cores asked; the limit is {}",
                req.cpu, l.cpu_cores
            ));
        }
        let ram = req.min_ram_mb.unwrap_or(req.ram_mb).min(req.ram_mb);
        if ram > l.ram.0 {
            return Some(format!("{} RAM asked; the limit is {}", Mem(ram), l.ram));
        }
        if req.gpu > 0 {
            let fit = l.gpus.iter().filter(|g| g.vram.0 >= req.vram_mb).count() as u32;
            if fit < req.gpu {
                return Some(if l.gpus.is_empty() {
                    "GPU asked; this machine has none (or `gpus = []`)".into()
                } else {
                    format!(
                        "{} GPU(s) with {} asked; {fit} under the limits have that much",
                        req.gpu,
                        Mem(req.vram_mb)
                    )
                });
            }
        }
        None
    }

    /// Claim `req` for `session`'s `task` at `now`. Mutates the ledger: a
    /// grant becomes a lease, a request that waits joins (or keeps) its
    /// place in the queue. Expires and heartbeats first.
    pub fn claim(
        &mut self,
        cap: &Capacity,
        session: &str,
        task: &str,
        repo: Option<&str>,
        req: &Request,
        now: u64,
    ) -> Outcome {
        self.expire(now);
        self.heartbeat(session, now);
        if let Some(l) = self.lease(session, task) {
            return Outcome::Held(l.clone());
        }
        if let Some(why) = Ledger::never_fits(cap, req) {
            self.queue
                .retain(|w| !(w.session == session && w.task == task));
            return Outcome::Refused(why);
        }
        let id = lease_id(session, task);
        // Join the line (or update the place already held) before looking
        // at who is ahead, so a Priority given now counts.
        match self.queue.iter_mut().find(|w| w.id == id) {
            Some(w) => {
                w.request = req.clone();
                w.repo = repo.map(String::from).or(w.repo.take());
            }
            None => self.queue.push(Waiter {
                id: id.clone(),
                session: session.into(),
                task: task.into(),
                repo: repo.map(String::from),
                request: req.clone(),
                queued_at: now,
                heartbeat_at: now,
            }),
        }
        let order = self.ordered_queue();
        let position = order.iter().position(|w| w.id == id).unwrap_or(0);
        let ahead: Vec<Waiter> = order[..position].iter().map(|w| (*w).clone()).collect();
        let fit = self.with_reserved(cap, &ahead).try_fit(cap, req);
        match fit {
            Ok(fit) => {
                self.queue.retain(|w| w.id != id);
                let lease = Lease {
                    id,
                    session: session.into(),
                    task: task.into(),
                    repo: repo.map(String::from),
                    cpu: req.cpu,
                    ram_mb: fit.ram_mb,
                    gpus: fit.gpus,
                    vram_mb: if req.gpu > 0 { req.vram_mb } else { 0 },
                    slots: req.slots.clone(),
                    peak_mb: req.peak_mb.map(|_| fit.counted_mb),
                    granted_at: now,
                    heartbeat_at: now,
                };
                self.leases.push(lease.clone());
                if lease.ram_mb < req.ram_mb {
                    Outcome::GrantedSmaller {
                        lease,
                        wanted_mb: req.ram_mb,
                    }
                } else {
                    Outcome::Granted(lease)
                }
            }
            fit => {
                let short = fit.err().unwrap_or_default();
                let blockers = self
                    .leases
                    .iter()
                    .filter(|l| blocks(l, req, &short))
                    .cloned()
                    .collect();
                Outcome::Queued {
                    position: position + 1,
                    blockers,
                    ahead,
                    short,
                }
            }
        }
    }

    /// Shrink `session`'s lease for `task` in place to the figures given
    /// (`None` keeps that one), when at least one is smaller than held and
    /// none larger: the manager asked to make room. The
    /// lease before and after; `None` when there is no such lease or the
    /// figures do not shrink it. A lease is never grown here: what it gives
    /// up may already be granted to another.
    pub fn shrink(
        &mut self,
        session: &str,
        task: &str,
        cpu: Option<u32>,
        ram_mb: Option<u64>,
        vram_mb: Option<u64>,
        now: u64,
    ) -> Option<(Lease, Lease)> {
        let l = self
            .leases
            .iter_mut()
            .find(|l| l.session == session && l.task == task)?;
        let vram_mb = vram_mb.filter(|_| !l.gpus.is_empty());
        let larger = cpu.is_some_and(|c| c > l.cpu)
            || ram_mb.is_some_and(|r| r > l.ram_mb)
            || vram_mb.is_some_and(|v| v > l.vram_mb);
        let smaller = cpu.is_some_and(|c| c < l.cpu)
            || ram_mb.is_some_and(|r| r < l.ram_mb)
            || vram_mb.is_some_and(|v| v < l.vram_mb);
        if larger || !smaller {
            return None;
        }
        let before = l.clone();
        l.cpu = cpu.unwrap_or(l.cpu);
        l.ram_mb = ram_mb.unwrap_or(l.ram_mb);
        l.vram_mb = vram_mb.unwrap_or(l.vram_mb);
        l.heartbeat_at = l.heartbeat_at.max(now);
        Some((before, l.clone()))
    }

    /// Give each lease admitted before its kind had a history (or before
    /// giverny#281) the expected peak `peak` finds for it now, so it stops
    /// counting at its whole lease. True when any changed.
    pub fn fill_peaks(&mut self, peak: &dyn Fn(&Lease) -> Option<u64>) -> bool {
        let mut changed = false;
        for l in self.leases.iter_mut().filter(|l| l.peak_mb.is_none()) {
            if let Some(p) = peak(l) {
                l.peak_mb = Some(p);
                changed = true;
            }
        }
        changed
    }

    /// Release `session`'s `task`: its lease and any place in the queue.
    /// The lease released, if there was one.
    pub fn release(&mut self, session: &str, task: &str, now: u64) -> Option<Lease> {
        self.expire(now);
        let id = lease_id(session, task);
        self.queue.retain(|w| w.id != id);
        let at = self
            .leases
            .iter()
            .position(|l| l.session == session && l.task == task)?;
        Some(self.leases.remove(at))
    }
}

/// Does lease `l` hold something `req` is short of?
fn blocks(l: &Lease, req: &Request, short: &[String]) -> bool {
    short.iter().any(|s| match s.as_str() {
        "cpu" => l.cpu > 0,
        // Every lease counts at some memory (its peak, RAM or the default).
        "ram" => true,
        "gpu" => !l.gpus.is_empty(),
        s => s.strip_prefix("slot ").is_some_and(|slot| {
            l.slots.iter().any(|x| x == slot) && req.slots.iter().any(|x| x == slot)
        }),
    })
}

/// `2 cpu, 3G, gpu0 8G, slot cargo:/x`.
pub fn describe_held(
    cpu: u32,
    ram_mb: u64,
    gpus: &[u32],
    vram_mb: u64,
    slots: &[String],
) -> String {
    let mut parts = Vec::new();
    if cpu > 0 {
        parts.push(format!("{cpu} cpu"));
    }
    if ram_mb > 0 {
        parts.push(Mem(ram_mb).to_string());
    }
    for g in gpus {
        parts.push(format!("gpu{g} {}", Mem(vram_mb)));
    }
    for s in slots {
        parts.push(format!("slot {s}"));
    }
    if parts.is_empty() {
        "nothing".into()
    } else {
        parts.join(", ")
    }
}

impl Lease {
    pub fn describe(&self) -> String {
        describe_held(self.cpu, self.ram_mb, &self.gpus, self.vram_mb, &self.slots)
    }
}

impl Request {
    pub fn describe(&self) -> String {
        let mut s = describe_held(self.cpu, self.ram_mb, &[], 0, &self.slots);
        if self.gpu > 0 {
            s.push_str(&format!(", {} gpu × {}", self.gpu, Mem(self.vram_mb)));
        }
        s
    }
}

/// How long `session`'s `task` has left by its feed row: a Running row's
/// `started` + `eta_s` − now, in seconds (negative when overdue).
pub fn eta_left_s(feed_dir: &Path, session: &str, task: &str, now: u64) -> Option<i64> {
    let (_, f) = feed::find(feed_dir, session)?;
    let r = f.rows.iter().find(|r| r.key == task)?;
    if r.stage() != feed::Stage::Running {
        return None;
    }
    let upto = r.paused_since_ms.unwrap_or(now);
    let el = upto.saturating_sub(r.started_ms?) / 1000;
    Some(r.eta_s? as i64 - el as i64)
}

/// `session`'s `task`'s time by its feed row: left on a Running row (as
/// [`eta_left_s`]), the estimate of a Planned one — the asker's own figure
/// when weighing a wait.
pub fn eta_or_estimate_s(feed_dir: &Path, session: &str, task: &str, now: u64) -> Option<i64> {
    let (_, f) = feed::find(feed_dir, session)?;
    let r = f.rows.iter().find(|r| r.key == task)?;
    match r.stage() {
        feed::Stage::Running => eta_left_s(feed_dir, session, task, now),
        feed::Stage::Planned => r.eta_s.map(|e| e as i64),
        feed::Stage::Done => None,
    }
}

/// When a queued claim should talk rather than wait: the
/// soonest any holder in `blockers` expects to finish is longer than the
/// asker's own `own_s`, or no holder can say. The holder to ask and the
/// line to print.
pub fn ask_hint(
    task: &str,
    blockers: &[Lease],
    own_s: Option<i64>,
    eta: &dyn Fn(&str, &str) -> Option<i64>,
) -> Option<String> {
    let mut held: Vec<(&Lease, Option<i64>)> = blockers
        .iter()
        .map(|l| (l, eta(&l.session, &l.task)))
        .collect();
    held.sort_by_key(|(_, e)| e.unwrap_or(i64::MAX));
    let (first, wait) = held.first().copied()?;
    let why = match (wait, own_s) {
        (Some(w), Some(o)) if w > o => format!(
            "that wait (~{}) is longer than {task} itself (~{})",
            feed::fmt_span(w.max(0)),
            feed::fmt_span(o.max(0))
        ),
        (None, _) => format!("{} has no ETA to wait out", first.task),
        _ => return None,
    };
    Some(format!(
        "{why}: ask its holder, `giverny manage ask {} \"<why you need it now>\"`",
        first.task
    ))
}

/// `, counted at its kind's peak 1.2G` when admission counted a lease at
/// an expected peak other than its RAM.
fn counted(l: &Lease) -> String {
    match l.peak_mb {
        Some(p) if p != l.ram_mb => format!(", counted at its kind's peak {}", Mem(p)),
        _ => String::new(),
    }
}

/// The line `claim` prints.
pub fn outcome_line(task: &str, o: &Outcome, eta: &dyn Fn(&str, &str) -> Option<i64>) -> String {
    let holder = |l: &Lease| {
        let left = eta(&l.session, &l.task)
            .map(|s| format!("; ~{}", feed::fmt_span(s.max(0))))
            .unwrap_or_default();
        format!("{} ({}{left})", l.task, l.describe())
    };
    match o {
        Outcome::Granted(l) => format!("granted {task}: {}{}", l.describe(), counted(l)),
        Outcome::Held(l) => format!("{task} holds its lease already: {}", l.describe()),
        Outcome::GrantedSmaller { lease, wanted_mb } => format!(
            "granted smaller {task}: {}{} (asked {}, more is not free)",
            lease.describe(),
            counted(lease),
            Mem(*wanted_mb)
        ),
        Outcome::Queued {
            position,
            blockers,
            ahead,
            short,
        } => {
            let mut s = format!("queued #{position}");
            let mut held: Vec<&Lease> = blockers.iter().collect();
            held.sort_by_key(|l| eta(&l.session, &l.task).unwrap_or(i64::MAX));
            if !held.is_empty() {
                let names: Vec<String> = held.iter().take(3).map(|l| holder(l)).collect();
                s.push_str(&format!(" behind {}", names.join(", ")));
                if held.len() > 3 {
                    s.push_str(&format!(" and {} more", held.len() - 3));
                }
            } else if !short.is_empty() {
                s.push_str(&format!(" ({} taken by other programs)", short.join(", ")));
            }
            if let Some(first) = ahead.first() {
                s.push_str(&format!(
                    "; after {} in the queue ({})",
                    first.task,
                    first.request.describe()
                ));
            }
            s.push_str("; re-run the same claim to keep the place and take it when free");
            s
        }
        Outcome::Refused(why) => format!("refused {task}: {why}"),
    }
}

/// The lease as a feed row carries it (`lease` on the row), for the pane.
pub fn row_lease(o: &Outcome, req: &Request) -> Option<Value> {
    let granted = |l: &Lease, state: &str| {
        let mut v = json!({
            "state": state,
            "id": l.id,
            "cpu": l.cpu,
            "ram_mb": l.ram_mb,
            "gpus": l.gpus,
            "vram_mb": l.vram_mb,
            "slots": l.slots,
            "granted_at": manage::stamp(l.granted_at),
        });
        if state == "smaller" {
            v["wanted_ram_mb"] = json!(req.ram_mb);
        }
        v
    };
    match o {
        Outcome::Granted(l) | Outcome::Held(l) => Some(granted(l, "granted")),
        Outcome::GrantedSmaller { lease, .. } => Some(granted(lease, "smaller")),
        Outcome::Queued {
            position,
            blockers,
            ahead,
            ..
        } => Some(json!({
            "state": "queued",
            "position": position,
            "behind": blockers.first().map(|l| l.task.clone())
                .or_else(|| ahead.first().map(|w| w.task.clone())),
            "cpu": req.cpu,
            "ram_mb": req.ram_mb,
            "gpus": [],
            "vram_mb": req.vram_mb,
            "slots": req.slots,
        })),
        Outcome::Refused(_) => None,
    }
}

/// An exclusive lock on the ledger's sibling `ledger.lock`, held until
/// dropped. `flock` on Unix: the kernel frees it when the process dies, so
/// no lock is ever left behind.
pub struct LedgerLock {
    #[cfg(unix)]
    _file: std::fs::File,
    #[cfg(not(unix))]
    _lock: manage::Lock,
}

impl LedgerLock {
    pub fn take(ledger: &Path) -> Result<LedgerLock, String> {
        let dir = ledger.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join("ledger.lock");
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            // SAFETY: a valid open descriptor; flock only blocks.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return Err(format!(
                    "{}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                ));
            }
            Ok(LedgerLock { _file: file })
        }
        #[cfg(not(unix))]
        {
            Ok(LedgerLock {
                _lock: manage::Lock::take(&path.with_extension("json"))?,
            })
        }
    }
}

/// Read the ledger at `path` (empty when missing), let `f` change it under
/// the lock, and write it back when it changed. Expired entries are dropped
/// on every read.
pub fn with_ledger<R>(
    path: &Path,
    now: u64,
    f: impl FnOnce(&mut Ledger) -> R,
) -> Result<R, String> {
    let _lock = LedgerLock::take(path)?;
    let before = match std::fs::read(path) {
        Ok(b) => Some(Ledger::parse(&b)?),
        Err(_) => None,
    };
    let mut l = before.clone().unwrap_or_default();
    l.expire(now);
    let out = f(&mut l);
    let changed = match &before {
        Some(b) => *b != l,
        None => l != Ledger::default(),
    };
    if changed {
        let v = serde_json::to_value(&l).map_err(|e| e.to_string())?;
        manage::write(path, &v).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(out)
}

/// The tasks of `session`'s feed in `feed_dir` that have landed (Done rows).
fn landed_tasks(feed_dir: &Path, session: &str) -> Vec<String> {
    feed::find(feed_dir, session)
        .map(|(_, f)| {
            f.rows
                .into_iter()
                .filter(|r| r.stage() == feed::Stage::Done)
                .map(|r| r.key)
                .collect()
        })
        .unwrap_or_default()
}

/// Every `giverny manage` command's heartbeat for `session`. Touches nothing
/// when there is no ledger yet, and writes only every [`BEAT_EVERY_MS`].
/// With the feed dir, a lease or queued request of a task whose feed row
/// has landed is dropped rather than renewed.
pub fn heartbeat(
    path: &Path,
    feed_dir: Option<&Path>,
    session: &str,
    now: u64,
) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let _lock = LedgerLock::take(path)?;
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(());
    };
    let mut l = Ledger::parse(&bytes)?;
    let before = l.clone();
    l.expire(now);
    let holds = |l: &Ledger| {
        l.leases.iter().any(|x| x.session == session)
            || l.queue.iter().any(|w| w.session == session)
    };
    if let Some(dir) = feed_dir
        && holds(&l)
    {
        l.drop_landed(session, &landed_tasks(dir, session));
    }
    if l.heartbeat(session, now)
        || l.leases.len() != before.leases.len()
        || l.queue.len() != before.queue.len()
    {
        let v = serde_json::to_value(&l).map_err(|e| e.to_string())?;
        manage::write(path, &v).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// Release `session`'s `task` in the ledger at `path` (nothing when there
/// is no ledger). The lease released, if any.
pub fn release_at(
    path: &Path,
    session: &str,
    task: &str,
    now: u64,
) -> Result<Option<Lease>, String> {
    if !path.exists() {
        return Ok(None);
    }
    with_ledger(path, now, |l| l.release(session, task, now))
}

/// Put `lease` on `session`'s feed row for `task` (or take it off, with
/// `None`), when the feed is `giverny manage`'s own and has that row.
pub fn annotate_row(feed_dir: &Path, session: &str, task: &str, lease: Option<Value>) {
    let file = manage::file_for(feed_dir, session);
    if !file.exists() {
        return;
    }
    let Ok(_lock) = manage::Lock::take(&file) else {
        return;
    };
    let Some(mut doc) = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return;
    };
    if manage::writer_of(&doc).is_some_and(|w| w != manage::WRITER) {
        return;
    }
    let Some(row) = doc
        .get_mut("rows")
        .and_then(Value::as_array_mut)
        .and_then(|rows| {
            rows.iter_mut()
                .find(|r| r.get("key").and_then(Value::as_str) == Some(task))
        })
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    match lease {
        Some(v) => row.insert("lease".into(), v),
        None => row.remove("lease"),
    };
    let _ = manage::write(&file, &doc);
}

/// `giverny manage resources`: capacity, limits, foreign load, every lease
/// and the queue.
pub fn report(
    l: &Ledger,
    cap: &Capacity,
    now: u64,
    eta: &dyn Fn(&str, &str) -> Option<i64>,
) -> String {
    let m = &cap.machine;
    let lim = &cap.limits;
    let auto = |is_auto: bool| if is_auto { " (auto)" } else { "" };
    let gpus = |g: &[(u32, Mem)]| {
        if g.is_empty() {
            "no GPU".to_string()
        } else {
            g.iter()
                .map(|(i, v)| format!("gpu{i} {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let free = l.free(cap);
    let mut out = String::new();
    out.push_str(&format!(
        "machine   {} cores, {} RAM, {}\n",
        m.cores,
        m.ram,
        gpus(&m.gpus.iter().map(|g| (g.index, g.vram)).collect::<Vec<_>>())
    ));
    out.push_str(&format!(
        "limits    {} cores{}, {} RAM{}, {}{}\n",
        lim.cpu_cores,
        auto(cap.configured.cpu_cores.get().is_none()),
        lim.ram,
        auto(cap.configured.ram.get().is_none()),
        gpus(
            &lim.gpus
                .iter()
                .map(|g| (g.index, g.vram))
                .collect::<Vec<_>>()
        ),
        auto(cap.configured.gpus.get().is_none()),
    ));
    out.push_str(&format!(
        "default   {} cores, {} RAM a task (Settings → Management panel)\n",
        cap.default_lease.cpu_cores, cap.default_lease.ram
    ));
    out.push_str(&format!(
        "others    load {:.1} beyond the leases; {} available{} (headroom {})\n",
        free.foreign_cpu,
        cap.load
            .mem_available
            .map(|m| m.to_string())
            .unwrap_or_else(|| "?".into()),
        if cap.load.load1.is_none() {
            ", load ?"
        } else {
            ""
        },
        ram_headroom(m),
    ));
    out.push_str(&format!(
        "memory    {} counted of {}: the leases at their expected peaks (or use, \
         or lease), {} for the claudes, {} headroom\n",
        Mem(free.counted_mb),
        lim.ram,
        Mem(cap.mem_use.other_mb),
        ram_headroom(m),
    ));
    out.push_str(&format!(
        "free now  {} cores, {} RAM{}\n",
        free.cpu,
        Mem(free.ram_mb),
        free.vram_mb
            .iter()
            .map(|(i, v)| format!(", gpu{i} {}", Mem(*v)))
            .collect::<String>()
    ));
    let ago = |t: u64| feed::fmt_span((now.saturating_sub(t) / 1000) as i64);
    if l.leases.is_empty() {
        out.push_str("leases    none\n");
    } else {
        out.push_str("leases\n");
        for x in &l.leases {
            let left = eta(&x.session, &x.task)
                .map(|s| format!(", ~{} left", feed::fmt_span(s.max(0))))
                .unwrap_or_default();
            let using = cap.mem_use.of(&x.id);
            let counts = cap.counted_mb(x);
            let mem = if using > 0 {
                format!("  counts {}, using {}", Mem(counts), Mem(using))
            } else {
                format!("  counts {}", Mem(counts))
            };
            out.push_str(&format!(
                "  {:<16} {}{mem}  session {}  beat {} ago{left}\n",
                x.task,
                x.describe(),
                short_session(&x.session),
                ago(x.heartbeat_at),
            ));
        }
    }
    let q = l.ordered_queue();
    if !q.is_empty() {
        out.push_str("queue\n");
        for (i, w) in q.iter().enumerate() {
            out.push_str(&format!(
                "  #{} {:<14} {}  session {}  queued {} ago{}\n",
                i + 1,
                w.task,
                w.request.describe(),
                short_session(&w.session),
                ago(w.queued_at),
                w.request
                    .priority
                    .as_deref()
                    .map(|p| format!(", {p}"))
                    .unwrap_or_default()
            ));
        }
    }
    out
}

fn short_session(s: &str) -> &str {
    s.get(..8).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use giverny_core::limits::{Gpu, GpuLimit};

    const T0: u64 = 1_790_000_000_000;
    const MIN: u64 = 60_000;

    /// 14 cores, 23 G, limits 12 cores / 16 G, nothing else running.
    fn cap() -> Capacity {
        let machine = Machine {
            cores: 14,
            ram: Mem::gb(23),
            gpus: vec![],
        };
        Capacity {
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
            machine,
            default_lease: Default::default(),
            mem_use: Default::default(),
        }
    }

    fn req(cpu: u32, ram_gb: u64) -> Request {
        Request {
            cpu,
            ram_mb: ram_gb * 1024,
            ..Default::default()
        }
    }

    #[test]
    fn two_sessions_past_the_limit_queue_and_a_release_unblocks() {
        let c = cap();
        let mut l = Ledger::default();
        let a = l.claim(&c, "a", "t1", None, &req(6, 7), T0);
        assert!(matches!(a, Outcome::Granted(_)), "{a:?}");
        let b = l.claim(&c, "b", "t2", None, &req(6, 7), T0);
        assert!(matches!(b, Outcome::Granted(_)), "{b:?}");
        // The limit (12 cores, 16 G less 1.15 G headroom) is full: a third
        // waits behind both.
        let q = l.claim(&c, "b", "t3", None, &req(2, 2), T0 + MIN);
        let Outcome::Queued {
            position, blockers, ..
        } = &q
        else {
            panic!("{q:?}")
        };
        assert_eq!(*position, 1);
        assert_eq!(blockers.len(), 2);
        assert_eq!(q.exit_code(), exit::QUEUED);
        // A fourth from session a lines up behind it.
        let q2 = l.claim(&c, "a", "t4", None, &req(1, 1), T0 + 2 * MIN);
        assert!(matches!(q2, Outcome::Queued { position: 2, .. }), "{q2:?}");
        // Once t1 is released, t4 may go first only with what is left after
        // t3's share is set aside (6 cores, 7.85 G free; t3 wants 2 and 2 G).
        assert!(l.release("a", "t1", T0 + 3 * MIN).is_some());
        let big = l.claim(&c, "a", "t4", None, &req(5, 1), T0 + 3 * MIN);
        assert!(
            matches!(big, Outcome::Queued { position: 2, .. }),
            "{big:?}"
        );
        let g = l.claim(&c, "a", "t4", None, &req(1, 1), T0 + 3 * MIN);
        assert!(matches!(g, Outcome::Granted(_)), "{g:?}");
        let g = l.claim(&c, "b", "t3", None, &req(2, 2), T0 + 3 * MIN);
        assert!(matches!(g, Outcome::Granted(_)), "{g:?}");
        assert!(l.queue.is_empty());
        // A re-claim is the same lease.
        let h = l.claim(&c, "a", "t4", None, &req(1, 1), T0 + 4 * MIN);
        assert!(matches!(h, Outcome::Held(_)), "{h:?}");
    }

    #[test]
    fn an_expired_lease_frees_itself_and_a_heartbeat_keeps_one() {
        let c = cap();
        let mut l = Ledger::default();
        l.claim(&c, "a", "big", None, &req(12, 4), T0);
        l.claim(&c, "b", "kept", None, &req(0, 4), T0);
        assert!(matches!(
            l.claim(&c, "c", "x", None, &req(4, 1), T0),
            Outcome::Queued { .. }
        ));
        // Session b keeps beating; a goes silent; c polls.
        for k in 1..=4 {
            l.heartbeat("b", T0 + k * 5 * MIN);
            l.heartbeat("c", T0 + k * 5 * MIN);
        }
        let now = T0 + TTL_MS + MIN;
        let g = l.claim(&c, "c", "x", None, &req(4, 1), now);
        assert!(matches!(g, Outcome::Granted(_)), "{g:?}");
        assert!(l.lease("a", "big").is_none(), "expired");
        assert!(
            l.lease("b", "kept").is_some(),
            "kept alive by its heartbeats"
        );
        // A queued request that stops polling expires too.
        let mut l = Ledger::default();
        l.claim(&c, "a", "big", None, &req(12, 1), T0);
        l.claim(&c, "z", "gone", None, &req(1, 1), T0);
        l.heartbeat("a", T0 + TTL_MS - MIN);
        assert_eq!(l.queue.len(), 1);
        l.expire(T0 + TTL_MS + MIN);
        assert!(l.queue.is_empty());
        assert_eq!(l.leases.len(), 1);
    }

    #[test]
    fn slots_are_exclusive() {
        let c = cap();
        let mut l = Ledger::default();
        let slot = |s: &str| Request {
            cpu: 1,
            slots: vec![s.to_string()],
            ..Default::default()
        };
        assert!(matches!(
            l.claim(&c, "a", "w1", None, &slot("cargo:/t"), T0),
            Outcome::Granted(_)
        ));
        let q = l.claim(&c, "b", "w2", None, &slot("cargo:/t"), T0);
        let Outcome::Queued {
            blockers, short, ..
        } = &q
        else {
            panic!("{q:?}")
        };
        assert_eq!(short, &vec!["slot cargo:/t".to_string()]);
        assert_eq!(blockers[0].task, "w1");
        let line = outcome_line("w2", &q, &|_, t| (t == "w1").then_some(14 * 60));
        assert!(
            line.starts_with("queued #1 behind w1 (1 cpu, slot cargo:/t; ~14m)"),
            "{line}"
        );
        // Another slot is free.
        assert!(matches!(
            l.claim(&c, "c", "w3", None, &slot("cargo:/other"), T0),
            Outcome::Granted(_)
        ));
        l.release("a", "w1", T0 + MIN);
        assert!(matches!(
            l.claim(&c, "b", "w2", None, &slot("cargo:/t"), T0 + MIN),
            Outcome::Granted(_)
        ));
    }

    #[test]
    fn other_programs_shrink_what_is_grantable() {
        let mut c = cap();
        // A browser eats RAM: 5 G available, less 1.15 G headroom.
        c.load.mem_available = Some(Mem::gb(5));
        let mut l = Ledger::default();
        let q = l.claim(&c, "a", "t", None, &req(1, 6), T0);
        assert!(matches!(q, Outcome::Queued { .. }), "{q:?}");
        assert!(outcome_line("t", &q, &|_, _| None).contains("taken by other programs"));
        // With --min-ram it is granted what is there.
        let r = Request {
            min_ram_mb: Some(2048),
            ..req(1, 6)
        };
        let g = l.claim(&c, "a", "t", None, &r, T0);
        let Outcome::GrantedSmaller { lease, wanted_mb } = &g else {
            panic!("{g:?}")
        };
        assert_eq!(*wanted_mb, 6 * 1024);
        assert_eq!(
            lease.ram_mb, 3840,
            "5G − 1.15G headroom, down to 256M steps"
        );
        assert_eq!(g.exit_code(), exit::GRANTED_SMALLER);
        // Load beyond the leases takes cores: 14 − 9 busy = 5 free.
        let mut c = cap();
        c.load.load1 = Some(9.0);
        let l = Ledger::default();
        assert_eq!(l.free(&c).cpu, 5);
        assert!(l.try_fit(&c, &req(6, 1)).is_err());
    }

    /// A lease as a 3G default, its kind peaking at `peak_mb`.
    fn idle(l: &mut Ledger, c: &Capacity, task: &str, peak_mb: Option<u64>) {
        let r = Request {
            peak_mb,
            ..req(1, 3)
        };
        let g = l.claim(c, "other", task, Some("demo"), &r, T0);
        assert!(matches!(g, Outcome::Granted(_)), "{task}: {g:?}");
    }

    /// giverny#281: five 3G leases whose tasks peak at 1.2G leave room on a
    /// 16G limit, though their leases alone add up to 15G.
    #[test]
    fn idle_leases_admit_a_new_task_by_their_expected_peaks() {
        let c = cap();
        let mut l = Ledger::default();
        for t in ["a", "b", "c", "d", "e"] {
            idle(&mut l, &c, t, Some(1229));
        }
        assert_eq!(l.leases.iter().map(|x| x.ram_mb).sum::<u64>(), 15 * 1024);
        let free = l.free(&c);
        assert_eq!(free.counted_mb, 5 * 1229);
        let new = Request {
            peak_mb: Some(1229),
            ..req(1, 3)
        };
        let g = l.claim(&c, "s", "new", Some("demo"), &new, T0);
        let Outcome::Granted(lease) = &g else {
            panic!("{g:?}")
        };
        // The lease is the 3G asked (its protection); it counts at its peak.
        assert_eq!((lease.ram_mb, lease.peak_mb), (3 * 1024, Some(1229)));
        assert_eq!(
            outcome_line("new", &g, &|_, _| None),
            "granted new: 1 cpu, 3G, counted at its kind's peak 1.2G"
        );

        // Leases granted before their kind had a history (or before #281)
        // count at their whole RAM until the history gives them a peak:
        // four take 12G, the claudes 2G, so 0.85G is left.
        let mut c = cap();
        c.mem_use.other_mb = 2048;
        let mut l = Ledger::default();
        for t in ["a", "b", "c", "d"] {
            idle(&mut l, &c, t, None);
        }
        let q = l.claim(&c, "s", "new", Some("demo"), &new, T0);
        assert!(matches!(q, Outcome::Queued { .. }), "{q:?}");
        assert!(l.fill_peaks(&|x| (x.repo.as_deref() == Some("demo")).then_some(1229)));
        assert!(!l.fill_peaks(&|_| Some(1)), "a peak once given stays");
        let g = l.claim(&c, "s", "new", Some("demo"), &new, T0);
        assert!(matches!(g, Outcome::Granted(_)), "{g:?}");
        assert!(l.queue.is_empty());

        // A lease of no RAM and no history counts at the default lease.
        let c = cap();
        let mut l = Ledger::default();
        l.claim(&c, "s", "slot", None, &req(1, 0), T0);
        assert_eq!(l.free(&c).counted_mb, 3 * 1024);
    }

    /// giverny#281: what is really committed still queues a task — peaks
    /// that fill the slice, a task already above its peak, the claudes' own
    /// memory, other programs.
    #[test]
    fn a_really_full_box_still_queues() {
        let new = Request {
            peak_mb: Some(1229),
            ..req(1, 3)
        };
        // Tasks that peak at 3.5G: four fill 16G less the headroom.
        let c = cap();
        let mut l = Ledger::default();
        for t in ["a", "b", "c", "d"] {
            idle(&mut l, &c, t, Some(3584));
        }
        let q = l.claim(&c, "s", "new", None, &new, T0);
        let Outcome::Queued {
            short, blockers, ..
        } = &q
        else {
            panic!("{q:?}")
        };
        assert_eq!(short, &vec!["ram".to_string()]);
        assert_eq!(blockers.len(), 4);

        // Five idle 1.2G peaks leave room, until one runs at 9G: a task
        // above its peak counts at its real use.
        let mut c = cap();
        let mut l = Ledger::default();
        for t in ["a", "b", "c", "d", "e"] {
            idle(&mut l, &c, t, Some(1229));
        }
        assert!(l.try_fit(&c, &new).is_ok());
        c.mem_use.by_lease.insert(lease_id("other", "a"), 9 * 1024);
        assert_eq!(l.free(&c).counted_mb, 4 * 1229 + 9 * 1024);
        assert_eq!(l.try_fit(&c, &new), Err(vec!["ram".to_string()]));
        // A task using less than its peak still counts at its peak.
        c.mem_use.by_lease.insert(lease_id("other", "a"), 100);
        assert!(l.try_fit(&c, &new).is_ok());

        // The claudes themselves hold 8G of the slice.
        let mut c = cap();
        c.mem_use.other_mb = 8 * 1024;
        assert_eq!(l.try_fit(&c, &new), Err(vec!["ram".to_string()]));

        // Other programs leave 5G available: the idle leases may still grow
        // by their peaks (3 × 1.2G), so 5G − 1.15G − 3.6G is too little…
        let mut c = cap();
        c.load.mem_available = Some(Mem::gb(5));
        let mut l = Ledger::default();
        for t in ["a", "b", "c"] {
            idle(&mut l, &c, t, Some(1229));
        }
        assert_eq!(l.try_fit(&c, &new), Err(vec!["ram".to_string()]));
        // …unless they already use what they will peak at.
        for t in ["a", "b", "c"] {
            c.mem_use.by_lease.insert(lease_id("other", t), 1229);
        }
        assert!(l.try_fit(&c, &new).is_ok());
    }

    /// What a run's scope holds is its memory less its page cache.
    #[test]
    fn a_runs_memory_is_read_from_its_scope_without_the_page_cache() {
        let dir = std::env::temp_dir().join(format!("giverny-memuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let runs = dir.join("runs");
        let cg = dir.join("scope");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::create_dir_all(&cg).unwrap();
        std::fs::write(cg.join("memory.current"), format!("{}\n", 3u64 << 30)).unwrap();
        std::fs::write(
            cg.join("memory.stat"),
            format!("anon {}\nfile {}\nkernel 0\n", 2u64 << 30, 1u64 << 30),
        )
        .unwrap();
        assert_eq!(held_mb(&cg), Some(2048));
        let stats = runs.join("1-0.stats");
        std::fs::write(&stats, format!("started\ncgroup {}\n", cg.display())).unwrap();
        let _live = crate::run_live::register(&stats, "demo#7", "s1", T0).unwrap();
        let u = MemUse::sample(&runs);
        assert_eq!(u.of("s1:demo#7"), 2048);
        assert_eq!(u.of("s1:other"), 0);
        assert_eq!(held_mb(&dir.join("none")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn too_big_is_refused_not_queued_and_gpus_degrade_to_none() {
        let c = cap();
        let mut l = Ledger::default();
        let r = l.claim(&c, "a", "huge", None, &req(13, 1), T0);
        assert!(matches!(r, Outcome::Refused(_)), "{r:?}");
        assert_eq!(r.exit_code(), exit::REFUSED);
        let gpu = Request {
            gpu: 1,
            vram_mb: 8 * 1024,
            ..req(1, 1)
        };
        let r = l.claim(&c, "a", "train", None, &gpu, T0);
        let Outcome::Refused(why) = &r else {
            panic!("{r:?}")
        };
        assert!(why.contains("none"), "{why}");
        assert!(l.queue.is_empty());

        // With two GPUs, VRAM is shared per device.
        let mut c = cap();
        c.machine.gpus = vec![Gpu {
            index: 0,
            name: "g".into(),
            vram: Mem::gb(24),
        }];
        c.limits.gpus = vec![GpuLimit {
            index: 0,
            vram: Mem::gb(21),
        }];
        let g = l.claim(&c, "a", "t1", None, &gpu, T0);
        let Outcome::Granted(lease) = &g else {
            panic!("{g:?}")
        };
        assert_eq!(lease.gpus, vec![0]);
        assert!(matches!(
            l.claim(&c, "b", "t2", None, &gpu, T0),
            Outcome::Granted(_)
        ));
        assert!(matches!(
            l.claim(&c, "c", "t3", None, &gpu, T0),
            Outcome::Queued { .. }
        ));
    }

    #[test]
    fn priority_goes_first_in_the_queue() {
        let c = cap();
        let mut l = Ledger::default();
        l.claim(&c, "a", "big", None, &req(12, 1), T0);
        l.claim(&c, "b", "early", None, &req(1, 1), T0);
        let urgent = Request {
            priority: Some("ASAP".into()),
            ..req(1, 1)
        };
        let q = l.claim(&c, "c", "urgent", None, &urgent, T0 + MIN);
        assert!(matches!(q, Outcome::Queued { position: 1, .. }), "{q:?}");
        let order: Vec<&str> = l.ordered_queue().iter().map(|w| w.task.as_str()).collect();
        assert_eq!(order, ["urgent", "early"]);
    }

    #[test]
    fn the_file_round_trips_under_the_lock() {
        let dir = std::env::temp_dir().join(format!("giverny-ledger-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("resources").join("ledger.json");
        let c = cap();
        let out = with_ledger(&path, T0, |l| {
            l.claim(&c, "a", "t", Some("demo"), &req(2, 3), T0)
        })
        .unwrap();
        assert!(matches!(out, Outcome::Granted(_)));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\"granted_at\": \"20"),
            "RFC 3339 stamps: {text}"
        );
        let l = Ledger::parse(text.as_bytes()).unwrap();
        assert_eq!(l.leases[0].ram_mb, 3072);
        assert_eq!(l.leases[0].repo.as_deref(), Some("demo"));
        // Heartbeats beat; a silent ledger expires on the next read.
        heartbeat(&path, None, "a", T0 + 10 * MIN).unwrap();
        let l = Ledger::parse(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(l.leases[0].heartbeat_at, T0 + 10 * MIN);
        let n = with_ledger(&path, T0 + 10 * MIN + TTL_MS, |l| l.leases.len()).unwrap();
        assert_eq!(n, 0);
        assert!(release_at(&path, "a", "t", T0).unwrap().is_none());
        // Many writers at once lose nothing.
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                let c = c.clone();
                std::thread::spawn(move || {
                    with_ledger(&path, T0, |l| {
                        l.claim(&c, &format!("s{i}"), "t", None, &req(1, 1), T0)
                    })
                    .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let l = Ledger::parse(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(l.leases.len(), 8);
        // A newer version is refused, never overwritten.
        std::fs::write(&path, br#"{"version": 99, "leases": []}"#).unwrap();
        assert!(with_ledger(&path, T0, |_| ()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

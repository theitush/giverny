//! The machine ledger: what every manager on this machine holds.
//!
//! Before a worker starts, its manager *claims* what the worker needs
//! (`giverny manage claim <task> --ram 3G --slot cargo:/x/target`) and is
//! answered *granted*, *granted smaller* (less RAM, down to `--min-ram`) or
//! *queued* behind whoever holds what it needs. The answer is a [`Lease`]
//! in one JSON file shared by every session on the machine; it is the
//! "hello" managers say to each other, and the source of truth when they
//! talk or when a worker's commands are capped to its grant.
//!
//! **Reserve RAM; CPU is shared and measured** (giverny#387). A lease holds
//! memory (and GPUs, VRAM, slots), never cores: running out of memory kills
//! a command, running out of CPU only slows everyone, and the kernel
//! already shares the cores — every `run` scope at one `CPUWeight`, the
//! Claude tabs above them, all under the slice's hard `CPUQuota`
//! ([`crate::tab_cap`]). Declared cores only queued workers for cores the
//! kernel would have shared anyway. So no manager says anything about CPU;
//! a claim waits on it only when the CPU has really been saturated for
//! about a minute *and* tasks are already building ([`Ledger::cpu_busy`]).
//!
//! **The file.** `<feed dir>/resources/ledger.json` (`$GIVERNY_LEDGER`
//! overrides), every read-modify-write under an exclusive `flock` on the
//! sibling `ledger.lock`, the write itself atomic (temp name, rename). Its
//! format is [`Ledger`]; `docs/management-panel.md` (**Resources**) describes it
//! for other writers. A `cpu` on a lease or request written before
//! giverny#387 is read and ignored.
//!
//! **Liveness.** A lease or a queued request lives [`TTL_MS`] past its
//! `heartbeat_at`; every `giverny manage` command from its session refreshes
//! it, and every read drops what has expired, so a dead manager frees
//! what it held without anyone cleaning up.
//!
//! **The grant rule** ([`Ledger::try_fit`]): a request fits when
//! - the CPU is not busy ([`Ledger::cpu_busy`]): busy is, on Linux, the
//!   slice's CPU time over about the last minute at [`BUSY_PCT`] of its
//!   quota or more, or its `cpu.pressure` (`some avg60`) at
//!   [`PRESSURE_PCT`]; on macOS (no cgroups) the host's 1-minute load
//!   average at that share of the limit's cores — *and* at
//!   least [`MIN_BUILDERS`] leases have a `run` command going or were
//!   granted in the last [`FRESH_MS`] (their build not yet in the measure).
//!   So one busy build alone never holds the next worker back, nor does
//!   load that is not the workers'; a third worker waits only while two
//!   already fill the CPU ([`CpuBusy`]);
//! - its memory fits by **expected peaks**, not by the sum of leases
//!   (giverny#281): every task, running or about to, counts at its expected
//!   peak — the high end of its kind's measured history (p90, as `claim`
//!   summarises it, [`crate::manage_history::expected_peak_mb`]) but never
//!   above what its lease lets a `run` use, twice its RAM (the scope's
//!   `MemoryHigh`, [`lease_bound_mb`], giverny#455) where `run` holds it
//!   there, and above what a command still running from before a shrink
//!   was started under ([`RunBounds`], giverny#456) — else its lease, else
//!   the default lease — or at what it really uses now when that is more
//!   ([`MemUse`]). So a kind's history only lowers what a lease counts at.
//!   The new task's expected peak must fit under
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
//! granted when it fits in what is left after every request queued ahead
//! of it is set aside ([`Ledger::with_reserved`]) — or by **backfill**
//! (giverny#414): beside those ahead that fit now, when it delays none
//! ahead that do not, by the holders' expected ends ([`Ledger::fit_behind`]).
//! So one big claim at the head no longer stalls the smaller ones behind
//! it, and none takes what the head will need when it can start. A
//! request that fits is never jumped, so nothing starves. A queued manager
//! polls by running the same `claim` again, which also keeps its place
//! alive — but the queue does not wait for it (giverny#429): every ledger
//! write, whoever makes it, grants each waiter that fits for its own
//! manager to take up, and a grant left untaken lapses
//! ([`Ledger::advance`]).
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

use std::collections::{HashMap, HashSet};
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
/// The CPU is saturated when it has been this busy, percent of the
/// limit's cores, over about the last minute (giverny#387): close enough
/// to the quota that a new build would only split the cores further.
pub const BUSY_PCT: u64 = 90;
/// …or when some task in the slice waited for a CPU this much of the last
/// minute (`cpu.pressure`, `some avg60`, percent): work is already queuing
/// for cores, whatever the counters say.
pub const PRESSURE_PCT: f64 = 60.0;
/// A saturated CPU holds a claim back only with at least this many leases
/// building (or about to): one build alone, however busy, never starves
/// the next worker, and load that is not the workers' never holds them.
pub const MIN_BUILDERS: usize = 2;
/// A lease granted this recently counts as building: its command has not
/// started yet, so no measure can see it, and a burst of claims the
/// moment the CPU frees must not all be granted at once.
pub const FRESH_MS: u64 = 2 * 60 * 1000;
/// A session no claude runs (by Claude Code's registry) is gone once it has
/// not beaten for this long (giverny#429): its waiters leave the queue and
/// a grant it never took up is dropped, not put back in line.
pub const GONE_MS: u64 = 5 * 60 * 1000;

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

/// The most a `run` under a lease of `ram_mb` can use (giverny#455): its
/// scope's `MemoryHigh`, twice the lease, above which the kernel reclaims
/// and throttles it ([`crate::manage_run`]; on macOS Giverny kills it there,
/// [`crate::user_cap::Ceiling::for_run`]). A lease of no RAM has no bound.
/// Only where `run` holds a command there ([`RunBounds`], giverny#456).
pub fn lease_bound_mb(ram_mb: u64) -> u64 {
    ram_mb * 2
}

/// What a task holding `ram_mb` is expected to reach: its kind's `peak`,
/// held to `bound` (what its commands can reach, [`RunBounds::bound`]; `None`
/// when nothing holds them) — the history only ever lowers what a lease
/// counts at, never past what it can use — else that RAM, else
/// `default_mb`.
fn expected_within(peak: Option<u64>, bound: Option<u64>, ram_mb: u64, default_mb: u64) -> u64 {
    match (peak, ram_mb) {
        (Some(p), _) => bound.map_or(p, |b| p.min(b)),
        (None, 0) => default_mb,
        (None, r) => r,
    }
}

/// `<ledger dir>/run-caps`: what the last `run` here did with its lease —
/// `capped` (a scope's `MemoryHigh`, or Giverny's own kill, at
/// [`lease_bound_mb`]) or `advisory` (nothing held it) — so the ledger
/// knows without asking systemd on every call (giverny#456).
pub fn run_caps_path(ledger: &Path) -> PathBuf {
    ledger.with_file_name("run-caps")
}

/// What the leases' commands can reach (giverny#456): where `run` holds a
/// command to [`lease_bound_mb`], and what each command running now was
/// started held to. A lease counts at no less than what its commands can
/// really use: nothing bounds it where `run` is advisory (Linux without a
/// user systemd, Windows), and a command started before its lease shrank
/// (`claim --ram <less>`) keeps the old scope's `MemoryHigh`, so the lease
/// counts at that until the command ends. Lowering the live scopes'
/// `MemoryHigh` instead would throttle a build mid-way, needs systemd (on
/// macOS the bound lives inside the `run` process), and can fail; counting
/// the old bound only costs the room the shrink would have freed, for as
/// long as the old command runs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunBounds {
    /// `run` holds a command to [`lease_bound_mb`] here: under a systemd
    /// scope, or by Giverny itself (macOS, [`crate::user_cap`]). Read from
    /// [`run_caps_path`], else probed as `run` probes.
    pub caps: bool,
    /// Each lease with a `run` going, by id: the most any of its running
    /// commands was started held to — `None` when one is held to nothing.
    pub live: HashMap<String, Option<u64>>,
}

impl RunBounds {
    /// This machine's, beside the ledger at `ledger`.
    pub fn detect(ledger: &Path) -> RunBounds {
        let caps = match std::fs::read_to_string(run_caps_path(ledger)) {
            Ok(t) => t.trim() == "capped",
            Err(_) => crate::manage_run::caps_here(),
        };
        RunBounds {
            caps,
            live: crate::run_live::running_bounds(&crate::run_live::runs_dir(ledger)),
        }
    }

    /// The most a lease of `ram_mb` (with id `id`, when it is one) lets its
    /// commands use: twice it where `run` caps, and at least what a running
    /// command was started under. `None`: unbounded (no RAM, `run`
    /// advisory, or a running command held to nothing).
    pub fn bound(&self, id: Option<&str>, ram_mb: u64) -> Option<u64> {
        if ram_mb == 0 || !self.caps {
            return None;
        }
        let new = lease_bound_mb(ram_mb);
        match id.and_then(|id| self.live.get(id)) {
            None => Some(new),
            Some(live) => live.map(|b| b.max(new)),
        }
    }
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

/// What a claim asks for: memory, GPUs and slots — never cores
/// (giverny#387; a `cpu` in an older ledger is ignored).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Request {
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

/// What one session's task holds: memory, GPUs and slots, never cores
/// (giverny#387; a `cpu` in an older ledger is ignored).
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
    /// Granted on another session's ledger call (giverny#429) and not yet
    /// taken up by its own manager: what to put back in line should it lapse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncollected: Option<Uncollected>,
}

/// A grant its manager has not taken up yet ([`Ledger::advance`]): the
/// request it was granted from and its place in the queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Uncollected {
    #[serde(with = "ts")]
    pub queued_at: u64,
    pub request: Request,
    /// Its manager's hook has said so ([`news`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub told: bool,
}

/// A grant that lapsed untaken ([`Ledger::advance`]): the waiter keeps its
/// place, but nothing is set aside for it and it is not granted again until
/// its manager claims again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lapsed {
    #[serde(with = "ts")]
    pub at: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub told: bool,
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
    /// Its grant lapsed untaken: dormant until its manager claims again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lapsed: Option<Lapsed>,
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
    /// What the leases' running commands use now (shown, never admitted on).
    pub cpu_use: CpuUse,
    /// How busy the CPU has been lately, and who is building.
    pub cpu: CpuBusy,
    /// The Claude sessions running now, by Claude Code's registry; `None`
    /// when that is not known (no registry read, none found): then only a
    /// heartbeat's [`TTL_MS`] says a session is gone.
    pub live_sessions: Option<HashSet<String>>,
    /// What the leases' commands can reach (giverny#456).
    pub run_bounds: RunBounds,
}

impl Capacity {
    /// This machine now, under the limits in Giverny's `config.toml`
    /// (`[manager.limits]`), all `auto` when it has none, with what the
    /// running commands of the ledger at `ledger` use.
    pub fn detect(ledger: &Path) -> Result<Capacity, String> {
        let machine = Machine::detect();
        let configured = Limits::load()?;
        let load = Load::sample();
        let now = manage::now_ms();
        let runs = crate::run_live::runs_dir(ledger);
        Ok(Capacity {
            limits: configured.resolve(&machine),
            configured,
            cpu: CpuBusy::sample(ledger, &load, now),
            load,
            machine,
            default_lease: giverny_core::config::DefaultLease::load(),
            mem_use: MemUse::sample(&runs),
            cpu_use: CpuUse::sample(&runs, now),
            live_sessions: live_sessions(),
            run_bounds: RunBounds::detect(ledger),
        })
    }

    /// Is `session` gone: no claude runs it, and it last beat at
    /// `last_beat`, [`GONE_MS`] or more before `now`? Never when the
    /// registry says nothing.
    pub fn gone(&self, session: &str, last_beat: u64, now: u64) -> bool {
        self.live_sessions
            .as_ref()
            .is_some_and(|live| !live.contains(session))
            && now.saturating_sub(last_beat) >= GONE_MS
    }

    /// How long a grant waits for its manager ([`Ledger::advance`]), ms.
    pub fn uncollected_ms(&self) -> u64 {
        self.default_lease.uncollected_min.max(1) * 60 * 1000
    }

    /// What admission counts `l` at: its expected peak, never above what
    /// its commands can reach ([`RunBounds::bound`], giverny#455/#456) —
    /// else its RAM, else the default lease — or what it uses now when that
    /// is more.
    pub fn counted_mb(&self, l: &Lease) -> u64 {
        let bound = self.run_bounds.bound(Some(&l.id), l.ram_mb);
        expected_within(l.peak_mb, bound, l.ram_mb, self.default_lease.ram.0)
            .max(self.mem_use.of(&l.id))
    }
}

/// How busy the CPU has been over about the last minute, and which leases
/// are building (giverny#387): what a claim waits on instead of declared
/// cores. Measured, never declared.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CpuBusy {
    /// The slice's cores busy on average, thousandths: its `cpu.stat`
    /// `usage_usec` over the last minute or so, from the samples kept
    /// beside the ledger ([`slice_busy_m`]). `None` without a slice (macOS,
    /// no user systemd) or before a sample is old enough.
    pub slice_m: Option<u64>,
    /// macOS only, which has no slice: the host's 1-minute load average,
    /// thousandths of a core. The supported platforms are Linux with
    /// systemd (WSL too) and macOS; elsewhere nothing measures the CPU and
    /// a claim never waits on it.
    pub load_m: Option<u64>,
    /// The slice's `cpu.pressure` `some avg60`, percent.
    pub pressure: Option<f64>,
    /// Leases with a `giverny manage run` command going now, by id.
    pub running: HashSet<String>,
    /// When it was sampled, epoch ms: what [`FRESH_MS`] is counted from.
    pub at_ms: u64,
}

impl CpuBusy {
    /// This machine now: the slice's use and pressure where it has one,
    /// the host's load average on macOS, the `run` commands going under the
    /// ledger at `ledger`.
    pub fn sample(ledger: &Path, load: &Load, now: u64) -> CpuBusy {
        let slice = slice_dir();
        let slice_m = slice.as_ref().and_then(|d| {
            let usec = std::fs::read_to_string(d.join("cpu.stat"))
                .ok()
                .and_then(|t| stat_field(&t, "usage_usec"))?;
            slice_busy_m(&cpu_samples_path(ledger), now, usec)
        });
        let pressure = slice.as_ref().and_then(|d| {
            std::fs::read_to_string(d.join("cpu.pressure"))
                .ok()
                .and_then(|t| some_avg60(&t))
        });
        CpuBusy {
            slice_m,
            load_m: load
                .load1
                .filter(|_| cfg!(target_os = "macos"))
                .map(|l| (l.max(0.0) * 1000.0).round() as u64),
            pressure,
            running: crate::run_live::running_leases(&crate::run_live::runs_dir(ledger)),
            at_ms: now,
        }
    }

    /// The busy figure `resources` and a queued claim show, thousandths of
    /// a core: the slice's own where it is known, else (macOS) the load
    /// average.
    pub fn busy_m(&self) -> Option<u64> {
        self.slice_m.or(self.load_m)
    }

    /// Saturated over about the last minute, under limits of `limit`
    /// cores: what measures it ([`CpuBusy::busy_m`]) at [`BUSY_PCT`] of
    /// the limit, or the slice's pressure at [`PRESSURE_PCT`]. Nothing
    /// measured is never saturated.
    pub fn saturated(&self, limit: u32) -> bool {
        let by_use = self
            .busy_m()
            .is_some_and(|m| limit > 0 && m * 100 >= u64::from(limit) * 1000 * BUSY_PCT);
        let by_pressure = self.pressure.is_some_and(|p| p >= PRESSURE_PCT);
        by_use || by_pressure
    }

    /// `6.3 of 8 busy (giverny-claude.slice, last minute; pressure 12%)`,
    /// `2.5 of 12 busy (1-minute load average)` on macOS, or `? of 8 busy`
    /// when nothing measures it (yet).
    pub fn describe(&self, limit: u32) -> String {
        let busy = self.busy_m().map(fmt_cores).unwrap_or_else(|| "?".into());
        let mut why = Vec::new();
        if self.slice_m.is_some() {
            why.push(format!("{}, last minute", crate::tab_cap::SLICE));
        } else if self.load_m.is_some() {
            why.push("1-minute load average".into());
        }
        if let Some(p) = self.pressure {
            why.push(format!("pressure {p:.0}%"));
        }
        let mut s = format!("{busy} of {limit} busy");
        if !why.is_empty() {
            s.push_str(&format!(" ({})", why.join("; ")));
        }
        s
    }
}

/// The sessions Claude Code's registry has running, in every config dir
/// this user's claudes use; `None` when it lists none (nothing to go by).
fn live_sessions() -> Option<HashSet<String>> {
    let w = crate::reach_guard::System::new(PathBuf::new());
    let live: HashSet<String> = crate::reach_guard::World::sessions(&w)
        .into_iter()
        .map(|(_, s, _)| s)
        .collect();
    (!live.is_empty()).then_some(live)
}

/// `<ledger dir>/cpu-samples`: the slice's CPU counter as seen by recent
/// `claim`s and `resources`, one `<epoch ms> <usage_usec>` a line.
pub fn cpu_samples_path(ledger: &Path) -> PathBuf {
    ledger
        .parent()
        .unwrap_or(Path::new("."))
        .join("cpu-samples")
}

/// A sample younger than this is not a minute's measure yet (ms).
const SAMPLE_MIN_MS: u64 = 20 * 1000;
/// A new sample is kept when the newest is this old (ms).
const SAMPLE_EVERY_MS: u64 = 30 * 1000;
/// The window: the oldest sample is dropped once the next is this old.
const SAMPLE_WINDOW_MS: u64 = 60 * 1000;
/// A sample older than this says nothing about now (ms).
const SAMPLE_MAX_MS: u64 = 5 * 60 * 1000;

/// The slice's cores busy since the oldest sample in `samples` that is at
/// least [`SAMPLE_MIN_MS`] old, thousandths, with `usec` its counter at
/// `now`; and `samples` kept to about a minute's window. Samples from the
/// future, older than [`SAMPLE_MAX_MS`] or above the counter (a reset
/// slice) are dropped.
pub fn busy_from_samples(samples: &mut Vec<(u64, u64)>, now: u64, usec: u64) -> Option<u64> {
    samples.retain(|&(at, u)| at <= now && now - at <= SAMPLE_MAX_MS && u <= usec);
    samples.sort_unstable();
    // µs of CPU per ms of wall time is thousandths of a core.
    let busy = samples
        .iter()
        .find(|&&(at, _)| now - at >= SAMPLE_MIN_MS)
        .map(|&(at, u)| (usec - u) / (now - at));
    if samples
        .last()
        .is_none_or(|&(at, _)| now - at >= SAMPLE_EVERY_MS)
    {
        samples.push((now, usec));
    }
    while samples.len() > 1 && now - samples[1].0 >= SAMPLE_WINDOW_MS {
        samples.remove(0);
    }
    busy
}

/// [`busy_from_samples`] over the samples file at `path`, written back
/// (atomically; two writers at once lose a sample, never the file).
fn slice_busy_m(path: &Path, now: u64, usec: u64) -> Option<u64> {
    let mut samples: Vec<(u64, u64)> = std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            Some((w.next()?.parse().ok()?, w.next()?.parse().ok()?))
        })
        .collect();
    let busy = busy_from_samples(&mut samples, now, usec);
    let text: String = samples.iter().map(|(a, u)| format!("{a} {u}\n")).collect();
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
    busy
}

/// `some avg60=…` of a `cpu.pressure` file, percent.
pub fn some_avg60(text: &str) -> Option<f64> {
    text.lines()
        .find_map(|l| l.strip_prefix("some "))?
        .split_whitespace()
        .find_map(|f| f.strip_prefix("avg60="))?
        .parse()
        .ok()
}

/// The cores the leases' running `giverny manage run` commands use now,
/// thousandths, by lease id: each scope's `cpu.stat` `usage_usec` over the
/// time it has run. An average since the command started, shown beside a
/// lease by `resources`; admission never counts it (giverny#387).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CpuUse {
    pub by_lease: HashMap<String, u64>,
}

impl CpuUse {
    pub fn of(&self, lease_id: &str) -> u64 {
        self.by_lease.get(lease_id).copied().unwrap_or(0)
    }

    /// The running commands under `runs_dir` at `now`. Nothing where they
    /// have no cgroup of their own, or say no start.
    pub fn sample(runs_dir: &Path, now: u64) -> CpuUse {
        let mut by_lease: HashMap<String, u64> = HashMap::new();
        for (session, task, cg, started) in crate::run_live::running_since(runs_dir) {
            let Some(started) = started else { continue };
            if let Some(m) = cpu_m(&cg, now.saturating_sub(started)) {
                *by_lease.entry(lease_id(&session, &task)).or_default() += m;
            }
        }
        CpuUse { by_lease }
    }
}

/// The cores the cgroup at `dir` used on average over `ran_ms`, thousandths.
fn cpu_m(dir: &Path, ran_ms: u64) -> Option<u64> {
    if ran_ms == 0 {
        return None;
    }
    let usec = std::fs::read_to_string(dir.join("cpu.stat"))
        .ok()
        .and_then(|t| stat_field(&t, "usage_usec"))?;
    // µs of CPU per ms of wall time is thousandths of a core.
    Some(usec / ran_ms)
}

/// `1.5` for 1500 thousandths of a core.
pub fn fmt_cores(m: u64) -> String {
    format!("{:.1}", m as f64 / 1000.0)
}

/// Memory the slice's processes cannot give back (no swap): what a
/// cgroup's `memory.stat` says will not come back on demand ([`MemSplit`]),
/// MiB. The page cache and reclaimable slab are left out because the
/// kernel reclaims them first (giverny#424).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemUse {
    /// Each lease's running `giverny manage run` commands, summed, by
    /// lease id (`<session>:<task>`).
    pub by_lease: HashMap<String, u64>,
    /// The rest of the slice: the claudes themselves (and their plain
    /// commands), which no lease counts.
    pub other_mb: u64,
    /// What the whole slice is made of, for `resources` to show.
    pub slice: Option<MemSplit>,
    /// `run` scopes whose `run` is gone but whose command lives on, with
    /// what each holds, MiB: in `other_mb`, no lease counts them, and
    /// nothing will stop them (giverny#424).
    pub orphans: Vec<(String, u64)>,
}

impl MemUse {
    pub fn of(&self, lease_id: &str) -> u64 {
        self.by_lease.get(lease_id).copied().unwrap_or(0)
    }

    /// The running commands under `runs_dir` (their scopes' cgroups, from
    /// [`crate::run_live::running`]) and [`crate::tab_cap::SLICE`] now.
    /// Nothing where there is no such cgroup (no user systemd, not Linux).
    pub fn sample(runs_dir: &Path) -> MemUse {
        Self::sample_in(runs_dir, slice_dir().as_deref())
    }

    fn sample_in(runs_dir: &Path, slice: Option<&Path>) -> MemUse {
        let mut by_lease: HashMap<String, u64> = HashMap::new();
        let mut in_runs = 0;
        for (session, task, cg) in crate::run_live::running(runs_dir) {
            if let Some(mb) = held_mb(&cg) {
                *by_lease.entry(lease_id(&session, &task)).or_default() += mb;
                in_runs += mb;
            }
        }
        let orphans = slice.map(orphan_scopes).unwrap_or_default();
        let slice = slice.and_then(MemSplit::read);
        let other_mb = slice.map_or(0, |s| s.held_mb().saturating_sub(in_runs));
        MemUse {
            by_lease,
            other_mb,
            slice,
            orphans,
        }
    }

    /// What the orphaned `run` scopes hold, MiB.
    pub fn orphans_mb(&self) -> u64 {
        self.orphans.iter().map(|(_, mb)| mb).sum()
    }
}

/// The `giverny-run-<task>-<pid>.scope` cgroups under `slice` whose `run`
/// process (`<pid>`) is gone, biggest first, with what each holds, MiB.
fn orphan_scopes(slice: &Path) -> Vec<(String, u64)> {
    let Ok(rd) = std::fs::read_dir(slice) else {
        return Vec::new();
    };
    let mut out: Vec<(String, u64)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let unit = name.strip_suffix(".scope")?;
            let pid: u32 = unit
                .strip_prefix("giverny-run-")?
                .rsplit_once('-')?
                .1
                .parse()
                .ok()?;
            if crate::run_live::alive(pid) {
                return None;
            }
            Some((unit.to_string(), held_mb(&e.path())?))
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// What a cgroup's memory is made of, bytes: `memory.current` and the
/// `memory.stat` fields that say what comes back on demand (giverny#424).
/// The slice sits at its `MemoryMax` full of dentry/inode slab and page
/// cache nobody holds; counting that as the claudes' turned ~2G of
/// processes into 10G and granted nothing on an idle box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemSplit {
    pub current: u64,
    pub anon: u64,
    /// Page cache, tmpfs and shared memory included.
    pub file: u64,
    /// tmpfs and shared memory: in `file`, but it goes only when its
    /// files do.
    pub shmem: u64,
    /// All kernel memory, both slabs included (`kernel`; on kernels before
    /// 5.18 the sum of its parts).
    pub kernel: u64,
    pub slab_reclaimable: u64,
    pub slab_unreclaimable: u64,
    pub sock: u64,
}

impl MemSplit {
    /// The cgroup at `dir`, `None` without its `memory.current`.
    pub fn read(dir: &Path) -> Option<MemSplit> {
        let current: u64 = std::fs::read_to_string(dir.join("memory.current"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        let stat = std::fs::read_to_string(dir.join("memory.stat")).unwrap_or_default();
        let f = |name| stat_field(&stat, name).unwrap_or(0);
        let kernel = stat_field(&stat, "kernel").unwrap_or_else(|| {
            [
                "kernel_stack",
                "pagetables",
                "percpu",
                "slab_reclaimable",
                "slab_unreclaimable",
            ]
            .into_iter()
            .map(f)
            .sum()
        });
        Some(MemSplit {
            current,
            anon: f("anon"),
            file: f("file"),
            shmem: f("shmem"),
            kernel,
            slab_reclaimable: f("slab_reclaimable"),
            slab_unreclaimable: f("slab_unreclaimable"),
            sock: f("sock"),
        })
    }

    /// What will not come back on demand, bytes: anon, shmem, kernel
    /// memory but its reclaimable slab, and socket buffers — never more
    /// than `memory.current`.
    pub fn held(&self) -> u64 {
        (self.anon + self.shmem + self.kernel.saturating_sub(self.slab_reclaimable) + self.sock)
            .min(self.current)
    }

    pub fn held_mb(&self) -> u64 {
        self.held() / (1024 * 1024)
    }

    /// One line for `resources`: what is held, of what, and what comes back.
    pub fn describe(&self) -> String {
        let m = |b: u64| Mem(b / (1024 * 1024));
        format!(
            "{} held (anon {}, shmem {}, kernel {}, sock {}) of {} in the slice; \
             comes back: cache {}, reclaimable slab {}",
            m(self.held()),
            m(self.anon),
            m(self.shmem),
            m(self.kernel.saturating_sub(self.slab_reclaimable)),
            m(self.sock),
            m(self.current),
            m(self.file.saturating_sub(self.shmem)),
            m(self.slab_reclaimable),
        )
    }
}

/// What the cgroup at `dir` holds ([`MemSplit::held`]), MiB.
fn held_mb(dir: &Path) -> Option<u64> {
    MemSplit::read(dir).map(|s| s.held_mb())
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

/// What is free now, after the leases and other programs' load. No
/// cores: CPU is shared, not granted ([`Ledger::cpu_busy`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Free {
    pub ram_mb: u64,
    /// `(gpu index, free MiB of VRAM)` for each GPU under the limits.
    pub vram_mb: Vec<(u32, u64)>,
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

/// What [`Ledger::advance`] did.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Advanced {
    /// Granted for managers not in this call, uncollected.
    pub granted: Vec<Lease>,
    /// Tasks whose grant lapsed untaken, back in line.
    pub lapsed: Vec<String>,
    /// Lease and queue ids dropped with their gone sessions.
    pub dropped: Vec<String>,
}

impl Advanced {
    pub fn moved(&self) -> bool {
        !(self.granted.is_empty() && self.lapsed.is_empty() && self.dropped.is_empty())
    }
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
    /// Granted ahead of `past`, a request queued before it that does not
    /// fit yet, which this grant does not delay ([`Ledger::fit_behind`]);
    /// `wanted_mb` is the RAM asked (more than the lease's when smaller).
    Backfilled {
        lease: Lease,
        wanted_mb: u64,
        past: String,
    },
    /// The task already holds this lease (a re-claim heartbeats it).
    Held(Lease),
    /// In line at `position` (1-based); `blockers` are the leases holding
    /// what it needs, `ahead` the queued tasks before it, `short` what it
    /// is short of, `behind` who is really in the way ([`in_the_way`], or
    /// the waiter a backfill would delay), `only_queue` true when it would
    /// fit were nobody queued ahead, `request` what it asked, and `cpu` how
    /// busy the CPU is when that is what it waits on ([`CpuBusy::describe`]).
    Queued {
        position: usize,
        blockers: Vec<Lease>,
        ahead: Vec<Waiter>,
        short: Vec<String>,
        behind: Option<String>,
        only_queue: bool,
        request: Request,
        cpu: Option<String>,
    },
    /// Larger than the limits allow: never queued.
    Refused(String),
}

impl Outcome {
    pub fn exit_code(&self) -> i32 {
        match self {
            Outcome::Granted(_) | Outcome::Held(_) => exit::GRANTED,
            Outcome::GrantedSmaller { .. } => exit::GRANTED_SMALLER,
            Outcome::Backfilled {
                lease, wanted_mb, ..
            } => {
                if lease.ram_mb < *wanted_mb {
                    exit::GRANTED_SMALLER
                } else {
                    exit::GRANTED
                }
            }
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

    /// The leases building now or about to (giverny#387): a `run` command
    /// going, or granted in the last [`FRESH_MS`] — its build not started,
    /// so no measure sees it yet.
    pub fn builders(&self, cap: &Capacity) -> Vec<&Lease> {
        self.leases
            .iter()
            .filter(|l| {
                cap.cpu.running.contains(&l.id)
                    || cap.cpu.at_ms.saturating_sub(l.granted_at) < FRESH_MS
            })
            .collect()
    }

    /// Is the CPU too busy for one more worker? Saturated over about the
    /// last minute ([`CpuBusy::saturated`]) and at least [`MIN_BUILDERS`]
    /// leases building ([`Ledger::builders`]). One busy build alone never
    /// holds the next worker back, nor does load that is not the workers':
    /// a claim waits only while workers already fill the CPU, so a new
    /// build would only split the cores further.
    pub fn cpu_busy(&self, cap: &Capacity) -> bool {
        cap.cpu.saturated(cap.limits.cpu_cores) && self.builders(cap).len() >= MIN_BUILDERS
    }

    /// What is free now under `cap`.
    pub fn free(&self, cap: &Capacity) -> Free {
        let counted_mb: u64 = self.leases.iter().map(|l| cap.counted_mb(l)).sum();
        // What the leases may still grow by before their peaks: not yet
        // taken out of `MemAvailable`.
        let growth_mb: u64 = self
            .leases
            .iter()
            .map(|l| cap.counted_mb(l).saturating_sub(cap.mem_use.of(&l.id)))
            .sum();
        let headroom = ram_headroom(&cap.machine).0;
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
            ram_mb,
            vram_mb,
            mem_room_mb,
            counted_mb,
        }
    }

    /// What `req` counts at when it holds `ram_mb`: its expected peak, held
    /// to what that lease lets it use ([`RunBounds::bound`]), else that RAM,
    /// else the default lease — never more than an empty slice has room
    /// for, so a task whose kind once peaked near the limit still starts on
    /// an idle box.
    fn counted_for(cap: &Capacity, req: &Request, ram_mb: u64) -> u64 {
        let bound = cap.run_bounds.bound(None, ram_mb);
        expected_within(req.peak_mb, bound, ram_mb, cap.default_lease.ram.0).min(
            cap.limits
                .ram
                .0
                .saturating_sub(ram_headroom(&cap.machine).0),
        )
    }

    /// Does `req` fit now? `Ok(Fit)` with what it would get, else what is
    /// short (`"cpu"` — the CPU is busy, [`Ledger::cpu_busy`] — `"ram"`,
    /// `"gpu"`, `"slot <name>"`).
    pub fn try_fit(&self, cap: &Capacity, req: &Request) -> Result<Fit, Vec<String>> {
        let free = self.free(cap);
        let mut short = Vec::new();
        if self.cpu_busy(cap) {
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
                ram_mb: r.ram_mb,
                gpus,
                vram_mb: r.vram_mb,
                slots: r.slots.clone(),
                peak_mb: Some(Ledger::counted_for(cap, r, r.ram_mb)),
                // About to start: it counts as building, as a fresh grant.
                granted_at: cap.cpu.at_ms,
                heartbeat_at: w.heartbeat_at,
                uncollected: None,
            });
        }
        v
    }

    /// Why `req` can never fit under the limits, if it cannot.
    pub fn never_fits(cap: &Capacity, req: &Request) -> Option<String> {
        let l = &cap.limits;
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

    /// Claim `req` for `session`'s `task` at `now`, knowing no task's time
    /// left: [`Ledger::claim_timed`] with every time unknown, so a backfill
    /// is granted only beside what is waiting, never before it.
    pub fn claim(
        &mut self,
        cap: &Capacity,
        session: &str,
        task: &str,
        repo: Option<&str>,
        req: &Request,
        now: u64,
    ) -> Outcome {
        self.claim_timed(cap, session, task, repo, req, now, &|_, _| None)
    }

    /// Claim `req` for `session`'s `task` at `now`. Mutates the ledger: a
    /// grant becomes a lease, a request that waits joins (or keeps) its
    /// place in the queue. Expires and heartbeats first. `left` is a task's
    /// expected time left, seconds, by `(session, task)` — a running one's
    /// ETA, a planned one's estimate ([`eta_or_estimate_s`]): what a
    /// backfill is weighed by ([`Ledger::fit_behind`]).
    #[allow(clippy::too_many_arguments)]
    pub fn claim_timed(
        &mut self,
        cap: &Capacity,
        session: &str,
        task: &str,
        repo: Option<&str>,
        req: &Request,
        now: u64,
        left: &dyn Fn(&str, &str) -> Option<i64>,
    ) -> Outcome {
        self.expire(now);
        self.heartbeat(session, now);
        // A grant made for it on another session's call: taken up now, as
        // if granted this moment (giverny#429).
        if self.collect(session, task, now)
            && let Some(l) = self.lease(session, task)
        {
            let lease = l.clone();
            return if lease.ram_mb < req.ram_mb && req.peak_mb.is_none() {
                Outcome::GrantedSmaller {
                    lease,
                    wanted_mb: req.ram_mb,
                }
            } else {
                Outcome::Granted(lease)
            };
        }
        if let Some(l) = self.lease(session, task) {
            return Outcome::Held(l.clone());
        }
        if let Some(why) = Ledger::never_fits(cap, req) {
            self.queue
                .retain(|w| !(w.session == session && w.task == task));
            self.advance(cap, now, left, None);
            return Outcome::Refused(why);
        }
        let id = lease_id(session, task);
        // Join the line (or update the place already held) before looking
        // at who is ahead, so a Priority given now counts. A place whose
        // grant lapsed is live again: its manager is back.
        match self.queue.iter_mut().find(|w| w.id == id) {
            Some(w) => {
                w.request = req.clone();
                w.repo = repo.map(String::from).or(w.repo.take());
                w.lapsed = None;
            }
            None => self.queue.push(Waiter {
                id: id.clone(),
                session: session.into(),
                task: task.into(),
                repo: repo.map(String::from),
                request: req.clone(),
                queued_at: now,
                heartbeat_at: now,
                lapsed: None,
            }),
        }
        // Everyone else in line that fits goes first, whether or not its
        // manager is polling: the queue moves on anyone's call.
        self.advance(cap, now, left, Some(&id));
        let order = self.ordered_queue();
        let position = order.iter().position(|w| w.id == id).unwrap_or(0);
        let ahead: Vec<Waiter> = order[..position]
            .iter()
            .filter(|w| w.lapsed.is_none())
            .map(|w| (*w).clone())
            .collect();
        let (fit, past) = self.fit_behind(cap, &ahead, session, task, req, now, left);
        match fit {
            Ok(fit) => {
                self.queue.retain(|w| w.id != id);
                let lease = Ledger::lease_for(session, task, repo, req, fit, now);
                self.leases.push(lease.clone());
                if let Some(past) = past {
                    Outcome::Backfilled {
                        lease,
                        wanted_mb: req.ram_mb,
                        past,
                    }
                } else if lease.ram_mb < req.ram_mb {
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
                let building: HashSet<String> =
                    self.builders(cap).iter().map(|l| l.id.clone()).collect();
                let is_building = |l: &Lease| building.contains(&l.id);
                let blockers: Vec<Lease> = self
                    .leases
                    .iter()
                    .filter(|l| blocks(l, req, &short, &is_building))
                    .cloned()
                    .collect();
                // Fitting now, it waits only on the queue: on the one it
                // would delay, else on whoever ahead wants what it would take.
                let only_queue = self.try_fit(cap, req).is_ok();
                let behind =
                    past.or_else(|| in_the_way(&blockers, &ahead, req, &short, &is_building));
                let cpu = short.iter().any(|s| s == "cpu").then(|| {
                    format!(
                        "CPU busy: {}, {} tasks building",
                        cap.cpu.describe(cap.limits.cpu_cores),
                        building.len()
                    )
                });
                Outcome::Queued {
                    position: position + 1,
                    blockers,
                    ahead,
                    short,
                    behind,
                    only_queue,
                    request: req.clone(),
                    cpu,
                }
            }
        }
    }

    /// The lease `fit` makes of `session`'s `req` for `task`, granted `now`.
    fn lease_for(
        session: &str,
        task: &str,
        repo: Option<&str>,
        req: &Request,
        fit: Fit,
        now: u64,
    ) -> Lease {
        Lease {
            id: lease_id(session, task),
            session: session.into(),
            task: task.into(),
            repo: repo.map(String::from),
            ram_mb: fit.ram_mb,
            gpus: fit.gpus,
            vram_mb: if req.gpu > 0 { req.vram_mb } else { 0 },
            slots: req.slots.clone(),
            peak_mb: req.peak_mb.map(|_| fit.counted_mb),
            granted_at: now,
            heartbeat_at: now,
            uncollected: None,
        }
    }

    /// Take up `session`'s grant for `task` that was made on another
    /// session's call (giverny#429): its manager has claimed, started or
    /// run it. The grant then dates from `now`, as a fresh one would.
    /// True when there was such a grant.
    pub fn collect(&mut self, session: &str, task: &str, now: u64) -> bool {
        match self
            .leases
            .iter_mut()
            .find(|l| l.session == session && l.task == task && l.uncollected.is_some())
        {
            Some(l) => {
                l.uncollected = None;
                l.granted_at = now;
                l.heartbeat_at = l.heartbeat_at.max(now);
                true
            }
            None => false,
        }
    }

    /// Move the queue on, whoever calls (giverny#429). A grant used to
    /// happen only inside its own manager's `claim`, so a manager that
    /// stopped polling — a restart, a crash, a session that moved on —
    /// froze every waiter behind it. Now, on any ledger write:
    ///
    /// 1. a grant made this way that its manager has not taken up within
    ///    [`Capacity::uncollected_ms`] lapses: its lease is released and
    ///    its request goes back to its place in line, *lapsed* — set aside
    ///    for nobody and not granted again until its manager claims again
    ///    — or out, when its session is gone ([`Capacity::gone`]);
    /// 2. the waiters of a session that is gone leave the queue;
    /// 3. every waiter that fits, in queue order (Priority, then first
    ///    come), by the same rule its own `claim` would meet
    ///    ([`Ledger::fit_behind`]), is granted now: the lease is its
    ///    session's, marked [`Uncollected`] until that manager's next
    ///    `claim` (answered *granted*), `start` or `run`; its hook tells it
    ///    on its next tool call ([`news`]).
    ///
    /// `skip` is a waiter left for its caller to weigh (the claimant's
    /// own, so its answer can say what it says).
    pub fn advance(
        &mut self,
        cap: &Capacity,
        now: u64,
        left: &dyn Fn(&str, &str) -> Option<i64>,
        skip: Option<&str>,
    ) -> Advanced {
        let mut out = Advanced::default();
        let hold = cap.uncollected_ms();
        let (lapsing, keep): (Vec<Lease>, Vec<Lease>) = self
            .leases
            .drain(..)
            .partition(|l| l.uncollected.is_some() && now.saturating_sub(l.granted_at) >= hold);
        self.leases = keep;
        for l in lapsing {
            let Some(u) = l.uncollected else { continue };
            if cap.gone(&l.session, l.heartbeat_at, now) {
                out.dropped.push(l.id);
                continue;
            }
            let w = Waiter {
                id: l.id,
                session: l.session,
                task: l.task,
                repo: l.repo,
                request: u.request,
                queued_at: u.queued_at,
                heartbeat_at: l.heartbeat_at,
                lapsed: Some(Lapsed {
                    at: now,
                    told: false,
                }),
            };
            out.lapsed.push(w.task.clone());
            self.queue.retain(|x| x.id != w.id);
            self.queue.push(w);
        }
        self.queue.retain(|w| {
            let gone = skip != Some(w.id.as_str()) && cap.gone(&w.session, w.heartbeat_at, now);
            if gone {
                out.dropped.push(w.id.clone());
            }
            !gone
        });
        let order: Vec<String> = self
            .ordered_queue()
            .iter()
            .filter(|w| w.lapsed.is_none() && skip != Some(w.id.as_str()))
            .map(|w| w.id.clone())
            .collect();
        for id in order {
            let q = self.ordered_queue();
            let Some(at) = q.iter().position(|w| w.id == id) else {
                continue;
            };
            let w = q[at].clone();
            let ahead: Vec<Waiter> = q[..at]
                .iter()
                .filter(|x| x.lapsed.is_none())
                .map(|x| (*x).clone())
                .collect();
            let (fit, _) = self.fit_behind(cap, &ahead, &w.session, &w.task, &w.request, now, left);
            let Ok(fit) = fit else { continue };
            self.queue.retain(|x| x.id != id);
            let mut lease =
                Ledger::lease_for(&w.session, &w.task, w.repo.as_deref(), &w.request, fit, now);
            lease.heartbeat_at = w.heartbeat_at;
            lease.uncollected = Some(Uncollected {
                queued_at: w.queued_at,
                request: w.request.clone(),
                told: false,
            });
            out.granted.push(lease.clone());
            self.leases.push(lease);
        }
        out
    }

    /// Is there anything [`Ledger::advance`] could move: a waiter, or a
    /// grant not yet taken up?
    pub fn may_advance(&self) -> bool {
        !self.queue.is_empty() || self.leases.iter().any(|l| l.uncollected.is_some())
    }

    /// Does `req` fit behind the requests queued `ahead` of it? It fits when
    /// it fits in what is left after every one of them is set aside
    /// ([`Ledger::with_reserved`]) — or, by **backfill** (giverny#414),
    /// when it fits beside those ahead that fit now and does not delay any
    /// that do not (EASY backfill, conservative):
    ///
    /// - a waiter ahead that does not fit now is *blocked*; its *shadow* is
    ///   the soonest it could fit, by the holders' expected ends (`left`;
    ///   an unknown end counts as now, so the shadow is never late), where
    ///   it needs its own expected peak and every blocked waiter's before
    ///   it, all at once;
    /// - `req` may go before each blocked waiter whose slots and GPUs it
    ///   does not want, when either it is expected to end by that shadow,
    ///   or what is surely free at the shadow (only known ends count)
    ///   still covers them all with `req` holding its share.
    ///
    /// Free memory only grows as holders end, so a waiter's real start is no
    /// sooner than its shadow and finds at least that much free: a backfill
    /// never pushes it back, by the estimates. Nothing jumps a waiter that
    /// fits now, so a waiter is granted the moment it fits; no starvation.
    ///
    /// The fit and, for a backfill, the blocked waiter it went past; for a
    /// refused backfill, the waiter it would have delayed.
    #[allow(clippy::too_many_arguments)]
    pub fn fit_behind(
        &self,
        cap: &Capacity,
        ahead: &[Waiter],
        session: &str,
        task: &str,
        req: &Request,
        now: u64,
        left: &dyn Fn(&str, &str) -> Option<i64>,
    ) -> (Result<Fit, Vec<String>>, Option<String>) {
        let all = self.with_reserved(cap, ahead).try_fit(cap, req);
        if all.is_ok() {
            return (all, None);
        }
        // Set aside the waiters ahead that fit now, in order; the rest are
        // blocked.
        let mut v = self.with_reserved(cap, &[]);
        let mut blocked: Vec<&Waiter> = Vec::new();
        for w in ahead {
            if v.try_fit(cap, &w.request).is_ok() {
                v = v.with_reserved(cap, std::slice::from_ref(w));
            } else {
                blocked.push(w);
            }
        }
        let Some(head) = blocked.first() else {
            return (all, None);
        };
        let Ok(fit) = v.try_fit(cap, req) else {
            return (all, None);
        };
        let free_now = v.free(cap).ram_mb;
        let end_of = |s: &str, t: &str| left(s, t).map(|x| now + x.max(0) as u64 * 1000);
        // Every holder (a lease, or a waiter set aside): what it frees, when.
        let mut holders: Vec<(Option<u64>, u64)> = v
            .leases
            .iter()
            .map(|l| (end_of(&l.session, &l.task), cap.counted_mb(l)))
            .collect();
        holders.sort_by_key(|&(end, _)| end.unwrap_or(now));
        // Its own end: a figure already spent says nothing.
        let own_end = left(session, task)
            .filter(|&x| x > 0)
            .map(|x| now + x as u64 * 1000);
        let mut need = 0;
        for w in &blocked {
            let r = &w.request;
            if (r.gpu > 0 && req.gpu > 0) || r.slots.iter().any(|s| req.slots.contains(s)) {
                return (all, Some(w.task.clone()));
            }
            need += Ledger::counted_for(cap, r, r.ram_mb);
            // The shadow: the soonest enough could be free.
            let mut freed = free_now;
            let mut shadow = now;
            for &(end, mb) in &holders {
                if freed >= need {
                    break;
                }
                freed += mb;
                shadow = end.unwrap_or(now);
            }
            if freed < need {
                // Not even every holder ending frees enough: it waits on
                // other programs, which may free it any moment.
                shadow = now;
            }
            let sure: u64 = free_now
                + holders
                    .iter()
                    .filter(|(end, _)| end.is_some_and(|e| e <= shadow))
                    .map(|&(_, mb)| mb)
                    .sum::<u64>();
            let done_first = own_end.is_some_and(|e| e <= shadow);
            if !done_first && sure < need + fit.counted_mb {
                return (all, Some(w.task.clone()));
            }
        }
        (Ok(fit), Some(head.task.clone()))
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
        ram_mb: Option<u64>,
        vram_mb: Option<u64>,
        now: u64,
    ) -> Option<(Lease, Lease)> {
        let l = self
            .leases
            .iter_mut()
            .find(|l| l.session == session && l.task == task)?;
        let vram_mb = vram_mb.filter(|_| !l.gpus.is_empty());
        let larger = ram_mb.is_some_and(|r| r > l.ram_mb) || vram_mb.is_some_and(|v| v > l.vram_mb);
        let smaller =
            ram_mb.is_some_and(|r| r < l.ram_mb) || vram_mb.is_some_and(|v| v < l.vram_mb);
        if larger || !smaller {
            return None;
        }
        let before = l.clone();
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

/// Who a queued `req` really waits on, given what it is `short` of: the
/// first request queued `ahead` of it that wants any of that, since it goes
/// first whatever frees (a backfill past it was refused,
/// [`Ledger::fit_behind`]); else the first
/// of `leases` holding any of it — for a busy CPU, the first `building`
/// ([`Ledger::builders`]); else nobody (other programs hold it).
pub fn in_the_way<'a>(
    leases: impl IntoIterator<Item = &'a Lease>,
    ahead: impl IntoIterator<Item = &'a Waiter>,
    req: &Request,
    short: &[String],
    building: &dyn Fn(&Lease) -> bool,
) -> Option<String> {
    let wants = |w: &Waiter| {
        let r = &w.request;
        short.iter().any(|s| match s.as_str() {
            // Every request waits on a busy CPU.
            "cpu" => true,
            // Every request is set aside at some memory.
            "ram" => true,
            "gpu" => r.gpu > 0,
            s => s
                .strip_prefix("slot ")
                .is_some_and(|slot| r.slots.iter().any(|x| x == slot)),
        })
    };
    ahead
        .into_iter()
        .find(|w| wants(w))
        .map(|w| w.task.clone())
        .or_else(|| {
            leases
                .into_iter()
                .find(|l| blocks(l, req, short, building))
                .map(|l| l.task.clone())
        })
}

/// What a queued request is short of, in its own sizes: `CPU` (busy),
/// `3G`, `CPU, 3G`, `gpu 8G`, `a slot`. Empty when nothing is known to be
/// short. The panel's queued row and `claim`'s line both say it.
pub fn short_need(short: &[String], ram_mb: u64, vram_mb: u64) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut push = |p: String| {
        if !parts.contains(&p) {
            parts.push(p);
        }
    };
    for s in short {
        match s.as_str() {
            "cpu" => push("CPU".into()),
            "ram" if ram_mb > 0 => push(Mem(ram_mb).to_string()),
            "ram" => push("RAM".into()),
            "gpu" if vram_mb > 0 => push(format!("gpu {}", Mem(vram_mb))),
            "gpu" => push("a GPU".into()),
            _ => push("a slot".into()),
        }
    }
    parts.join(", ")
}

/// Does lease `l` hold something `req` is short of? A busy CPU is held by
/// the leases `building`.
fn blocks(l: &Lease, req: &Request, short: &[String], building: &dyn Fn(&Lease) -> bool) -> bool {
    short.iter().any(|s| match s.as_str() {
        "cpu" => building(l),
        // Every lease counts at some memory (its peak, RAM or the default).
        "ram" => true,
        "gpu" => !l.gpus.is_empty(),
        s => s.strip_prefix("slot ").is_some_and(|slot| {
            l.slots.iter().any(|x| x == slot) && req.slots.iter().any(|x| x == slot)
        }),
    })
}

/// `3G, gpu0 8G, slot cargo:/x`: what a lease holds — never cores
/// (giverny#387).
pub fn describe_held(ram_mb: u64, gpus: &[u32], vram_mb: u64, slots: &[String]) -> String {
    let mut parts = Vec::new();
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
        describe_held(self.ram_mb, &self.gpus, self.vram_mb, &self.slots)
    }
}

impl Request {
    pub fn describe(&self) -> String {
        let mut s = describe_held(self.ram_mb, &[], 0, &self.slots);
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

/// `, counted at its kind's peak 1.2G` when admission counts a lease at an
/// expected peak other than its RAM; `, counted at 6G, the most its lease
/// lets it use` when it is held to that ([`lease_bound_mb`], giverny#455):
/// its kind peaked at that or more.
fn counted(l: &Lease) -> String {
    match l.peak_mb {
        Some(p) if l.ram_mb > 0 && p == lease_bound_mb(l.ram_mb) => format!(
            ", counted at {}, the most its lease lets it use",
            Mem(lease_bound_mb(l.ram_mb))
        ),
        Some(p) if p != l.ram_mb => format!(", counted at its kind's peak {}", Mem(p)),
        _ => String::new(),
    }
}

/// `; granted, waiting for its manager` for a grant its manager has not
/// taken up ([`Ledger::advance`]): what the queued and held lines say of it.
pub fn waiting(l: &Lease) -> &'static str {
    if l.uncollected.is_some() {
        "; granted, waiting for its manager"
    } else {
        ""
    }
}

/// The line `claim` prints.
pub fn outcome_line(task: &str, o: &Outcome, eta: &dyn Fn(&str, &str) -> Option<i64>) -> String {
    let holder = |l: &Lease| {
        let left = eta(&l.session, &l.task)
            .map(|s| format!("; ~{}", feed::fmt_span(s.max(0))))
            .unwrap_or_default();
        format!("{} ({}{left}{})", l.task, l.describe(), waiting(l))
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
        Outcome::Backfilled {
            lease,
            wanted_mb,
            past,
        } => {
            let smaller = if lease.ram_mb < *wanted_mb {
                format!(" (asked {}, more is not free)", Mem(*wanted_mb))
            } else {
                String::new()
            };
            format!(
                "granted {task}: {}{}{smaller}, backfilled ahead of {past}, which it does not delay",
                lease.describe(),
                counted(lease),
            )
        }
        Outcome::Queued {
            position,
            blockers,
            ahead,
            short,
            behind,
            only_queue,
            request,
            cpu,
        } => {
            let mut s = format!("queued #{position}");
            // What is short, in the request's own sizes (the panel's words).
            let need = short_need(short, request.ram_mb, request.vram_mb);
            if !need.is_empty() {
                s.push_str(&format!(" for {need}"));
            }
            let waiter = behind
                .as_deref()
                .and_then(|b| ahead.iter().position(|w| w.task == b));
            let mut held: Vec<&Lease> = blockers.iter().collect();
            held.sort_by_key(|l| {
                (
                    behind.as_deref() != Some(l.task.as_str()),
                    eta(&l.session, &l.task).unwrap_or(i64::MAX),
                )
            });
            let list = |held: &[&Lease]| {
                let mut names: Vec<String> = held.iter().take(3).map(|l| holder(l)).collect();
                if held.len() > 3 {
                    names.push(format!("{} more", held.len() - 3));
                }
                names.join(", ")
            };
            if let (Some(k), true) = (waiter, *only_queue) {
                // Nothing but the queue holds it back.
                let w = &ahead[k];
                s.push_str(&format!(
                    " behind {}'s {} (queued #{}), the only thing in the way",
                    w.task,
                    w.request.describe(),
                    k + 1
                ));
            } else if let Some(k) = waiter {
                let w = &ahead[k];
                s.push_str(&format!(
                    " behind {} (queued #{}: {})",
                    w.task,
                    k + 1,
                    w.request.describe()
                ));
                if !held.is_empty() {
                    s.push_str(&format!("; held by {}", list(&held)));
                }
            } else if !held.is_empty() {
                s.push_str(&format!(" behind {}", list(&held)));
                if let Some(first) = ahead.first() {
                    s.push_str(&format!(
                        "; after {} in the queue ({})",
                        first.task,
                        first.request.describe()
                    ));
                }
            } else if short.iter().any(|x| x != "cpu") {
                s.push_str(&format!(" ({} taken by other programs)", short.join(", ")));
            }
            if let Some(c) = cpu {
                s.push_str(&format!("; {c}"));
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
            "ram_mb": l.ram_mb,
            "gpus": l.gpus,
            "vram_mb": l.vram_mb,
            "slots": l.slots,
            "granted_at": manage::stamp(l.granted_at),
        });
        if state == "smaller" && l.ram_mb < req.ram_mb {
            v["wanted_ram_mb"] = json!(req.ram_mb);
        }
        v
    };
    match o {
        Outcome::Granted(l) | Outcome::Held(l) => Some(granted(l, "granted")),
        Outcome::GrantedSmaller { lease, .. } => Some(granted(lease, "smaller")),
        Outcome::Backfilled { lease, past, .. } => {
            let mut v = granted(
                lease,
                if lease.ram_mb < req.ram_mb {
                    "smaller"
                } else {
                    "granted"
                },
            );
            v["backfilled_past"] = json!(past);
            Some(v)
        }
        Outcome::Queued {
            position,
            short,
            behind,
            ..
        } => Some(json!({
            "state": "queued",
            "position": position,
            "behind": behind,
            "short": short,
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

/// [`Ledger::advance`] on `l` when it has anything to move, under `cap`
/// (else this machine, detected — only then), with the tasks' times from
/// the feeds in `feed_dir`. Nothing when the machine cannot be read.
fn advance_in(
    l: &mut Ledger,
    path: &Path,
    feed_dir: Option<&Path>,
    cap: Option<&Capacity>,
    now: u64,
) -> Advanced {
    if !l.may_advance() {
        return Advanced::default();
    }
    let detected;
    let cap = match cap {
        Some(c) => c,
        None => match Capacity::detect(path) {
            Ok(c) => {
                detected = c;
                &detected
            }
            Err(_) => return Advanced::default(),
        },
    };
    let left = |s: &str, t: &str| feed_dir.and_then(|d| eta_or_estimate_s(d, s, t, now));
    l.advance(cap, now, &left, None)
}

/// Every `giverny manage` command's heartbeat for `session`, and the hook's.
/// Touches nothing when there is no ledger yet, and writes only every
/// [`BEAT_EVERY_MS`] — or when the queue moved: every beat also moves it
/// on ([`Ledger::advance`], giverny#429), under `cap` (else this machine),
/// so a queue whose managers stopped polling still advances. With the feed
/// dir, a lease or queued request of a task whose feed row has landed is
/// dropped rather than renewed.
pub fn heartbeat(
    path: &Path,
    feed_dir: Option<&Path>,
    session: &str,
    now: u64,
    cap: Option<&Capacity>,
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
    let beat = l.heartbeat(session, now);
    let moved = advance_in(&mut l, path, feed_dir, cap, now).moved();
    if beat || moved || l.leases.len() != before.leases.len() || l.queue.len() != before.queue.len()
    {
        let v = serde_json::to_value(&l).map_err(|e| e.to_string())?;
        manage::write(path, &v).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// Release `session`'s `task` in the ledger at `path` (nothing when there
/// is no ledger), and move the queue on into what it freed
/// ([`Ledger::advance`], under `cap`, else this machine). The lease
/// released, if any.
pub fn release_at(
    path: &Path,
    feed_dir: Option<&Path>,
    session: &str,
    task: &str,
    now: u64,
    cap: Option<&Capacity>,
) -> Result<Option<Lease>, String> {
    if !path.exists() {
        return Ok(None);
    }
    with_ledger(path, now, |l| {
        let gone = l.release(session, task, now);
        advance_in(l, path, feed_dir, cap, now);
        gone
    })
}

/// `session`'s manager takes up its grant for `task` (`start`, `run`):
/// [`Ledger::collect`] in the ledger at `path`, when there is one.
pub fn collect_at(path: &Path, session: &str, task: &str, now: u64) -> Result<bool, String> {
    let has = crate::manage_inbox::read_ledger(path, now).is_some_and(|l| {
        l.leases
            .iter()
            .any(|x| x.session == session && x.task == task && x.uncollected.is_some())
    });
    if !has {
        return Ok(false);
    }
    with_ledger(path, now, |l| l.collect(session, task, now))
}

/// What the ledger did for `session` while its manager was not looking
/// (giverny#429), told once on its hook: a queued claim granted on another
/// session's call, or such a grant that lapsed untaken.
pub fn news(path: &Path, session: &str, now: u64) -> Option<String> {
    let l = crate::manage_inbox::read_ledger(path, now)?;
    let untold = l
        .leases
        .iter()
        .any(|x| x.session == session && x.uncollected.as_ref().is_some_and(|u| !u.told))
        || l.queue
            .iter()
            .any(|w| w.session == session && w.lapsed.as_ref().is_some_and(|x| !x.told));
    if !untold {
        return None;
    }
    let hold_min = giverny_core::config::DefaultLease::load()
        .uncollected_min
        .max(1);
    let lines = with_ledger(path, now, |l| {
        let mut lines = Vec::new();
        for x in l.leases.iter_mut().filter(|x| x.session == session) {
            if let Some(u) = x.uncollected.as_mut().filter(|u| !u.told) {
                u.told = true;
                let ago = feed::fmt_span((now.saturating_sub(x.granted_at) / 1000) as i64);
                lines.push(format!(
                    "Giverny: the ledger granted your queued claim for {} ({}) {ago} ago, on                      another session's call. Start its worker now; `giverny-manage claim {}`                      takes it up (exit 0), as do `start` and `run`. Untaken for {hold_min}m,                      it lapses and the queue moves past it.",
                    x.task,
                    x.describe(),
                    x.task,
                ));
            }
        }
        for w in l.queue.iter_mut().filter(|w| w.session == session) {
            if let Some(x) = w.lapsed.as_mut().filter(|x| !x.told) {
                x.told = true;
                lines.push(format!(
                    "Giverny: your grant for {} ({}) lapsed untaken after {hold_min}m and was                      given back; it keeps its place in the queue but waits for you: re-run                      `giverny-manage claim {}` when you can start it.",
                    w.task,
                    w.request.describe(),
                    w.task,
                ));
            }
        }
        lines
    })
    .ok()?;
    (!lines.is_empty()).then(|| lines.join("\n"))
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
        Some(v) => {
            crate::manage_run::close_oom_on_grant(row, &v);
            row.insert("lease".into(), v)
        }
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
        "default   {} RAM a task (Settings → Management panel)\n",
        cap.default_lease.ram
    ));
    let building = l.builders(cap).len();
    out.push_str(&format!(
        "cpu       {} — shared, not reserved; {}\n",
        cap.cpu.describe(lim.cpu_cores),
        if l.cpu_busy(cap) {
            format!("busy: claims wait ({building} tasks building)")
        } else if cap.cpu.saturated(lim.cpu_cores) {
            format!("saturated, but {building} task(s) building: claims go on")
        } else {
            "claims go on".to_string()
        }
    ));
    out.push_str(&format!(
        "others    {} available (headroom {})\n",
        cap.load
            .mem_available
            .map(|m| m.to_string())
            .unwrap_or_else(|| "?".into()),
        ram_headroom(m),
    ));
    out.push_str(&format!(
        "memory    {} counted of {}: the leases at their expected peaks (or use, \
         or lease), {} for the claudes{}, {} headroom\n",
        Mem(free.counted_mb),
        lim.ram,
        Mem(cap
            .mem_use
            .other_mb
            .saturating_sub(cap.mem_use.orphans_mb())),
        if cap.mem_use.orphans.is_empty() {
            String::new()
        } else {
            format!(", {} in orphaned runs", Mem(cap.mem_use.orphans_mb()))
        },
        ram_headroom(m),
    ));
    if let Some(split) = &cap.mem_use.slice {
        out.push_str(&format!("slice     {}\n", split.describe()));
    }
    if !cap.mem_use.orphans.is_empty() {
        out.push_str(&format!(
            "orphans   {} — their `run` is gone and no lease counts them; \
             `systemctl --user stop <unit>.scope` frees it\n",
            cap.mem_use
                .orphans
                .iter()
                .map(|(u, mb)| format!("{u} {}", Mem(*mb)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push_str(&format!(
        "free now  {} RAM{}\n",
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
            let cpu_using = cap.cpu_use.of(&x.id);
            let mem = match (cpu_using > 0, using > 0) {
                (false, false) => format!("  counts {}", Mem(counts)),
                (c, m) => format!(
                    "  counts {}, using {}",
                    Mem(counts),
                    [
                        c.then(|| format!("{} cpu", fmt_cores(cpu_using))),
                        m.then(|| Mem(using).to_string()),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(", ")
                ),
            };
            let uncollected = match &x.uncollected {
                Some(_) => format!(
                    ", granted {} ago, waiting for its manager (lapses in {})",
                    ago(x.granted_at),
                    feed::fmt_span(
                        ((x.granted_at + cap.uncollected_ms()).saturating_sub(now) / 1000) as i64
                    )
                ),
                None => String::new(),
            };
            out.push_str(&format!(
                "  {:<16} {}{mem}  session {}  beat {} ago{left}{uncollected}\n",
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
                "  #{} {:<14} {}  session {}  queued {} ago{}{}\n",
                i + 1,
                w.task,
                w.request.describe(),
                short_session(&w.session),
                ago(w.queued_at),
                w.request
                    .priority
                    .as_deref()
                    .map(|p| format!(", {p}"))
                    .unwrap_or_default(),
                w.lapsed
                    .as_ref()
                    .map(|x| format!(
                        ", its grant lapsed {} ago untaken: waits for its manager to claim again",
                        ago(x.at)
                    ))
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
            cpu_use: Default::default(),
            cpu: CpuBusy {
                slice_m: Some(0),
                at_ms: T0,
                ..Default::default()
            },
            live_sessions: None,
            run_bounds: RunBounds {
                caps: true,
                ..Default::default()
            },
        }
    }

    fn req(ram_gb: u64) -> Request {
        Request {
            ram_mb: ram_gb * 1024,
            ..Default::default()
        }
    }

    #[test]
    fn two_sessions_past_the_limit_queue_and_a_release_unblocks() {
        let c = cap();
        let mut l = Ledger::default();
        let a = l.claim(&c, "a", "t1", None, &req(7), T0);
        assert!(matches!(a, Outcome::Granted(_)), "{a:?}");
        let b = l.claim(&c, "b", "t2", None, &req(7), T0);
        assert!(matches!(b, Outcome::Granted(_)), "{b:?}");
        // The limit (16 G less 1.15 G headroom) is full: a third waits
        // behind both.
        let q = l.claim(&c, "b", "t3", None, &req(2), T0 + MIN);
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
        let q2 = l.claim(&c, "a", "t4", None, &req(1), T0 + 2 * MIN);
        assert!(matches!(q2, Outcome::Queued { position: 2, .. }), "{q2:?}");
        // Once t1 is released, t4's call grants t3 first, at its place
        // (giverny#429): 7.85 G free, t3's 2 G taken, 6 G no longer fits.
        assert!(l.release("a", "t1", T0 + 3 * MIN).is_some());
        let big = l.claim(&c, "a", "t4", None, &req(6), T0 + 3 * MIN);
        assert!(
            matches!(big, Outcome::Queued { position: 1, .. }),
            "{big:?}"
        );
        assert!(l.lease("b", "t3").is_some_and(|x| x.uncollected.is_some()));
        let g = l.claim(&c, "a", "t4", None, &req(1), T0 + 3 * MIN);
        assert!(matches!(g, Outcome::Granted(_)), "{g:?}");
        let g = l.claim(&c, "b", "t3", None, &req(2), T0 + 3 * MIN);
        assert!(matches!(g, Outcome::Granted(_)), "{g:?}");
        assert!(l.queue.is_empty());
        // A re-claim is the same lease.
        let h = l.claim(&c, "a", "t4", None, &req(1), T0 + 4 * MIN);
        assert!(matches!(h, Outcome::Held(_)), "{h:?}");
    }

    /// giverny#387: a claim holds no cores and waits on none. Ten leases
    /// on a 12-core limit leave the next one granted on its RAM alone; an
    /// older ledger's `cpu` figures are read and count for nothing.
    #[test]
    fn a_claim_is_granted_on_ram_alone_and_old_cpu_figures_are_ignored() {
        let c = cap();
        let mut l = Ledger::default();
        for i in 0..10 {
            let g = l.claim(&c, "a", &format!("t{i}"), None, &req(1), T0);
            assert!(matches!(g, Outcome::Granted(_)), "{g:?}");
        }
        let g = l.claim(&c, "b", "next", None, &req(3), T0);
        let Outcome::Granted(lease) = &g else {
            panic!("{g:?}")
        };
        assert_eq!(outcome_line("next", &g, &|_, _| None), "granted next: 3G");
        let text = serde_json::to_string(lease).unwrap();
        assert!(!text.contains("cpu"), "a lease holds no cores: {text}");
        // A ledger from before: its cores are ignored, its RAM is not.
        let old = Ledger::parse(
            br#"{"version":1,"leases":[
              {"id":"o:fold","session":"o","task":"fold","cpu":12,"cpu_use_m":900,
               "ram_mb":2048,"granted_at":"2026-10-14T10:00:00Z",
               "heartbeat_at":"2026-10-14T10:00:00Z"}],
             "queue":[
              {"id":"o:w","session":"o","task":"w","cpu":3,"min_cpu":1,"ram_mb":1024,
               "queued_at":"2026-10-14T10:00:00Z","heartbeat_at":"2026-10-14T10:00:00Z"}]}"#,
        )
        .unwrap();
        assert_eq!(old.leases[0].describe(), "2G");
        assert_eq!(old.queue[0].request.describe(), "1G");
        assert!(old.try_fit(&c, &req(3)).is_ok());
    }

    /// The machine with the CPU `slice_m` thousandths busy over the last
    /// minute and `running` leases building.
    fn busy(slice_m: u64, running: &[&str]) -> Capacity {
        let mut c = cap();
        c.cpu.slice_m = Some(slice_m);
        c.cpu.running = running.iter().map(|s| s.to_string()).collect();
        c
    }

    /// giverny#387: a claim waits on CPU only while the CPU has been
    /// saturated *and* at least two tasks are building (or about to): one
    /// busy build alone never holds the next worker back.
    #[test]
    fn a_claim_waits_on_cpu_only_when_saturated_with_two_building() {
        // 11 of the 12 cores busy: saturated (at 90 %).
        let mut l = Ledger::default();
        l.claim(&cap(), "a", "build", None, &req(2), T0 - 10 * MIN);
        let c = busy(11_000, &["a:build"]);
        assert!(c.cpu.saturated(12));
        // One build alone, however busy: the next worker goes.
        let g = l.claim(&c, "b", "second", None, &req(2), T0);
        assert!(matches!(g, Outcome::Granted(_)), "{g:?}");
        // Now two build (one running, one granted a moment ago): a third
        // waits for CPU, behind the first building.
        let q = l.claim(&c, "b", "third", None, &req(2), T0);
        let Outcome::Queued { short, behind, .. } = &q else {
            panic!("{q:?}")
        };
        assert_eq!(short, &vec!["cpu".to_string()]);
        assert_eq!(behind.as_deref(), Some("build"));
        let line = outcome_line("third", &q, &|_, _| None);
        assert!(
            line.starts_with(
                "queued #1 for CPU behind build (2G), second (2G); CPU busy: 11.0 of 12 busy"
            ),
            "{line}"
        );
        assert!(line.contains("2 tasks building"), "{line}");
        let row = row_lease(&q, &req(2)).unwrap();
        assert_eq!(row["short"], json!(["cpu"]));
        assert!(row.get("cpu").is_none(), "{row}");
        // The second's grant is no longer fresh and it runs nothing: one
        // builds, so the third goes.
        let mut later = busy(11_000, &["a:build"]);
        later.cpu.at_ms = T0 + FRESH_MS;
        assert!(!l.cpu_busy(&later));
        assert!(matches!(
            l.claim(&later, "b", "third", None, &req(2), T0 + FRESH_MS),
            Outcome::Granted(_)
        ));
        // Two running but the CPU not saturated: no wait.
        let calm = busy(6_000, &["a:build", "b:second"]);
        assert!(!calm.cpu.saturated(12));
        assert!(!l.cpu_busy(&calm));
    }

    /// What says the CPU is saturated: on Linux the slice at 90 % of its
    /// quota or its pressure at 60 %; on macOS the load average at 90 % of
    /// the limit.
    #[test]
    fn saturation_is_read_from_the_slice_or_on_macos_the_load() {
        let at = |slice_m: Option<u64>, load_m: Option<u64>, pressure: Option<f64>| {
            CpuBusy {
                slice_m,
                load_m,
                pressure,
                ..Default::default()
            }
            .saturated(12)
        };
        assert!(at(Some(10_800), None, None));
        assert!(!at(Some(10_700), Some(13_000), None), "the slice speaks");
        assert!(at(None, Some(10_800), None), "macOS: the load average");
        assert!(!at(None, Some(10_000), None));
        assert!(at(Some(1_000), None, Some(61.0)), "pressure");
        assert!(!at(Some(1_000), None, Some(12.0)));
        assert!(!at(None, None, None), "nothing measures: never busy");
        let c = CpuBusy {
            slice_m: Some(6_300),
            pressure: Some(12.4),
            ..Default::default()
        };
        assert_eq!(
            c.describe(8),
            "6.3 of 8 busy (giverny-claude.slice, last minute; pressure 12%)"
        );
        let mac = CpuBusy {
            load_m: Some(2_500),
            ..Default::default()
        };
        assert_eq!(mac.describe(12), "2.5 of 12 busy (1-minute load average)");
        assert_eq!(CpuBusy::default().describe(12), "? of 12 busy");
        assert_eq!(
            some_avg60(
                "some avg10=1.00 avg60=12.40 avg300=3.66 total=7\n\
                 full avg10=0.00 avg60=0.24 avg300=2.50 total=5\n"
            ),
            Some(12.4)
        );
        assert_eq!(some_avg60(""), None);
    }

    /// The slice's busy cores are its CPU counter over about the last
    /// minute, from samples kept between calls.
    #[test]
    fn the_slices_busy_cores_come_from_samples_a_minute_apart() {
        let s = 1_000_000;
        let mut v: Vec<(u64, u64)> = Vec::new();
        // The first sample says nothing yet.
        assert_eq!(busy_from_samples(&mut v, T0, s), None);
        assert_eq!(v, vec![(T0, s)]);
        // 10 s later: too young a window; no new sample either.
        assert_eq!(busy_from_samples(&mut v, T0 + 10_000, s + 60_000_000), None);
        assert_eq!(v.len(), 1);
        // 30 s on, 4 cores busy since: 120 s of CPU in 30 s.
        assert_eq!(
            busy_from_samples(&mut v, T0 + 30_000, s + 120_000_000),
            Some(4000)
        );
        assert_eq!(v.len(), 2);
        // 90 s on: measured from the oldest (T0), and the window slides.
        assert_eq!(
            busy_from_samples(&mut v, T0 + 90_000, s + 360_000_000),
            Some(4000)
        );
        assert_eq!(
            v.iter().map(|x| x.0).collect::<Vec<_>>(),
            [T0 + 30_000, T0 + 90_000]
        );
        // A slice made anew (its counter back at zero) or samples too old
        // to speak for now are dropped.
        assert_eq!(busy_from_samples(&mut v, T0 + 100_000, 5), None);
        let mut old = vec![(T0, s)];
        assert_eq!(
            busy_from_samples(&mut old, T0 + SAMPLE_MAX_MS + 1, s * 2),
            None
        );
        // The file round trip.
        let dir = std::env::temp_dir().join(format!("giverny-cpusamples-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = cpu_samples_path(&dir.join("ledger.json"));
        assert_eq!(slice_busy_m(&path, T0, s), None);
        assert_eq!(
            slice_busy_m(&path, T0 + 60_000, s + 120_000_000),
            Some(2000)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_expired_lease_frees_itself_and_a_heartbeat_keeps_one() {
        let c = cap();
        let mut l = Ledger::default();
        l.claim(&c, "a", "big", None, &req(10), T0);
        l.claim(&c, "b", "kept", None, &req(4), T0);
        assert!(matches!(
            l.claim(&c, "c", "x", None, &req(4), T0),
            Outcome::Queued { .. }
        ));
        // Session b keeps beating; a goes silent; c polls.
        for k in 1..=4 {
            l.heartbeat("b", T0 + k * 5 * MIN);
            l.heartbeat("c", T0 + k * 5 * MIN);
        }
        let now = T0 + TTL_MS + MIN;
        let g = l.claim(&c, "c", "x", None, &req(4), now);
        assert!(matches!(g, Outcome::Granted(_)), "{g:?}");
        assert!(l.lease("a", "big").is_none(), "expired");
        assert!(
            l.lease("b", "kept").is_some(),
            "kept alive by its heartbeats"
        );
        // A queued request that stops polling expires too.
        let mut l = Ledger::default();
        l.claim(&c, "a", "big", None, &req(14), T0);
        l.claim(&c, "z", "gone", None, &req(1), T0);
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
            line.starts_with("queued #1 for a slot behind w1 (slot cargo:/t; ~14m)"),
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

    /// giverny#333: a queued request names what it is really short of, in
    /// its own sizes, and who is really in the way — the request queued
    /// ahead that wants it, else a lease holding it. `claim`'s line and the
    /// row's lease agree. The grant rule is not this test's.
    #[test]
    fn a_queued_request_names_what_is_short_and_who_is_in_the_way() {
        let queued = |o: &Outcome| match o {
            Outcome::Queued { short, behind, .. } => (short.clone(), behind.clone()),
            o => panic!("{o:?}"),
        };
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();

        // CPU busy: fold146 and idle build on a saturated CPU; RAM is plenty.
        let mut l = Ledger::default();
        l.claim(&cap(), "o", "fold146", None, &req(2), T0 - 10 * MIN);
        l.claim(&cap(), "o", "idle", None, &req(1), T0 - 10 * MIN);
        let c = busy(12_000, &["o:fold146", "o:idle"]);
        let q = l.claim(&c, "a", "899x", None, &req(3), T0);
        assert_eq!(queued(&q), (s(&["cpu"]), Some("fold146".into())));
        let line = outcome_line("899x", &q, &|_, _| None);
        assert!(
            line.starts_with("queued #1 for CPU behind fold146 (2G), idle (1G); CPU busy"),
            "{line}"
        );
        let row = row_lease(&q, &req(3)).unwrap();
        assert_eq!(row["short"], json!(["cpu"]));
        assert_eq!(row["behind"], "fold146");

        // Held back by the request ahead: 899x goes first whatever frees,
        // so a later one waits on it, not on the builders.
        let q = l.claim(&c, "b", "283", None, &req(1), T0 + MIN);
        assert_eq!(queued(&q), (s(&["cpu"]), Some("899x".into())));
        let line = outcome_line("283", &q, &|_, _| None);
        assert!(
            line.starts_with(
                "queued #2 for CPU behind 899x (queued #1: 3G); held by fold146 (2G), idle (1G)"
            ),
            "{line}"
        );
        assert_eq!(row_lease(&q, &req(1)).unwrap()["behind"], "899x");

        // RAM short: 12G of the ~14.8G held; the CPU is idle.
        let c = cap();
        let mut l = Ledger::default();
        l.claim(&c, "o", "big", None, &req(12), T0);
        let q = l.claim(&c, "a", "t", None, &req(4), T0);
        assert_eq!(queued(&q), (s(&["ram"]), Some("big".into())));
        assert!(
            outcome_line("t", &q, &|_, _| None).starts_with("queued #1 for 4G behind big"),
            "{}",
            outcome_line("t", &q, &|_, _| None)
        );

        // Both short.
        let mut l = Ledger::default();
        l.claim(&cap(), "o", "big", None, &req(12), T0 - 10 * MIN);
        l.claim(&cap(), "o", "x", None, &req(1), T0 - 10 * MIN);
        let c = busy(12_000, &["o:big", "o:x"]);
        let q = l.claim(&c, "a", "t", None, &req(4), T0);
        assert_eq!(queued(&q), (s(&["cpu", "ram"]), Some("big".into())));
        assert!(
            outcome_line("t", &q, &|_, _| None).starts_with("queued #1 for CPU, 4G behind big"),
            "{}",
            outcome_line("t", &q, &|_, _| None)
        );
    }

    #[test]
    fn other_programs_shrink_what_is_grantable() {
        let mut c = cap();
        // A browser eats RAM: 5 G available, less 1.15 G headroom.
        c.load.mem_available = Some(Mem::gb(5));
        let mut l = Ledger::default();
        let q = l.claim(&c, "a", "t", None, &req(6), T0);
        assert!(matches!(q, Outcome::Queued { .. }), "{q:?}");
        assert!(outcome_line("t", &q, &|_, _| None).contains("taken by other programs"));
        // With --min-ram it is granted what is there.
        let r = Request {
            min_ram_mb: Some(2048),
            ..req(6)
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
        assert_eq!(
            outcome_line("t", &g, &|_, _| None),
            "granted smaller t: 3.8G (asked 6G, more is not free)"
        );
        let row = row_lease(&g, &r).unwrap();
        assert_eq!(row["state"], "smaller");
        assert_eq!(row["wanted_ram_mb"], 6 * 1024);
        // Past --min-ram it queues.
        let r = Request {
            min_ram_mb: Some(4096),
            ..req(6)
        };
        let q = Ledger::default().claim(&c, "a", "u", None, &r, T0);
        assert!(matches!(q, Outcome::Queued { .. }), "{q:?}");
    }

    /// A lease as a 3G default, its kind peaking at `peak_mb`.
    fn idle(l: &mut Ledger, c: &Capacity, task: &str, peak_mb: Option<u64>) {
        let r = Request { peak_mb, ..req(3) };
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
            ..req(3)
        };
        let g = l.claim(&c, "s", "new", Some("demo"), &new, T0);
        let Outcome::Granted(lease) = &g else {
            panic!("{g:?}")
        };
        // The lease is the 3G asked (its protection); it counts at its peak.
        assert_eq!((lease.ram_mb, lease.peak_mb), (3 * 1024, Some(1229)));
        assert_eq!(
            outcome_line("new", &g, &|_, _| None),
            "granted new: 3G, counted at its kind's peak 1.2G"
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
        l.claim(&c, "s", "slot", None, &req(0), T0);
        assert_eq!(l.free(&c).counted_mb, 3 * 1024);
    }

    /// giverny#455: a kind's history only lowers what a lease counts at. A
    /// `run` under a lease is throttled at twice it (`MemoryHigh`), so a
    /// peak past that is memory the lease cannot use.
    #[test]
    fn a_lease_counts_at_most_what_it_can_use() {
        let mut c = cap();
        let lease = |ram_gb: u64, peak_mb: Option<u64>| Lease {
            id: lease_id("s", "t"),
            session: "s".into(),
            task: "t".into(),
            repo: None,
            ram_mb: ram_gb * 1024,
            gpus: vec![],
            vram_mb: 0,
            slots: vec![],
            peak_mb,
            granted_at: T0,
            heartbeat_at: T0,
            uncollected: None,
        };
        // A 3G lease whose kind peaked at 16G: at most 6G.
        assert_eq!(c.counted_mb(&lease(3, Some(16 * 1024))), 6 * 1024);
        // Between its RAM and twice it, its kind's peak stands.
        assert_eq!(c.counted_mb(&lease(3, Some(4222))), 4222);
        // A 16G lease whose kind peaks at 2G: 2G.
        assert_eq!(c.counted_mb(&lease(16, Some(2048))), 2048);
        // No RAM, no bound; no history, its RAM.
        assert_eq!(c.counted_mb(&lease(0, Some(9 * 1024))), 9 * 1024);
        assert_eq!(c.counted_mb(&lease(3, None)), 3 * 1024);
        // What it uses now, above both, still counts.
        c.mem_use.by_lease.insert(lease_id("s", "t"), 7 * 1024);
        assert_eq!(c.counted_mb(&lease(3, Some(16 * 1024))), 7 * 1024);
        assert_eq!(c.counted_mb(&lease(1, Some(2048))), 7 * 1024);
        // Admission holds a request to the same bound, at the RAM granted.
        let c = cap();
        let r = Request {
            peak_mb: Some(16 * 1024),
            ..req(3)
        };
        assert_eq!(Ledger::counted_for(&c, &r, 3 * 1024), 6 * 1024);
        assert_eq!(Ledger::counted_for(&c, &r, 2 * 1024), 4 * 1024);
        let r = Request {
            peak_mb: Some(2048),
            ..req(16)
        };
        assert_eq!(Ledger::counted_for(&c, &r, 16 * 1024), 2048);
    }

    /// giverny#455, end to end: on a 20G limit two 3G leases whose kind
    /// once peaked at 16G counted at 16G each, so memory was full on paper;
    /// held to what they can use (6G each), a third such claim fits.
    #[test]
    fn leases_whose_kind_peaked_high_leave_room_for_another() {
        let mut c = cap();
        c.limits.ram = Mem::gb(20);
        let mut l = Ledger::default();
        idle(&mut l, &c, "a", Some(16 * 1024));
        idle(&mut l, &c, "b", Some(16 * 1024));
        assert_eq!(l.free(&c).counted_mb, 12 * 1024);
        let new = Request {
            peak_mb: Some(16 * 1024),
            ..req(3)
        };
        let g = l.claim(&c, "s", "new", Some("demo"), &new, T0);
        let Outcome::Granted(lease) = &g else {
            panic!("{g:?}")
        };
        assert_eq!((lease.ram_mb, lease.peak_mb), (3 * 1024, Some(6 * 1024)));
        assert_eq!(
            outcome_line("new", &g, &|_, _| None),
            "granted new: 3G, counted at 6G, the most its lease lets it use"
        );
        // That is all there is: a fourth waits.
        let q = l.claim(&c, "s", "fourth", Some("demo"), &new, T0);
        assert!(matches!(q, Outcome::Queued { .. }), "{q:?}");
    }

    /// giverny#456: the bound holds only where `run` holds a command to it,
    /// and a command started before its lease shrank keeps the old one: a
    /// lease counts at no less than what its commands can really reach.
    #[test]
    fn a_lease_counts_at_what_its_commands_can_really_reach() {
        let lease = |ram_gb: u64, peak_mb: Option<u64>| Lease {
            id: lease_id("s", "t"),
            session: "s".into(),
            task: "t".into(),
            repo: None,
            ram_mb: ram_gb * 1024,
            gpus: vec![],
            vram_mb: 0,
            slots: vec![],
            peak_mb,
            granted_at: T0,
            heartbeat_at: T0,
            uncollected: None,
        };
        let id = lease_id("s", "t");
        // Where `run` is advisory, nothing holds a 3G lease to 6G: it counts
        // at its kind's peak, as before giverny#455, and so is admitted.
        let mut c = cap();
        c.run_bounds.caps = false;
        assert_eq!(c.counted_mb(&lease(3, Some(10 * 1024))), 10 * 1024);
        let r = Request {
            peak_mb: Some(10 * 1024),
            ..req(3)
        };
        assert_eq!(Ledger::counted_for(&c, &r, 3 * 1024), 10 * 1024);
        let mut l = Ledger::default();
        let g = l.claim(&c, "s", "t", Some("demo"), &r, T0);
        let Outcome::Granted(granted) = &g else {
            panic!("{g:?}")
        };
        assert_eq!(granted.peak_mb, Some(10 * 1024));
        assert_eq!(
            outcome_line("t", &g, &|_, _| None),
            "granted t: 3G, counted at its kind's peak 10G"
        );
        assert_eq!(l.free(&c).counted_mb, 10 * 1024);
        // Where it caps, 6G; but a command of it running held to nothing (a
        // scope systemd would not make) is unbounded again.
        c.run_bounds.caps = true;
        assert_eq!(c.counted_mb(&lease(3, Some(10 * 1024))), 6 * 1024);
        c.run_bounds.live.insert(id.clone(), None);
        assert_eq!(c.counted_mb(&lease(3, Some(10 * 1024))), 10 * 1024);
        // Shrunk from 3G to 1G while a command started under 3G (held at
        // 6G) runs: 6G until it ends, then twice the new lease.
        let mut l = Ledger::default();
        let c = cap();
        let r = Request {
            peak_mb: Some(10 * 1024),
            ..req(3)
        };
        assert!(matches!(
            l.claim(&c, "s", "t", Some("demo"), &r, T0),
            Outcome::Granted(_)
        ));
        let (_, shrunk) = l.shrink("s", "t", Some(1024), None, T0).unwrap();
        let mut c = cap();
        c.run_bounds.live.insert(id.clone(), Some(6 * 1024));
        assert_eq!(c.counted_mb(&shrunk), 6 * 1024);
        assert_eq!(l.free(&c).counted_mb, 6 * 1024);
        c.run_bounds.live.clear();
        assert_eq!(c.counted_mb(&shrunk), 2 * 1024);
        // A command held below the lease's bound (started under a smaller
        // lease) never lowers it.
        c.run_bounds.live.insert(id, Some(1024));
        assert_eq!(c.counted_mb(&lease(3, Some(10 * 1024))), 6 * 1024);
    }

    /// giverny#456: what `run` did is read beside the ledger — whether it
    /// caps here, and what each running command is held to.
    #[test]
    fn run_bounds_are_read_beside_the_ledger() {
        let dir = std::env::temp_dir().join(format!("giverny-runbounds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let runs = crate::run_live::runs_dir(&dir.join("ledger.json"));
        std::fs::create_dir_all(&runs).unwrap();
        let ledger = dir.join("ledger.json");
        std::fs::write(run_caps_path(&ledger), "capped\n").unwrap();
        let _a =
            crate::run_live::register_bound(&runs.join("1-0.stats"), "a", "s", T0, Some(6144), &[])
                .unwrap();
        let _b =
            crate::run_live::register_bound(&runs.join("1-1.stats"), "a", "s", T0, Some(2048), &[])
                .unwrap();
        let _c = crate::run_live::register(&runs.join("1-2.stats"), "b", "s", T0).unwrap();
        let b = RunBounds::detect(&ledger);
        assert!(b.caps);
        assert_eq!(
            b.live,
            HashMap::from([(lease_id("s", "a"), Some(6144)), (lease_id("s", "b"), None)])
        );
        assert_eq!(b.bound(Some(&lease_id("s", "a")), 1024), Some(6144));
        assert_eq!(b.bound(Some(&lease_id("s", "b")), 1024), None);
        assert_eq!(b.bound(Some(&lease_id("s", "c")), 1024), Some(2048));
        assert_eq!(b.bound(None, 0), None);
        std::fs::write(run_caps_path(&ledger), "advisory\n").unwrap();
        let b = RunBounds::detect(&ledger);
        assert!(!b.caps);
        assert_eq!(b.bound(Some(&lease_id("s", "c")), 1024), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// giverny#281: what is really committed still queues a task — peaks
    /// that fill the slice, a task already above its peak, the claudes' own
    /// memory, other programs.
    #[test]
    fn a_really_full_box_still_queues() {
        let new = Request {
            peak_mb: Some(1229),
            ..req(3)
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
        // Who is building: every live run, scope or not.
        let plain = runs.join("1-1.stats");
        std::fs::write(&plain, "").unwrap();
        let _p = crate::run_live::register(&plain, "acme#5", "s2", T0).unwrap();
        let mut ids: Vec<String> = crate::run_live::running_leases(&runs).into_iter().collect();
        ids.sort();
        assert_eq!(ids, ["s1:demo#7", "s2:acme#5"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// giverny#424: a slice at its cap, full of page cache and reclaimable
    /// slab (dentries, inodes) around ~1.5G of processes, counts the
    /// claudes at what will not come back, so a claim that fits is granted.
    #[test]
    fn a_slice_full_of_cache_and_reclaimable_slab_still_grants() {
        let dir = std::env::temp_dir().join(format!("giverny-memsplit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let runs = dir.join("runs");
        let slice = dir.join("slice");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::create_dir_all(&slice).unwrap();
        let mb = |n: u64| n << 20;
        // 16G in all: 1.5G anon, 4G cache (200M of it tmpfs), 10.4G kernel
        // of which 9.8G reclaimable slab, 20M of sockets.
        std::fs::write(slice.join("memory.current"), format!("{}\n", mb(16 * 1024))).unwrap();
        std::fs::write(
            slice.join("memory.stat"),
            format!(
                "anon {}\nfile {}\nkernel {}\nkernel_stack {}\npagetables {}\nsock {}\n\
                 shmem {}\nslab_reclaimable {}\nslab_unreclaimable {}\nslab {}\n",
                mb(1536),
                mb(4096),
                mb(10650),
                mb(40),
                mb(60),
                mb(20),
                mb(200),
                mb(10035),
                mb(515),
                mb(10550),
            ),
        )
        .unwrap();
        // A run scope whose `run` is gone, and one whose `run` lives.
        for (unit, anon) in [
            ("giverny-run-scale343-999999999".to_string(), 1024u64),
            (format!("giverny-run-mine-{}", std::process::id()), 512),
        ] {
            let d = slice.join(format!("{unit}.scope"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("memory.current"), format!("{}\n", mb(anon + 100))).unwrap();
            std::fs::write(
                d.join("memory.stat"),
                format!("anon {}\nfile {}\n", mb(anon), mb(100)),
            )
            .unwrap();
        }
        let u = MemUse::sample_in(&runs, Some(&slice));
        // anon + shmem + kernel less reclaimable slab + sock.
        assert_eq!(u.other_mb, 1536 + 200 + 615 + 20);
        assert_eq!(
            u.orphans,
            [("giverny-run-scale343-999999999".to_string(), 1024)]
        );
        let split = u.slice.unwrap();
        assert_eq!(
            split.describe(),
            "2.3G held (anon 1.5G, shmem 200M, kernel 615M, sock 20M) of 16G in the slice; \
             comes back: cache 3.8G, reclaimable slab 9.8G"
        );
        let mut c = cap();
        c.mem_use = u;
        let g = Ledger::default().claim(&c, "s", "spx", Some("inbar"), &req(4), T0);
        assert!(matches!(g, Outcome::Granted(_)), "{g:?}");
        let shown = report(&Ledger::default(), &c, T0, &|_, _| None);
        assert!(
            shown.contains("1.3G for the claudes, 1G in orphaned runs,"),
            "{shown}"
        );
        assert!(
            shown.contains("orphans   giverny-run-scale343-999999999 1G — their `run` is gone"),
            "{shown}"
        );
        // Counted the old way (all but the page cache), it queued.
        c.mem_use.other_mb = (mb(16 * 1024) - mb(4096)) >> 20;
        let q = Ledger::default().claim(&c, "s", "spx", Some("inbar"), &req(4), T0);
        assert!(matches!(q, Outcome::Queued { .. }), "{q:?}");
        // An old kernel without `kernel` sums its parts.
        std::fs::write(
            slice.join("memory.stat"),
            format!(
                "anon {}\nkernel_stack {}\npagetables {}\nslab_reclaimable {}\nslab_unreclaimable {}\n",
                mb(1024),
                mb(40),
                mb(60),
                mb(8192),
                mb(500),
            ),
        )
        .unwrap();
        assert_eq!(held_mb(&slice), Some(1024 + 600));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A run's cores are its scope's CPU time over the time it has run.
    #[test]
    fn a_runs_cores_are_read_from_its_scope() {
        let dir = std::env::temp_dir().join(format!("giverny-cpuuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let runs = dir.join("runs");
        let cg = dir.join("scope");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::create_dir_all(&cg).unwrap();
        // 90 s of CPU in the minute since it started: 1.5 cores.
        std::fs::write(
            cg.join("cpu.stat"),
            "usage_usec 90000000\nuser_usec 80000000\nsystem_usec 10000000\n",
        )
        .unwrap();
        let stats = runs.join("1-0.stats");
        std::fs::write(&stats, format!("started\ncgroup {}\n", cg.display())).unwrap();
        let _live = crate::run_live::register(&stats, "demo#7", "s1", T0).unwrap();
        let u = CpuUse::sample(&runs, T0 + MIN);
        assert_eq!(u.of("s1:demo#7"), 1500);
        assert_eq!(u.of("s1:other"), 0);
        assert_eq!(cpu_m(&cg, 0), None);
        assert_eq!(cpu_m(&dir.join("none"), MIN), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn too_big_is_refused_not_queued_and_gpus_degrade_to_none() {
        let c = cap();
        let mut l = Ledger::default();
        let r = l.claim(&c, "a", "huge", None, &req(17), T0);
        assert!(matches!(r, Outcome::Refused(_)), "{r:?}");
        assert_eq!(r.exit_code(), exit::REFUSED);
        let gpu = Request {
            gpu: 1,
            vram_mb: 8 * 1024,
            ..req(1)
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
        l.claim(&c, "a", "big", None, &req(14), T0);
        l.claim(&c, "b", "early", None, &req(1), T0);
        let urgent = Request {
            priority: Some("ASAP".into()),
            ..req(1)
        };
        let q = l.claim(&c, "c", "urgent", None, &urgent, T0 + MIN);
        assert!(matches!(q, Outcome::Queued { position: 1, .. }), "{q:?}");
        let order: Vec<&str> = l.ordered_queue().iter().map(|w| w.task.as_str()).collect();
        assert_eq!(order, ["urgent", "early"]);
    }

    /// giverny#414: 9G held for ~11m more leaves 5.85G of the ~14.85G; the
    /// head of the queue wants 6G. A smaller claim behind it is granted now
    /// when it does not delay the head, and waits when it would.
    #[test]
    fn a_small_claim_backfills_past_a_big_head_that_does_not_fit() {
        let c = cap();
        let times = |own: Option<i64>, holder: Option<i64>| {
            move |_: &str, t: &str| match t {
                "a" => holder,
                "small" => own,
                _ => None,
            }
        };
        let start = |head_gb: u64| {
            let mut l = Ledger::default();
            l.claim(&c, "o", "a", None, &req(9), T0);
            let q = l.claim_timed(
                &c,
                "h",
                "spx",
                None,
                &req(head_gb),
                T0,
                &times(None, Some(660)),
            );
            assert!(matches!(q, Outcome::Queued { position: 1, .. }), "{q:?}");
            l
        };

        // Done (~5m) before the head can start (~11m): granted now.
        let mut l = start(6);
        let g = l.claim_timed(
            &c,
            "s",
            "small",
            None,
            &req(3),
            T0,
            &times(Some(300), Some(660)),
        );
        let Outcome::Backfilled { past, .. } = &g else {
            panic!("{g:?}")
        };
        assert_eq!(past, "spx");
        assert_eq!(g.exit_code(), exit::GRANTED);
        let line = outcome_line("small", &g, &|_, _| None);
        assert_eq!(
            line,
            "granted small: 3G, backfilled ahead of spx, which it does not delay"
        );
        assert_eq!(row_lease(&g, &req(3)).unwrap()["backfilled_past"], "spx");
        assert_eq!(l.ordered_queue()[0].task, "spx", "the head keeps its place");

        // No time of its own, but when the 9G ends there is room for both.
        let mut l = start(6);
        let g = l.claim_timed(&c, "s", "small", None, &req(3), T0, &times(None, Some(660)));
        assert!(matches!(g, Outcome::Backfilled { .. }), "{g:?}");

        // The holder's end unknown: the head may fit any moment, so the
        // small one would delay it and waits — on the head alone.
        let mut l = start(6);
        let q = l.claim_timed(&c, "s", "small", None, &req(3), T0, &times(Some(300), None));
        let Outcome::Queued {
            behind, only_queue, ..
        } = &q
        else {
            panic!("{q:?}")
        };
        assert_eq!((behind.as_deref(), *only_queue), (Some("spx"), true));
        let line = outcome_line("small", &q, &|_, _| None);
        assert!(
            line.starts_with(
                "queued #2 for 3G behind spx's 6G (queued #1), the only thing in the way;"
            ),
            "{line}"
        );

        // A 12G head needs the 9G back and then has no room for 3G more: a
        // small one still going at ~11m would push it back, one done by
        // then would not.
        let mut l = start(12);
        let q = l.claim_timed(
            &c,
            "s",
            "small",
            None,
            &req(3),
            T0,
            &times(Some(1200), Some(660)),
        );
        assert!(matches!(q, Outcome::Queued { .. }), "{q:?}");
        let g = l.claim_timed(
            &c,
            "s",
            "small",
            None,
            &req(3),
            T0,
            &times(Some(300), Some(660)),
        );
        assert!(matches!(g, Outcome::Backfilled { .. }), "{g:?}");

        // A slot the head wants is never backfilled into.
        let mut l = Ledger::default();
        l.claim(&c, "o", "a", None, &req(9), T0);
        let slot = |gb| Request {
            slots: vec!["cargo:/t".into()],
            ..req(gb)
        };
        l.claim(&c, "h", "spx", None, &slot(6), T0);
        let q = l.claim_timed(
            &c,
            "s",
            "small",
            None,
            &slot(3),
            T0,
            &times(Some(300), Some(660)),
        );
        assert!(matches!(q, Outcome::Queued { .. }), "{q:?}");
    }

    /// The head is granted first the moment it fits: once it fits nobody
    /// takes its share, and it is granted as ever, not as a backfill.
    #[test]
    fn a_head_that_fits_goes_first() {
        let c = cap();
        let t = |_: &str, _: &str| Some(60);
        let mut l = Ledger::default();
        l.claim(&c, "o", "a", None, &req(9), T0);
        l.claim_timed(&c, "h", "spx", None, &req(6), T0, &t);
        assert!(l.release("o", "a", T0 + MIN).is_some());
        // 14.85G free: the head fits, so wide's call grants it first
        // (giverny#429), and 10G more does not fit beside its 6G.
        let q = l.claim_timed(&c, "s", "wide", None, &req(10), T0 + MIN, &t);
        let Outcome::Queued {
            behind, only_queue, ..
        } = &q
        else {
            panic!("{q:?}")
        };
        assert_eq!((behind.as_deref(), *only_queue), (Some("spx"), false));
        let line = outcome_line("wide", &q, &|_, _| None);
        assert!(
            line.contains("behind spx (6G; granted, waiting for its manager)"),
            "{line}"
        );
        let g = l.claim_timed(&c, "h", "spx", None, &req(6), T0 + MIN, &t);
        assert!(matches!(g, Outcome::Granted(_)), "{g:?}");
        assert!(l.lease("h", "spx").is_some_and(|x| x.uncollected.is_none()));
    }

    /// Priority still beats first come with backfill on: a later ASAP
    /// claim is ahead of an earlier plain one, which may not take its share.
    #[test]
    fn priority_still_goes_first_with_backfill() {
        let c = cap();
        let t = |_: &str, t: &str| (t == "a").then_some(660);
        let mut l = Ledger::default();
        l.claim(&c, "o", "a", None, &req(9), T0);
        l.claim_timed(&c, "h", "plain", None, &req(8), T0, &t);
        let urgent = Request {
            priority: Some("asap".into()),
            ..req(8)
        };
        let q = l.claim_timed(&c, "u", "urgent", None, &urgent, T0 + MIN, &t);
        assert!(matches!(q, Outcome::Queued { position: 1, .. }), "{q:?}");
        assert!(l.release("o", "a", T0 + 2 * MIN).is_some());
        // Room for one 8G: the plain one, polling first, waits on urgent.
        let q = l.claim_timed(&c, "h", "plain", None, &req(8), T0 + 2 * MIN, &t);
        let Outcome::Queued { behind, .. } = &q else {
            panic!("{q:?}")
        };
        assert_eq!(behind.as_deref(), Some("urgent"));
        let g = l.claim_timed(&c, "u", "urgent", None, &urgent, T0 + 2 * MIN, &t);
        assert!(matches!(g, Outcome::Granted(_)), "{g:?}");
    }

    /// giverny#429, as seen live: one 2G lease, 12.85G free, and three
    /// waiters ahead that all fit — spx-backtest 3G, sim2 2G, clean 1G —
    /// whose managers stopped polling. g422's claim behind them used to
    /// answer "behind spx-backtest's 3G, the only thing in the way" for
    /// hours. Now its call grants all three at their places, then g422.
    #[test]
    fn fitting_waiters_with_silent_owners_are_granted_on_anyones_call() {
        let c = cap();
        let mut l = Ledger::default();
        l.claim(&c, "me", "g429", None, &req(2), T0);
        l.claim(&c, "o", "big", None, &req(12), T0);
        for (s, t, gb) in [
            ("inbar", "spx-backtest", 3),
            ("inbar", "sim2", 2),
            ("tolad", "clean", 1),
        ] {
            let q = l.claim(&c, s, t, None, &req(gb), T0 + MIN);
            assert!(matches!(q, Outcome::Queued { .. }), "{q:?}");
        }
        assert!(l.release("o", "big", T0 + 2 * MIN).is_some());
        // Hours later, their owners have not polled since (their hooks still
        // beat): g422 asks.
        let at = T0 + 3 * 60 * MIN;
        for s in ["inbar", "tolad"] {
            l.heartbeat(s, at - MIN);
        }
        let g = l.claim(&c, "me", "g422", None, &req(3), at);
        assert!(matches!(g, Outcome::Granted(_)), "{g:?}");
        assert!(l.queue.is_empty(), "{:?}", l.queue);
        for t in ["spx-backtest", "sim2", "clean"] {
            let x = l.leases.iter().find(|x| x.task == t).unwrap();
            assert!(x.uncollected.is_some(), "{t} granted for its manager");
            assert_eq!(x.granted_at, at);
        }
        // The panel and the report say so.
        let text = report(&l, &c, at, &|_, _| None);
        assert!(
            text.contains("granted 0m ago, waiting for its manager (lapses in 10m)"),
            "{text}"
        );
        // Its owner's next claim finds it granted, as if just now: exit 0.
        let g = l.claim(&c, "inbar", "spx-backtest", None, &req(3), at + 4 * MIN);
        let Outcome::Granted(x) = &g else {
            panic!("{g:?}")
        };
        assert_eq!(g.exit_code(), exit::GRANTED);
        assert_eq!(
            (x.granted_at, x.uncollected.is_none()),
            (at + 4 * MIN, true)
        );
        assert_eq!(
            outcome_line("spx-backtest", &g, &|_, _| None),
            "granted spx-backtest: 3G"
        );
        // A second claim is the held lease, as ever.
        let h = l.claim(&c, "inbar", "spx-backtest", None, &req(3), at + 5 * MIN);
        assert!(matches!(h, Outcome::Held(_)), "{h:?}");
    }

    /// giverny#429: a grant nobody takes up lapses after the setting's
    /// minutes; the next waiter gets what it held, at once, on one call.
    /// The lapsed one keeps its place, dormant, until its manager claims.
    #[test]
    fn an_uncollected_grant_lapses_and_the_next_waiter_gets_it() {
        let c = cap();
        let mut l = Ledger::default();
        l.claim(&c, "o", "big", None, &req(9), T0);
        l.claim(&c, "h", "spx", None, &req(8), T0);
        l.claim(&c, "d", "g422", None, &req(8), T0 + MIN);
        assert!(l.release("o", "big", T0 + 2 * MIN).is_some());
        // Anyone's call (a third session's beat) grants the head.
        let n = l.advance(&c, T0 + 2 * MIN, &|_, _| None, None);
        assert_eq!(n.granted.len(), 1);
        assert!(l.lease("h", "spx").is_some_and(|x| x.uncollected.is_some()));
        // Its manager is alive (its hook beats) but never claims.
        l.heartbeat("h", T0 + 8 * MIN);
        let q = l.claim(&c, "d", "g422", None, &req(8), T0 + 9 * MIN);
        assert!(matches!(q, Outcome::Queued { .. }), "{q:?}");
        // Ten minutes after the grant, g422's one call takes it.
        let g = l.claim(&c, "d", "g422", None, &req(8), T0 + 12 * MIN);
        assert!(matches!(g, Outcome::Granted(_)), "{g:?}");
        assert!(l.lease("h", "spx").is_none());
        let w = l.queue.iter().find(|w| w.task == "spx").unwrap();
        assert_eq!(w.queued_at, T0, "back at its place");
        assert!(w.lapsed.is_some());
        // Dormant: nothing set aside for it, never granted again unasked.
        assert!(l.release("d", "g422", T0 + 13 * MIN).is_some());
        assert!(!l.advance(&c, T0 + 13 * MIN, &|_, _| None, None).moved());
        let g = l.claim(&c, "x", "small", None, &req(10), T0 + 13 * MIN);
        assert!(matches!(g, Outcome::Granted(_)), "{g:?}");
        let text = report(&l, &c, T0 + 13 * MIN, &|_, _| None);
        assert!(text.contains("its grant lapsed 1m ago untaken"), "{text}");
        // Its manager comes back: live again, at its old place.
        let q = l.claim(&c, "h", "spx", None, &req(8), T0 + 14 * MIN);
        assert!(matches!(q, Outcome::Queued { position: 1, .. }), "{q:?}");
        assert!(l.queue[0].lapsed.is_none());
    }

    /// giverny#429: a session no claude runs, that has not beaten for
    /// [`GONE_MS`], leaves the queue; its untaken grant is dropped, not put
    /// back. Without the registry's word, only the TTL says so.
    #[test]
    fn a_dead_sessions_waiters_drop_out() {
        let mut c = cap();
        let mut l = Ledger::default();
        l.claim(&c, "o", "big", None, &req(14), T0);
        l.claim(&c, "dead", "w", None, &req(2), T0);
        l.claim(&c, "live", "v", None, &req(2), T0);
        let later = T0 + GONE_MS;
        l.heartbeat("o", later);
        assert!(
            !l.advance(&c, later, &|_, _| None, None).moved(),
            "no registry"
        );
        c.live_sessions = Some(["o", "live"].into_iter().map(String::from).collect());
        let n = l.advance(&c, later, &|_, _| None, None);
        assert_eq!(n.dropped, vec!["dead:w".to_string()]);
        assert_eq!(l.queue.len(), 1);
        // A grant its gone session never took up is dropped when it lapses.
        assert!(l.release("o", "big", later).is_some());
        l.advance(&c, later, &|_, _| None, None);
        assert!(
            l.lease("live", "v")
                .is_some_and(|x| x.uncollected.is_some())
        );
        c.live_sessions = Some(["o"].into_iter().map(String::from).collect());
        let n = l.advance(&c, later + c.uncollected_ms(), &|_, _| None, None);
        assert_eq!(n.dropped, vec!["live:v".to_string()]);
        assert!(l.leases.is_empty() && l.queue.is_empty());
    }

    /// giverny#429: the hook tells a manager once what the queue did for
    /// it; `start` (or `run`) takes a grant up as a claim does.
    #[test]
    fn news_is_told_once_and_start_collects() {
        let dir = std::env::temp_dir().join(format!("giverny-news-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("ledger.json");
        let c = cap();
        with_ledger(&path, T0, |l| {
            l.claim(&c, "o", "big", None, &req(14), T0);
            l.claim(&c, "h", "spx", None, &req(3), T0);
        })
        .unwrap();
        assert!(news(&path, "h", T0).is_none());
        release_at(&path, None, "o", "big", T0 + MIN, Some(&c)).unwrap();
        let said = news(&path, "h", T0 + 2 * MIN).unwrap();
        assert!(
            said.contains("granted your queued claim for spx (3G) 1m ago"),
            "{said}"
        );
        assert!(news(&path, "h", T0 + 2 * MIN).is_none(), "told once");
        assert!(news(&path, "o", T0 + 2 * MIN).is_none());
        assert!(collect_at(&path, "h", "spx", T0 + 3 * MIN).unwrap());
        assert!(!collect_at(&path, "h", "spx", T0 + 3 * MIN).unwrap());
        let l = Ledger::parse(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(l.leases[0].granted_at, T0 + 3 * MIN);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_held_lease_shrinks_in_place_never_grows() {
        let c = cap();
        let mut l = Ledger::default();
        l.claim(&c, "a", "t", None, &req(3), T0);
        let (was, now) = l.shrink("a", "t", Some(1024), None, T0).unwrap();
        assert_eq!((was.ram_mb, now.ram_mb), (3072, 1024));
        assert!(l.shrink("a", "t", Some(2048), None, T0).is_none());
        assert!(l.shrink("a", "t", None, None, T0).is_none());
    }

    #[test]
    fn the_file_round_trips_under_the_lock() {
        let dir = std::env::temp_dir().join(format!("giverny-ledger-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("resources").join("ledger.json");
        let c = cap();
        let out = with_ledger(&path, T0, |l| {
            l.claim(&c, "a", "t", Some("demo"), &req(3), T0)
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
        heartbeat(&path, None, "a", T0 + 10 * MIN, Some(&c)).unwrap();
        let l = Ledger::parse(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(l.leases[0].heartbeat_at, T0 + 10 * MIN);
        let n = with_ledger(&path, T0 + 10 * MIN + TTL_MS, |l| l.leases.len()).unwrap();
        assert_eq!(n, 0);
        assert!(
            release_at(&path, None, "a", "t", T0, Some(&c))
                .unwrap()
                .is_none()
        );
        // Many writers at once lose nothing.
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                let c = c.clone();
                std::thread::spawn(move || {
                    with_ledger(&path, T0, |l| {
                        l.claim(&c, &format!("s{i}"), "t", None, &req(1), T0)
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

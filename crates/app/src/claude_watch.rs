//! App-side Claude awareness: merges the hook relay stream with the
//! `sessions/<pid>.json` registry into per-tab states, and refreshes the
//! per-account usage meters.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use giverny_claude::attach;
use giverny_claude::hooks::{self, RelayMsg};
use giverny_claude::jobs::{self, Job};
use giverny_claude::profiles::{self, Profile};
use giverny_claude::registry;
use giverny_claude::usage::{self, AccountUsage};
use giverny_claude::wsl;
use giverny_core::tabs::TabId;

use crate::agents_live::AgentsLive;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClaudeState {
    /// No Claude running in this tab.
    #[default]
    None,
    /// Claude open, waiting at its prompt.
    Idle,
    /// Claude is working.
    Busy,
    /// Claude needs the user (permission / question / agent input).
    NeedsYou,
    /// Claude finished while the tab was in the background.
    DoneUnseen,
}

#[derive(Debug, Clone, Default)]
pub struct ClaudeTab {
    pub state: ClaudeState,
    pub session_id: Option<String>,
    pub session_name: Option<String>,
    /// Short account name (profile) this tab's Claude runs under.
    pub account: Option<String>,
    /// A background shell is alive in this session while the agent itself is
    /// at its prompt. Not a working state — marked, never animated.
    pub background: bool,
    /// The user's most recent prompt in this session, in full: from the
    /// `UserPromptSubmit` hook, or read back from the transcript for a
    /// session no hook has reported a prompt for.
    pub last_prompt: Option<String>,
    last_hook: Option<Instant>,
    seen_in_scan: bool,
}

impl ClaudeTab {
    /// Is a claude running in this tab now: seen in the last scan, or heard
    /// from in the last few seconds.
    fn has_claude(&self) -> bool {
        self.seen_in_scan
            || self
                .last_hook
                .is_some_and(|t| t.elapsed() < Duration::from_secs(5))
    }

    /// The conversation this tab holds. A different one — `/clear`, a
    /// `/resume`, a new `claude` — has not been asked anything yet, as far as
    /// this tab knows.
    fn set_session(&mut self, session: Option<String>) {
        if self.session_id != session {
            self.last_prompt = None;
        }
        self.session_id = session;
    }
}

pub struct AccountPanel {
    pub profile: Profile,
    pub usage: Option<AccountUsage>,
    /// Fresher percentages pushed by the statusline (official `rate_limits`),
    /// overriding the on-disk cache for the windows they cover.
    pub live: Option<LiveUsage>,
    pub statusline_on: bool,
    /// The most any window has been used, for as long as that window lasts.
    /// Keyed by `LimitEntry::kind`.
    pub peak: HashMap<String, Peak>,
}

/// The high-water mark of one window.
///
/// Usage within a window only ever goes up, but the two sources it can be read
/// from disagree: the status line reports the turn it is in, while the cache
/// holds whatever `/usage` last fetched, which can be minutes behind and is
/// then written with a *fresh* timestamp. Reading "whichever was sampled last"
/// therefore walks backwards, and the bar bounces between two numbers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peak {
    pub percent: f64,
    /// The window this belongs to. A different reset is a different window,
    /// and the mark starts again.
    pub resets: Option<jiff::Timestamp>,
}

/// Where a reading falls against a high-water mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Window {
    /// The mark's own window: the reading can only raise it.
    Same,
    /// A window that closed before the mark's opened — a number nothing
    /// current should be showing.
    Older,
    /// A later window, or one the mark cannot vouch for: start again.
    Other,
}

impl Peak {
    /// Two reset times this close apart name the same window. The sources do
    /// not spell it the same way — the push says `17:30:00`, the cache
    /// `17:30:00.062934` — and a window never renews less than five hours
    /// after the last one did, so an hour is slack, not ambiguity.
    const SAME_WINDOW_SECS: i64 = 3600;
    /// Without a reset time to go by, a number that has fallen this far below
    /// the mark is a new window rather than a source that is behind.
    const A_RESET_NOT_A_DISAGREEMENT: f64 = 25.0;

    /// Which window `read` is from, as far as this mark can tell.
    ///
    /// Every running `claude` pushes the percentage *its own* last request
    /// was answered with, and a session that has sat idle for an hour pushes
    /// an hour-old number — with the current window's reset time beside it,
    /// because that has not changed. A known reset time is what identifies
    /// the window, so a number that is merely behind never ends the mark,
    /// however far behind it is.
    fn place(&self, read: &Reading, now: jiff::Timestamp) -> Window {
        match (self.resets, read.resets) {
            // The mark's window is over; whatever comes next starts afresh.
            (Some(mine), _) if mine <= now => Window::Other,
            (Some(mine), Some(theirs)) => {
                let apart = theirs.as_second() - mine.as_second();
                if apart.abs() < Self::SAME_WINDOW_SECS {
                    Window::Same
                } else if apart < 0 {
                    Window::Older
                } else {
                    Window::Other
                }
            }
            // A mark with no reset time and a reading that has one: the
            // reading knows more.
            (None, Some(_)) => Window::Other,
            (_, None) if read.percent + Self::A_RESET_NOT_A_DISAGREEMENT < self.percent => {
                Window::Other
            }
            (_, None) => Window::Same,
        }
    }
}

/// One usage bar's numbers, taken from whichever source is freshest.
///
/// The cache and the statusline push disagree whenever the cache has stopped
/// being refreshed, and they have to be read as a set: a percentage from the
/// push beside a severity from the cache is how a week 21% used came up red,
/// the cache still holding the last thing it managed to fetch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    pub percent: f64,
    /// The percentage came from a statusline push rather than the cache.
    pub live: bool,
    /// Out, or nearly. Red.
    pub critical: bool,
    /// When the window renews, if anything still knows.
    pub resets: Option<jiff::Timestamp>,
}

/// Where an account's displayed numbers came from, and how old they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Pushed by the statusline this many minutes ago.
    Live(i64),
    /// Read from Claude Code's on-disk cache, this many minutes old.
    Cache(i64),
    None,
}

/// Push-based usage from Claude Code's statusline payload.
#[derive(Debug, Clone)]
pub struct LiveUsage {
    pub at: Instant,
    pub five_hour: Option<f64>,
    pub seven_day: Option<f64>,
    /// When each window reopens, if the push says. The on-disk cache carries
    /// this too, but a cache older than the window it describes has a reset
    /// time in the past — and a lapsed reset time is no reset time at all,
    /// which is how a live 90% ends up with nothing next to it.
    pub five_hour_resets: Option<jiff::Timestamp>,
    pub seven_day_resets: Option<jiff::Timestamp>,
}

/// Side effects for the app to apply after a tick.
#[derive(Default)]
pub struct WatchEffects {
    /// `(tab, session_id, config_dir)` — `None` session means it ended.
    pub captured: Vec<(TabId, Option<String>, Option<PathBuf>)>,
    /// Desktop notifications to fire: `(summary, body)`.
    pub notify: Vec<(String, String)>,
}

pub struct ClaudeWatch {
    pub profiles: Vec<Profile>,
    /// Accounts with a refresh currently running, so we never stack them.
    refreshing: Arc<Mutex<HashSet<PathBuf>>>,
    /// When we last *asked* for a refresh, successful or not. Age alone can't
    /// gate the sweep: an account whose cache never appears (logged out, no
    /// `claude` on PATH) reads as infinitely old and would be retried on every
    /// tick forever.
    attempted: Arc<Mutex<HashMap<PathBuf, Instant>>>,
    /// Set by a refresh thread when it finishes, so the panel picks up the new
    /// numbers on the next frame instead of waiting out the read interval.
    cache_dirty: Arc<AtomicBool>,
    pub tabs: HashMap<TabId, ClaudeTab>,
    /// Background agents across every account — the Claudes with no tab.
    /// One a tab shows is left out ([`ClaudeWatch::shown_in_a_tab`]).
    pub jobs: Vec<Job>,
    /// The job each tab's claude parked on (`parkedJobId`), from the last
    /// registry scan: that claude is a client of the background daemon.
    parked: HashMap<TabId, String>,
    /// The session of each tab's parked claude itself — the client, not
    /// the job — from the same scan.
    parked_by: HashMap<TabId, String>,
    /// Every job of the last jobs scan, shown or not.
    all_jobs: Vec<Job>,
    /// What each tab's `claude attach` names (an id, a conversation, a
    /// name), from the last process scan: that claude shows the job and
    /// writes no registry entry to say so (giverny#243).
    attached: HashMap<TabId, String>,
    /// Which tab shows each background job now, by the job's short id
    /// ([`jobs_on_screen`]). A job's hooks carry its id, not a tab's
    /// (giverny#242).
    viewing: HashMap<String, TabId>,
    pub accounts: Vec<AccountPanel>,
    pub hooks_installed: bool,
    hook_rx: Option<Receiver<RelayMsg>>,
    last_scan: Instant,
    /// The last registry scan, and whether every live session predates the
    /// settings file. Both are filesystem work — for an account inside WSL,
    /// filesystem work across a share — so they happen on a worker and the UI
    /// reads whatever it last said.
    scan_rx: Option<crossbeam_channel::Receiver<ScanResult>>,
    scanned: ScanResult,
    last_jobs: Instant,
    last_usage: Instant,
    /// When each account's cache file was last seen changing, so a rewrite is
    /// noticed rather than waited out.
    cache_mtimes: Arc<Mutex<HashMap<PathBuf, std::time::SystemTime>>>,
    last_cache_stat: Instant,
    /// One stat sweep at a time: a share that is slow to answer must not
    /// stack up threads behind it.
    stat_in_flight: Arc<AtomicFlag>,
    /// What a later, warmer discovery found, the one worker looking, and when
    /// it last looked.
    late: Arc<Mutex<Option<Vec<Profile>>>>,
    last_look: Instant,
    late_in_flight: Arc<AtomicFlag>,
    extra_dirs: Vec<PathBuf>,
    /// Each tab's subagents, from relayed `subagentStatusLine` ticks — what
    /// the agents pane draws. See [`AgentsLive::tracker`].
    pub agents: AgentsLive,
    /// A side instance (`GIVERNY_NO_ACCOUNT_SETUP`): read the accounts, never
    /// write them. See [`leaves_accounts_alone`].
    leave_accounts: bool,
    /// Sessions whose transcript has been asked for its last prompt, so each
    /// is read once, and what the readers found: `(session, prompt)`.
    prompts_asked: HashSet<String>,
    prompts_found: (
        crossbeam_channel::Sender<FoundPrompt>,
        crossbeam_channel::Receiver<FoundPrompt>,
    ),
}

/// `(session, prompt)`, read from a transcript.
type FoundPrompt = (String, String);

/// The environment variable that makes this a side instance.
pub const NO_ACCOUNT_SETUP_ENV: &str = "GIVERNY_NO_ACCOUNT_SETUP";

/// Is this a side instance that must leave every account's Claude config
/// alone?
///
/// Each account's `settings.json` names one Giverny binary for its hooks,
/// status lines and plugin, and every Giverny adopts the accounts it finds:
/// it points those entries at *its own* executable and follows *its own*
/// `agents_pane` setting. A second Giverny started to test a build — even
/// with a config and runtime dir of its own — therefore re-points the live
/// instance's sessions at the test binary, or strips the subagent line and
/// the plugin (and `plugins/known_marketplaces.json`) out from under it.
///
/// `GIVERNY_NO_ACCOUNT_SETUP=1` turns all of that off: hooks, status lines,
/// auto mode, the subagent line and the plugin are read but never written,
/// at startup or from the UI. Set and not empty or `0` counts as on.
///
/// An explicit switch rather than a guess: a test build differs from the
/// installed one only in its path, which is exactly what a real reinstall
/// (`cargo install`, a moved build) also changes, and the path refresh
/// exists to follow that.
pub fn leaves_accounts_alone(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// What a write refused by a side instance reports, for the UI's log line.
const LEFT_ALONE: &str = "side instance (GIVERNY_NO_ACCOUNT_SETUP): account settings left alone";

/// How often the on-disk usage caches are re-read. The numbers inside them
/// only move when Claude Code fetches (minutes apart), and anything faster —
/// a statusline push, a finished refresh — updates the panel directly, so
/// polling harder buys nothing.
const USAGE_READ_INTERVAL: Duration = Duration::from_secs(60);
/// How often the cache files are checked for having been rewritten.
const CACHE_STAT_INTERVAL: Duration = Duration::from_secs(2);
/// How often accounts are looked for again while none inside WSL is known.
const LOOK_AGAIN_INTERVAL: Duration = Duration::from_secs(45);

/// A bool two threads share. `AtomicBool` in a name that says what it is for.
#[derive(Default)]
pub struct AtomicFlag(AtomicBool);

impl AtomicFlag {
    fn get(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    fn set(&self, value: bool) {
        self.0.store(value, Ordering::Relaxed);
    }
}

fn needs_you(notification_type: &str) -> bool {
    matches!(
        notification_type,
        "permission_prompt" | "elicitation_dialog" | "agent_needs_input"
    )
}

/// The session is back at its prompt. Only `idle_prompt` says that.
fn idle_kind(notification_type: &str) -> bool {
    notification_type == "idle_prompt"
}

/// A *piece* of work finished — a subagent, a task. Worth a done-marker in a
/// background tab, but it is not evidence the session stopped: these fire
/// mid-turn, while the main agent carries on with the result. Treating them
/// as "finished" is what used to kill the spinner half way through the work.
fn finished_kind(notification_type: &str) -> bool {
    matches!(notification_type, "agent_completed" | "task_completed")
}

/// One tab's state, reconciled with what Claude Code says about that session
/// right now.
///
/// Hooks and the registry answer different questions. Hooks bracket a *turn*;
/// the registry says what the session is doing right now, including the one
/// state no hook marks the start of: blocked on the user (`waiting`).
fn merge_registry(
    current: ClaudeState,
    hooks_own: bool,
    live: &giverny_claude::registry::SessionEntry,
) -> ClaudeState {
    // Working is unambiguous evidence Claude is running again, so it always
    // clears a stale flag — even under hook authority. A declined permission
    // prompt emits no hook to clear the one it raised, and a turn that ended
    // before this one left a tick behind that "finished" no longer describes.
    if live.busy() && matches!(current, ClaudeState::NeedsYou | ClaudeState::DoneUnseen) {
        return ClaudeState::Busy;
    }
    if hooks_own {
        // Hooks are authoritative for a session that emits them: they mark
        // the end of a turn exactly, and the registry must not re-open one it
        // closed.
        return current;
    }
    match current {
        // Nothing here has heard from a hook, so these are ours to keep until
        // the user attends to them.
        ClaudeState::NeedsYou | ClaudeState::DoneUnseen => current,
        _ if live.busy() => ClaudeState::Busy,
        // Blocked on the user with no hook to say so — the state a session
        // started before hooks were installed would otherwise sit silent in.
        _ if live.waiting() => ClaudeState::NeedsYou,
        _ => ClaudeState::Idle,
    }
}

/// One pass over the session registries, done on a worker.
#[derive(Default)]
struct ScanResult {
    live: Vec<registry::LiveSession>,
    /// What each tab's `claude attach` names, for a tab running one
    /// ([`attach::under`]).
    attached: HashMap<TabId, String>,
}

/// When one rate-limit window resets, out of a statusline push.
///
/// The field has been spelled more than one way across Claude Code versions,
/// and a moment is written variously: an RFC 3339 string, epoch seconds, or
/// epoch milliseconds. Read whichever one is there.
fn reset_time(window: &serde_json::Value) -> Option<jiff::Timestamp> {
    for name in ["resets_at", "reset_at", "resets_at_ms", "reset_at_ms"] {
        let Some(value) = window.get(name) else {
            continue;
        };
        if let Some(text) = value.as_str()
            && let Ok(at) = text.parse::<jiff::Timestamp>()
        {
            return Some(at);
        }
        if let Some(number) = value.as_i64() {
            // Milliseconds if it is far too large to be seconds.
            let millis = if number > 100_000_000_000 {
                number
            } else {
                number * 1000
            };
            if let Ok(at) = jiff::Timestamp::from_millisecond(millis) {
                return Some(at);
            }
        }
    }
    None
}

/// Which tab shows each background job, by the job's short id.
///
/// A claude parked on a job (`parked`, by tab) is a client of the daemon,
/// and Claude Code's agents view switches it between jobs without saying so
/// anywhere on disk: `parkedJobId` keeps naming the first. What does follow
/// the switch is the tab's title, which Claude Code sets to the shown
/// session's name. So: the one live job named as the title says, else the
/// parked one; of several by that name, the parked one or else the most
/// recently active. A placeholder (`placeholders`) is never the fallback: a
/// tab whose title names no job shows none.
pub fn jobs_on_screen(
    parked: &HashMap<TabId, String>,
    titles: &HashMap<TabId, String>,
    jobs: &[Job],
    placeholders: &HashSet<String>,
) -> HashMap<String, TabId> {
    let mut out = HashMap::new();
    for (&tab, parked_on) in parked {
        let title = titles.get(&tab).map(|t| bare_title(t)).unwrap_or_default();
        let named: Vec<&Job> = jobs
            .iter()
            .filter(|j| j.live && !title.is_empty() && j.name == title)
            .collect();
        let shown = named
            .iter()
            .find(|j| &j.id == parked_on)
            .or(named.first())
            .map(|j| j.id.clone())
            .or_else(|| (!placeholders.contains(parked_on)).then(|| parked_on.clone()));
        if let Some(shown) = shown {
            out.insert(shown, tab);
        }
    }
    out
}

/// A tab title without the status glyph Claude Code puts before the
/// session's name (`✳`, `◐`, a braille spinner).
fn bare_title(title: &str) -> &str {
    title
        .trim_start_matches(|c: char| c.is_whitespace() || (!c.is_ascii() && !c.is_alphanumeric()))
        .trim_end()
}

impl ClaudeWatch {
    /// `config_read` is false when Giverny's config file could not be parsed:
    /// then the startup pass that brings accounts' hook paths and statusline
    /// up to date is skipped, since the settings it would follow are
    /// defaults standing in for the unreadable file.
    pub fn new(
        spool: &Path,
        extra_dirs: &[PathBuf],
        config_read: bool,
        wake: impl Fn() + Send + 'static,
    ) -> (Self, Vec<RelayMsg>) {
        let profiles = profiles::discover(extra_dirs);
        // Unix: a socket for instant delivery. Elsewhere (or if binding
        // fails): poll the spool file the relay always falls back to.
        #[cfg(unix)]
        let listener = hooks::spawn_listener(spool, wake);
        #[cfg(not(unix))]
        let listener = hooks::spawn_spool_watcher(spool, wake);
        let (hook_rx, spooled) = match listener {
            Ok((rx, spooled)) => (Some(rx), spooled),
            Err(err) => {
                tracing::warn!("hook listener unavailable: {err:#}");
                (None, Vec::new())
            }
        };

        let leave_accounts =
            leaves_accounts_alone(std::env::var_os(NO_ACCOUNT_SETUP_ENV).as_deref());
        if leave_accounts {
            tracing::info!("{LEFT_ALONE}");
        } else {
            // Before anything is written: what is written names the link.
            #[cfg(unix)]
            if let Err(err) = hooks::point_link() {
                tracing::warn!("giverny link not pointed here: {err}");
            }
        }
        Self::adopt_statusline_where_hooked(&profiles, leave_accounts || !config_read);
        let mut watch = ClaudeWatch {
            refreshing: Arc::new(Mutex::new(HashSet::new())),
            attempted: Arc::new(Mutex::new(HashMap::new())),
            cache_dirty: Arc::new(AtomicBool::new(false)),
            hooks_installed: Self::check_installed(&profiles),
            profiles,
            tabs: HashMap::new(),
            jobs: Vec::new(),
            parked: HashMap::new(),
            parked_by: HashMap::new(),
            all_jobs: Vec::new(),
            attached: HashMap::new(),
            viewing: HashMap::new(),
            accounts: Vec::new(),
            hook_rx,
            last_scan: Instant::now() - Duration::from_secs(10),
            scan_rx: None,
            scanned: ScanResult::default(),
            last_jobs: Instant::now() - Duration::from_secs(10),
            last_usage: Instant::now() - USAGE_READ_INTERVAL,
            cache_mtimes: Arc::new(Mutex::new(HashMap::new())),
            last_cache_stat: Instant::now() - CACHE_STAT_INTERVAL,
            stat_in_flight: Arc::new(AtomicFlag::default()),
            late: Arc::new(Mutex::new(None)),
            last_look: Instant::now(),
            late_in_flight: Arc::new(AtomicFlag::default()),
            extra_dirs: extra_dirs.to_vec(),
            // Beside the spool: Giverny's own state dir.
            agents: AgentsLive::load(
                spool
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join("agents.json"),
            ),
            leave_accounts,
            prompts_asked: HashSet::new(),
            prompts_found: crossbeam_channel::unbounded(),
        };
        // Before any tab is restored: a tab holding a running job's
        // conversation attaches to the job, which takes knowing the jobs
        // ([`ClaudeWatch::live_job_holding`]).
        watch.all_jobs = jobs::scan(watch.profiles.iter().map(|p| p.config_dir.clone()));
        watch.refresh_usage();
        (watch, spooled)
    }

    fn check_installed(profiles: &[Profile]) -> bool {
        !profiles.is_empty()
            && profiles
                .iter()
                .all(|p| hooks::installed_in(&p.config_dir.join("settings.json")))
    }

    pub fn install_hooks(&mut self) -> Result<usize, String> {
        if self.leave_accounts {
            return Err(LEFT_ALONE.into());
        }
        let mut ok = 0;
        let mut errs = Vec::new();
        for p in &self.profiles {
            let settings = p.config_dir.join("settings.json");
            match hooks::install_into(&settings) {
                Ok(_) => ok += 1,
                Err(e) => errs.push(format!("{}: {e}", p.name)),
            }
            // Live usage comes with it — the on-disk cache goes stale for
            // accounts that aren't actively running Claude. Profiles with a
            // statusline of their own are left alone (set_statusline errs).
            if let Err(e) = hooks::set_statusline(&settings, true) {
                tracing::info!("statusline skipped for {}: {e}", p.name);
            }
        }
        self.hooks_installed = Self::check_installed(&self.profiles);
        self.refresh_usage();
        if errs.is_empty() {
            Ok(ok)
        } else {
            Err(errs.join("; "))
        }
    }

    /// Profiles that already have our hooks get the live-usage statusline
    /// too: installing hooks is the consent boundary, and without this the
    /// usage panel silently shows day-old numbers.
    fn adopt_statusline_where_hooked(profiles: &[Profile], leave_accounts: bool) {
        if leave_accounts {
            return;
        }
        for p in profiles {
            let settings = p.config_dir.join("settings.json");
            if !hooks::installed_in(&settings) {
                // Installed once, but not for everything we listen to now: a
                // new event in a new version. Consent was given; bring the
                // file up to date rather than asking again.
                if hooks::partly_installed_in(&settings) {
                    match hooks::install_into(&settings) {
                        Ok(_) => tracing::info!("hooks brought up to date for {}", p.name),
                        Err(e) => tracing::warn!("hook update failed for {}: {e}", p.name),
                    }
                }
                continue;
            }
            // Our entries point at whichever binary installed them. After a
            // `cargo install` or a moved build, that path can be stale —
            // rewrite it to the running executable so the relay keeps working.
            if hooks::needs_path_refresh(&settings) {
                match hooks::install_into(&settings) {
                    Ok(_) => tracing::info!("hook paths refreshed for {}", p.name),
                    Err(e) => tracing::warn!("hook refresh failed for {}: {e}", p.name),
                }
            }
            if !hooks::statusline_installed_in(&settings) || hooks::needs_path_refresh(&settings) {
                match hooks::set_statusline(&settings, true) {
                    Ok(()) => tracing::info!("live-usage statusline enabled for {}", p.name),
                    Err(e) => tracing::info!("statusline skipped for {}: {e}", p.name),
                }
            }
        }
    }

    pub fn tab_id_of(msg: &RelayMsg) -> Option<TabId> {
        let raw = msg.tab_id.as_deref()?;
        raw.strip_prefix("giverny-")?.parse::<u64>().ok().map(TabId)
    }

    /// The tab a message is for: the one it names, or the one showing the
    /// background job it came from.
    pub fn tab_of(&self, msg: &RelayMsg) -> Option<TabId> {
        Self::tab_id_of(msg).or_else(|| self.viewing.get(msg.job.as_deref()?).copied())
    }

    /// The job each tab shows, by what runs in it: a claude parked on one
    /// (`parkedJobId`), a `claude attach` (its argument resolved against
    /// `jobs`, as Claude Code resolves it), or — for a tab opened from
    /// BACKGROUND (`opened`, its `bg_job`) with no claude seen in it yet —
    /// the job it was opened for. [`jobs_on_screen`] then follows the title.
    fn jobs_by_tab(&self, opened: &HashMap<TabId, String>, jobs: &[Job]) -> HashMap<TabId, String> {
        let mut on = self.parked.clone();
        for (&tab, target) in &self.attached {
            if let Some(job) = attach::resolve(target, jobs) {
                on.insert(tab, job.id.clone());
            }
        }
        for (&tab, job) in opened {
            if !self.tabs.get(&tab).is_some_and(|t| t.seen_in_scan) {
                on.entry(tab).or_insert_with(|| job.clone());
            }
        }
        on
    }

    /// Does a tab show this background job — viewing it, or holding its
    /// conversation? Then it is the tab's, not one more agent in the
    /// background list (giverny#243).
    ///
    /// Only a tab with a claude in it now holds a conversation: one that
    /// detached from the job keeps the id it last held, and the job is back
    /// in the background.
    fn shown_in_a_tab(&self, job: &Job) -> bool {
        self.viewing.contains_key(&job.id)
            || self.tabs.values().any(|tab| {
                tab.has_claude()
                    && tab.session_id.as_deref().is_some_and(|sid| {
                        job.session_id.as_deref() == Some(sid)
                            || job.resume_session_id.as_deref() == Some(sid)
                    })
            })
    }

    fn account_of(&self, config_dir: Option<&Path>) -> Option<String> {
        let dir = config_dir?;
        profiles::find(&self.profiles, dir).map(|p| p.name.clone())
    }

    /// The profile directory a session means by the `CLAUDE_CONFIG_DIR` it
    /// reports. A session inside WSL reports the path it can open
    /// (`/home/x/.claude`); profiles here are keyed by the path Windows can
    /// open. Anything else passes through unchanged.
    fn canonical_dir(&self, reported: Option<&str>) -> Option<PathBuf> {
        let reported = PathBuf::from(reported?);
        let known: Vec<PathBuf> = self.profiles.iter().map(|p| p.config_dir.clone()).collect();
        Some(wsl::canonical_config_dir(&reported, &known).unwrap_or(reported))
    }

    /// Apply one hook message. `active` = the currently focused tab.
    pub fn handle_msg(
        &mut self,
        msg: &RelayMsg,
        active: Option<TabId>,
        tab_title: &str,
        effects: &mut WatchEffects,
    ) {
        // Statusline pushes carry usage, not tab state.
        if msg.hook_event() == Some(hooks::STATUSLINE_EVENT) {
            self.apply_statusline(msg);
            return;
        }
        // Subagent-line ticks carry a tab's live workers, not its state.
        if msg.hook_event() == Some(hooks::SUBAGENT_LINE_EVENT) {
            if let Some(tab_id) = self.tab_of(msg) {
                let config_dir = self.canonical_dir(msg.config_dir.as_deref());
                self.agents.apply_live(tab_id, config_dir, &msg.event);
            }
            return;
        }
        // `giverny orchestrator-session clear-done`: the tab's Done rows, cleared by hand.
        if msg.hook_event() == Some(hooks::CLEAR_DONE_EVENT) {
            if let Some(tab_id) = self.tab_of(msg) {
                let at = msg.event.get("at_ms").and_then(|v| v.as_u64());
                self.agents.clear_done(tab_id, at);
            }
            return;
        }
        let Some(tab_id) = self.tab_of(msg) else {
            return;
        };
        let config_dir = self.canonical_dir(msg.config_dir.as_deref());
        let account = self.account_of(config_dir.as_deref());
        let entry = self.tabs.entry(tab_id).or_default();
        entry.last_hook = Some(Instant::now());
        if account.is_some() {
            entry.account = account;
        }
        let is_active = active == Some(tab_id);

        match msg.hook_event() {
            Some("SessionStart") => {
                entry.state = ClaudeState::Idle;
                entry.set_session(msg.session_id().map(str::to_string));
                self.agents.session_started(
                    tab_id,
                    msg.event.get("source").and_then(|v| v.as_str()),
                    msg.session_id(),
                    config_dir.clone(),
                );
                effects
                    .captured
                    .push((tab_id, msg.session_id().map(str::to_string), config_dir));
            }
            Some("UserPromptSubmit") => {
                entry.state = ClaudeState::Busy;
                if let Some(prompt) = msg.prompt().map(str::trim).filter(|p| !p.is_empty()) {
                    entry.last_prompt = Some(prompt.to_string());
                }
            }
            // A tool call is work happening now, whoever asked for it. It is
            // what tells a tab apart from the turn that ended before it: a
            // session carrying on after a permission was granted, or an agent
            // continuing by itself, emits nothing else.
            Some("PostToolUse") => entry.state = ClaudeState::Busy,
            Some("Stop") => {
                entry.state = if is_active {
                    ClaudeState::Idle
                } else {
                    ClaudeState::DoneUnseen
                };
            }
            Some("Notification") => {
                if let Some(kind) = msg.notification_type() {
                    if needs_you(kind) {
                        entry.state = ClaudeState::NeedsYou;
                        effects.notify.push((
                            format!("{tab_title} — needs you"),
                            msg.message()
                                .unwrap_or("Claude is waiting for input")
                                .to_string(),
                        ));
                    } else if idle_kind(kind) {
                        entry.state = if is_active {
                            ClaudeState::Idle
                        } else {
                            ClaudeState::DoneUnseen
                        };
                    } else if finished_kind(kind) {
                        // A subagent or task finished. Only meaningful if the
                        // session itself is not working — otherwise the main
                        // agent is still going and the spinner stays.
                        if entry.state != ClaudeState::Busy && !is_active {
                            entry.state = ClaudeState::DoneUnseen;
                        }
                    }
                }
            }
            Some("SessionEnd") => {
                self.agents.session_ended(tab_id);
                entry.state = ClaudeState::None;
                entry.set_session(None);
                entry.session_name = None;
                effects.captured.push((tab_id, None, None));
            }
            _ => {}
        }
    }

    /// Periodic merge: hook stream + registry scan + usage refresh.
    /// `shell_pids` maps tabs to their shell process ids.
    pub fn tick(
        &mut self,
        shell_pids: &HashMap<TabId, u32>,
        active: Option<TabId>,
        titles: &HashMap<TabId, String>,
        opened: &HashMap<TabId, String>,
    ) -> WatchEffects {
        let mut effects = WatchEffects::default();
        self.remember_peaks();

        // Hook stream first (crisp transitions).
        let msgs: Vec<RelayMsg> = self
            .hook_rx
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for msg in &msgs {
            let title = self
                .tab_of(msg)
                .and_then(|id| titles.get(&id).cloned())
                .unwrap_or_else(|| "tab".into());
            self.handle_msg(msg, active, &title, &mut effects);
        }
        // An empty title map is a workspace not built yet, not one with no
        // tabs: keep every tracker until there is something to compare with.
        if !titles.is_empty() {
            self.agents.tick(|id| titles.contains_key(&id));
        }

        self.apply_found_prompts();

        // Registry scan: baseline busy/idle + identity, ~1 Hz, off-thread.
        if let Some(rx) = &self.scan_rx {
            match rx.try_recv() {
                Ok(result) => {
                    self.scan_rx = None;
                    self.scanned = result;
                    self.merge_scan(shell_pids, &mut effects);
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => self.scan_rx = None,
                Err(crossbeam_channel::TryRecvError::Empty) => {}
            }
        }
        if self.scan_rx.is_none() && self.last_scan.elapsed() >= Duration::from_secs(1) {
            self.last_scan = Instant::now();
            let dirs: Vec<PathBuf> = self.profiles.iter().map(|p| p.config_dir.clone()).collect();
            let shells = shell_pids.clone();
            let (tx, rx) = crossbeam_channel::bounded(1);
            if std::thread::Builder::new()
                .name("giverny session scan".into())
                .spawn(move || {
                    let live = registry::scan(dirs);
                    let attached = shells
                        .into_iter()
                        .filter_map(|(tab, shell)| Some((tab, attach::under(shell)?)))
                        .collect();
                    let _ = tx.send(ScanResult { live, attached });
                })
                .is_ok()
            {
                self.scan_rx = Some(rx);
            }
        }

        // Background agents: a handful of small files, so a slower tick than
        // the session registry is plenty.
        if self.last_jobs.elapsed() >= Duration::from_secs(3) {
            self.last_jobs = Instant::now();
            let dirs: Vec<PathBuf> = self.profiles.iter().map(|p| p.config_dir.clone()).collect();
            // Finished agents drop off: the list is what still wants
            // watching, not a record of everything that ever ran.
            let jobs = jobs::scan(dirs);
            self.apply_jobs(jobs, titles, opened, &mut effects);
        }

        // Re-read the caches when the file says so, when a refresh we asked
        // for has just rewritten one, or on the slow timer as a backstop.
        //
        // The timer alone meant a number could be a minute out of date with a
        // file that had already been rewritten — Claude Code updates the cache
        // itself every time a session fetches usage, which is the freshest
        // source there is short of the statusline push.
        self.watch_caches();
        self.look_again();
        if self.cache_dirty.swap(false, Ordering::Relaxed)
            || self.last_usage.elapsed() >= USAGE_READ_INTERVAL
        {
            self.refresh_usage();
        }

        effects
    }

    /// Fold a jobs scan in: which tab shows which job, what those tabs
    /// hold, and what is left for BACKGROUND.
    fn apply_jobs(
        &mut self,
        jobs: Vec<Job>,
        titles: &HashMap<TabId, String>,
        opened: &HashMap<TabId, String>,
        effects: &mut WatchEffects,
    ) {
        let on = self.jobs_by_tab(opened, &jobs);
        let placeholders = self.placeholders(&jobs);
        let before: HashSet<TabId> = self.viewing.values().copied().collect();
        self.viewing = jobs_on_screen(&on, titles, &jobs, &placeholders);
        // A tab that stopped showing a job, with no claude of its own in it
        // now: its pane goes, as at a `SessionEnd` (giverny#244).
        for tab in before {
            if !self.viewing.values().any(|&t| t == tab)
                && !self.tabs.get(&tab).is_some_and(ClaudeTab::has_claude)
            {
                self.agents.session_ended(tab);
            }
        }
        // A tab showing a job holds the conversation the job holds now,
        // whatever hooks it missed: its pane follows it, and a restart
        // resumes it, which attaches to the job (giverny#242).
        for job in &jobs {
            let (Some(&tab), Some(sid)) = (self.viewing.get(&job.id), job.resume_target()) else {
                continue;
            };
            self.agents.job_holds(
                tab,
                sid,
                job.forked_from.as_deref(),
                Some(job.config_dir.clone()),
            );
            let entry = self.tabs.entry(tab).or_default();
            if entry.session_id.as_deref() != Some(sid) {
                entry.set_session(Some(sid.to_string()));
                effects
                    .captured
                    .push((tab, Some(sid.to_string()), Some(job.config_dir.clone())));
            }
        }
        self.jobs = jobs
            .iter()
            .filter(|job| {
                job.worth_watching() && !self.shown_in_a_tab(job) && !placeholders.contains(&job.id)
            })
            .cloned()
            .collect();
        self.all_jobs = jobs;
    }

    /// The jobs a tab's claude parked on only as a placeholder: untouched
    /// (no name, no intent) and forked from that very claude's own session.
    /// A `claude --resume` of a job's conversation does this — Claude Code
    /// makes the client's fresh session a job, parks on it and shows the
    /// other one — and the placeholder is neither an agent to list under
    /// BACKGROUND nor what the tab shows (giverny#243).
    fn placeholders(&self, jobs: &[Job]) -> HashSet<String> {
        self.parked
            .iter()
            .filter_map(|(tab, id)| {
                let job = jobs.iter().find(|j| &j.id == id)?;
                let client = self.parked_by.get(tab)?;
                (job.untouched && job.forked_from.as_deref() == Some(client.as_str()))
                    .then(|| job.id.clone())
            })
            .collect()
    }

    /// The running background job whose conversation `session` is — the one
    /// it holds now, or its own — from the last jobs scan.
    pub fn live_job_holding(&self, session: &str) -> Option<&Job> {
        self.all_jobs.iter().find(|j| {
            j.live
                && (j.resume_session_id.as_deref() == Some(session)
                    || j.session_id.as_deref() == Some(session))
        })
    }

    /// Fold the last scan into per-tab state.
    fn merge_scan(&mut self, shell_pids: &HashMap<TabId, u32>, effects: &mut WatchEffects) {
        for tab in self.tabs.values_mut() {
            tab.seen_in_scan = false;
        }
        // Which tab holds which conversation, as the hooks reported it. This
        // is the only way to match a session that runs where our process ids
        // mean nothing: an entry inside a WSL distribution carries a Linux pid
        // and the tab's shell is a `wsl.exe` on the Windows side, so the
        // ancestry walk below can never connect the two. Without a match the
        // tab is "not seen in the scan", and five seconds after its last hook
        // it goes back to showing no Claude at all — which is what a tab does
        // between turns, all day.
        let by_session: HashMap<String, TabId> = self
            .tabs
            .iter()
            .filter_map(|(id, tab)| Some((tab.session_id.clone()?, *id)))
            .collect();
        self.parked.clear();
        self.parked_by.clear();
        self.attached = self.scanned.attached.clone();
        for &tab in self.attached.keys() {
            // A claude is there, showing a job: the job's hooks own its
            // state, as a parked one's would.
            self.tabs.entry(tab).or_default().seen_in_scan = true;
        }
        let mut ask_prompts: Vec<(String, PathBuf)> = Vec::new();
        {
            for live in self.scanned.live.clone() {
                // A job's worker is the daemon's; the tab showing the job is
                // found by its attach or its park, not by the conversation.
                if live.entry.job_worker() {
                    continue;
                }
                let Some(tab_id) = shell_pids
                    .iter()
                    .find(|(_, shell)| registry::has_ancestor(live.entry.pid, **shell))
                    .map(|(id, _)| *id)
                    .or_else(|| by_session.get(&live.entry.session_id).copied())
                else {
                    continue;
                };
                if let Some(job) = &live.entry.parked_job_id {
                    self.parked.insert(tab_id, job.clone());
                    self.parked_by.insert(tab_id, live.entry.session_id.clone());
                }
                let account = self.account_of(Some(&live.config_dir));
                let entry = self.tabs.entry(tab_id).or_default();
                entry.seen_in_scan = true;
                entry.background = live.entry.background_shell();
                // Remember which conversation this tab is holding, so it can
                // be resumed after a restart. Hooks report this too, but only
                // for sessions that started *after* they were installed —
                // every older session would otherwise be lost on restart
                // despite the registry naming it the whole time.
                //
                // Not for a claude parked on a background job: the id it
                // registered is the conversation it handed over, and
                // resuming that forks it into another job. The tab holds
                // what the job it shows holds, set by the jobs pass.
                if live.entry.parked_job_id.is_none() {
                    if entry.session_id.as_deref() != Some(live.entry.session_id.as_str()) {
                        effects.captured.push((
                            tab_id,
                            Some(live.entry.session_id.clone()),
                            Some(live.config_dir.clone()),
                        ));
                    }
                    entry.set_session(Some(live.entry.session_id.clone()));
                }
                entry.session_name = live.entry.name.clone();

                if account.is_some() {
                    entry.account = account;
                }
                // State authority is PER TAB: only once this tab's session has
                // actually emitted hook events do hooks own its state (the
                // registry file can lag with a stale "busy" and must not stomp
                // a crisp Stop). Hooks load at claude startup — a session
                // started before install never fires them, and a global
                // hooks-installed check would freeze such tabs; per-tab
                // evidence keeps the registry driving exactly those.
                let hooks_own = entry.last_hook.is_some();
                let was = entry.state;
                entry.state = merge_registry(entry.state, hooks_own, &live.entry);
                // Adopted mid-way or resumed after a restart, no hook has said
                // what was asked, but the transcript has. A session with no
                // hooks at all never will: each turn it starts is read again.
                let sid = &live.entry.session_id;
                let new_turn =
                    !hooks_own && was != ClaudeState::Busy && entry.state == ClaudeState::Busy;
                if new_turn {
                    self.prompts_asked.remove(sid);
                }
                if (entry.last_prompt.is_none() || new_turn)
                    && self.prompts_asked.insert(sid.clone())
                {
                    ask_prompts.push((sid.clone(), live.config_dir.clone()));
                }
            }
            // Sessions gone from the registry: clear unless hooks spoke recently.
            for tab in self.tabs.values_mut() {
                let hook_recent = tab
                    .last_hook
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(5));
                if !tab.seen_in_scan && !hook_recent && tab.state != ClaudeState::DoneUnseen {
                    tab.state = ClaudeState::None;
                    tab.background = false;
                    tab.session_name = None;
                    // The session is gone — its hook evidence goes with it, so
                    // a future claude (with or without hooks) starts fresh.
                    tab.last_hook = None;
                }
            }
        }
        self.read_prompts(ask_prompts);
    }

    /// A statusline push: official `rate_limits` for one account.
    fn apply_statusline(&mut self, msg: &RelayMsg) {
        let window =
            |key: &str| -> Option<&serde_json::Value> { msg.event.get("rate_limits")?.get(key) };
        let pct = |key: &str| -> Option<f64> { window(key)?.get("used_percentage")?.as_f64() };
        let resets = |key: &str| -> Option<jiff::Timestamp> { reset_time(window(key)?) };
        let live = LiveUsage {
            at: Instant::now(),
            five_hour: pct("five_hour"),
            seven_day: pct("seven_day"),
            five_hour_resets: resets("five_hour"),
            seven_day_resets: resets("seven_day"),
        };
        if live.five_hour.is_none() && live.seven_day.is_none() {
            return;
        }
        // Attribute to the account: explicit config dir, else the default profile.
        let dir = self
            .canonical_dir(msg.config_dir.as_deref())
            .or_else(|| dirs::home_dir().map(|h| h.join(".claude")));
        let Some(dir) = dir else { return };
        if let Some(acc) = self
            .accounts
            .iter_mut()
            .find(|a| a.profile.config_dir == dir)
        {
            acc.live = Some(live);
        }
    }

    /// Watch the cache files for being rewritten, off the UI thread.
    ///
    /// A `stat` is cheap until the file is inside a stopped WSL distribution,
    /// where the first touch of the share starts it and takes seconds. Twice
    /// a second on the UI thread, that is a frozen window. The thread sets the
    /// same dirty flag a refresh we asked for sets, and the read happens on
    /// the next tick either way.
    fn watch_caches(&mut self) {
        if self.last_cache_stat.elapsed() < CACHE_STAT_INTERVAL || self.stat_in_flight.get() {
            return;
        }
        self.last_cache_stat = Instant::now();
        let paths: Vec<PathBuf> = self
            .profiles
            .iter()
            .map(|p| profiles::identity_path(&p.config_dir))
            .collect();
        let seen = Arc::clone(&self.cache_mtimes);
        let dirty = Arc::clone(&self.cache_dirty);
        let in_flight = Arc::clone(&self.stat_in_flight);
        in_flight.set(true);
        let _ = std::thread::Builder::new()
            .name("giverny usage stat".into())
            .spawn(move || {
                for path in paths {
                    let Ok(at) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
                        continue;
                    };
                    let mut seen = seen.lock().unwrap();
                    if seen.get(&path) != Some(&at) {
                        seen.insert(path, at);
                        dirty.store(true, Ordering::Relaxed);
                    }
                }
                in_flight.set(false);
            });
    }

    /// Keep each window's high-water mark up to date.
    ///
    /// A reading from the window the mark is for can only raise it; one from
    /// a later window replaces it; one that belongs to no window the mark can
    /// place is let in only if it has not fallen tens of points below the mark.
    fn remember_peaks(&mut self) {
        let now = jiff::Timestamp::now();
        for acc in &mut self.accounts {
            let Some(usage) = &acc.usage else { continue };
            for limit in &usage.limits {
                let read = Self::sampled(acc, limit, now);
                let peak = acc.peak.entry(limit.kind.clone()).or_insert(Peak {
                    percent: read.percent,
                    resets: read.resets,
                });
                match peak.place(&read, now) {
                    Window::Same => peak.percent = peak.percent.max(read.percent),
                    // The mark's window is the current one; this number is
                    // from before it.
                    Window::Older => {}
                    Window::Other => {
                        *peak = Peak {
                            percent: read.percent,
                            resets: read.resets,
                        }
                    }
                }
            }
        }
    }

    /// Look for accounts again, off the UI thread, while none inside WSL has
    /// turned up.
    ///
    /// Discovery runs once, at startup — and a distribution that has to boot
    /// first can take longer to say where its home is than anything here will
    /// wait for it. Asked while it was still cold, it says nothing, and the
    /// account living in it is then missing for the whole run: no usage, no
    /// identity, and a resumed session attributed to nobody. It will answer a
    /// minute later; this is what asks again.
    fn look_again(&mut self) {
        if !cfg!(windows)
            || self.late_in_flight.get()
            || self.last_look.elapsed() < LOOK_AGAIN_INTERVAL
        {
            return;
        }
        if self
            .profiles
            .iter()
            .any(|p| wsl::is_wsl_path(&p.config_dir))
        {
            return;
        }
        if let Some(found) = self.late.lock().ok().and_then(|mut l| l.take())
            && found.len() > self.profiles.len()
        {
            self.profiles = found;
            self.refresh_usage();
            return;
        }
        self.last_look = Instant::now();
        let extra = self.extra_dirs.clone();
        let slot = Arc::clone(&self.late);
        let in_flight = Arc::clone(&self.late_in_flight);
        in_flight.set(true);
        let _ = std::thread::Builder::new()
            .name("giverny accounts".into())
            .spawn(move || {
                let found = profiles::discover(&extra);
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some(found);
                }
                in_flight.set(false);
            });
    }

    fn refresh_usage(&mut self) {
        self.last_usage = Instant::now();
        // The panels are rebuilt from the profiles, so anything the panel
        // learned rather than read — the last push, how far each window has
        // got — has to be carried over or it resets every minute.
        let mut previous: HashMap<PathBuf, (Option<LiveUsage>, HashMap<String, Peak>)> = self
            .accounts
            .drain(..)
            .map(|a| (a.profile.config_dir, (a.live, a.peak)))
            .collect();
        self.accounts = self
            .profiles
            .iter()
            .map(|p| {
                let (live, peak) = previous.remove(&p.config_dir).unwrap_or_default();
                AccountPanel {
                    usage: usage::read(&p.config_dir),
                    live,
                    peak,
                    statusline_on: hooks::statusline_installed_in(
                        &p.config_dir.join("settings.json"),
                    ),
                    profile: p.clone(),
                }
            })
            .collect();
    }

    /// Is a refresh due for an account? Split out from the sweep so the two
    /// ways it can be spared — young numbers, and a recent attempt — are
    /// testable without spawning anything.
    fn refresh_due(
        age_minutes: i64,
        since_attempt: Option<Duration>,
        max_age_minutes: u64,
    ) -> bool {
        let window = Duration::from_secs(max_age_minutes * 60);
        if age_minutes < max_age_minutes as i64 {
            return false;
        }
        // An account with no readable cache is infinitely "old", so the age
        // test never spares it; the attempt clock is what stops the retry loop.
        since_attempt.is_none_or(|since| since >= window)
    }

    /// Ask Claude Code to refresh accounts whose numbers have aged out.
    /// `max_age_minutes == 0` disables the sweep; `force` refreshes everything
    /// now (the user asked), subject only to the in-flight guard.
    pub fn refresh_stale_usage(&self, max_age_minutes: u64, force: bool) {
        if max_age_minutes == 0 && !force {
            return;
        }
        let now = jiff::Timestamp::now();
        for acc in &self.accounts {
            if !force {
                let age = acc
                    .usage
                    .as_ref()
                    .map(|u| usage::age_minutes(u, now))
                    .unwrap_or(i64::MAX);
                let since = self
                    .attempted
                    .lock()
                    .unwrap()
                    .get(&acc.profile.config_dir)
                    .map(|t| t.elapsed());
                if !Self::refresh_due(age, since, max_age_minutes) {
                    continue;
                }
            }
            self.spawn_refresh(acc.profile.config_dir.clone());
        }
    }

    fn spawn_refresh(&self, config_dir: PathBuf) {
        {
            let mut busy = self.refreshing.lock().unwrap();
            if !busy.insert(config_dir.clone()) {
                return; // already refreshing this account
            }
        }
        self.attempted
            .lock()
            .unwrap()
            .insert(config_dir.clone(), Instant::now());
        let busy = Arc::clone(&self.refreshing);
        let dirty = Arc::clone(&self.cache_dirty);
        let _ = std::thread::Builder::new()
            .name("giverny usage refresh".into())
            .spawn(move || {
                match usage::refresh_via_cli(&config_dir) {
                    Ok(()) => {
                        tracing::info!("usage refreshed for {}", config_dir.display());
                        // Show the new numbers without waiting for the timer.
                        dirty.store(true, Ordering::Relaxed);
                    }
                    Err(err) => tracing::info!("usage refresh skipped: {err}"),
                }
                busy.lock().unwrap().remove(&config_dir);
            });
    }

    /// Is any account mid-refresh (for the spinner in the rail)?
    pub fn refresh_in_flight(&self) -> bool {
        !self.refreshing.lock().unwrap().is_empty()
    }

    /// Start every Claude session in auto mode, in every account.
    ///
    /// Claude Code reads `settings.json` when a session starts, so this
    /// changes the next `claude`, not the ones already running.
    pub fn set_auto_mode(&mut self, enable: bool) {
        if self.leave_accounts {
            return;
        }
        for p in &self.profiles {
            let settings = p.config_dir.join("settings.json");
            match hooks::set_auto_mode(&settings, enable) {
                Ok(()) => tracing::info!(
                    "auto mode {} for {}",
                    if enable { "on" } else { "off" },
                    p.name
                ),
                Err(err) => tracing::warn!("auto mode unchanged for {}: {err}", p.name),
            }
        }
    }

    /// Apply the auto-mode setting to accounts that have no permission mode
    /// of their own — an account added after the toggle was turned on, or one
    /// whose settings.json was rewritten. A mode set by hand is never
    /// overridden here; only the explicit toggle does that.
    pub fn ensure_auto_mode(&mut self) {
        if self.leave_accounts {
            return;
        }
        let missing: Vec<PathBuf> = self
            .profiles
            .iter()
            .map(|p| p.config_dir.join("settings.json"))
            .filter(|s| hooks::default_mode_in(s).is_none())
            .collect();
        for settings in missing {
            if let Err(err) = hooks::set_auto_mode(&settings, true) {
                tracing::warn!("auto mode unchanged for {}: {err}", settings.display());
            }
        }
    }

    /// Do all accounts start Claude in auto mode?
    pub fn auto_mode_on(&self) -> bool {
        !self.profiles.is_empty()
            && self
                .profiles
                .iter()
                .all(|p| hooks::auto_mode_in(&p.config_dir.join("settings.json")))
    }

    /// Turn the live-usage statusline on/off for every profile.
    pub fn set_statusline(&mut self, enable: bool) -> Result<(), String> {
        if self.leave_accounts {
            return Err(LEFT_ALONE.into());
        }
        let mut errs = Vec::new();
        for p in &self.profiles {
            if let Err(e) = hooks::set_statusline(&p.config_dir.join("settings.json"), enable) {
                errs.push(format!("{}: {e}", p.name));
            }
        }
        self.refresh_usage();
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("; "))
        }
    }

    /// Follow the `claude.agents_pane` setting in every account: on installs
    /// `subagentStatusLine` (`giverny relay --subagent-line`), which feeds the
    /// pane and hides Claude Code's own subagent panel; off removes it, and
    /// Claude Code draws its panel natively again. Called at startup too, so
    /// an account added since, or a moved binary, is brought in line. An
    /// account with a `subagentStatusLine` of its own is left alone.
    ///
    /// Claude Code watches its settings files, so this reaches running
    /// sessions without a restart.
    ///
    /// The same switch carries the `giverny` plugin: on, its
    /// marketplace is written under `base` (Giverny's config dir) and each
    /// account's `settings.json` gains the two keys that load it; off, the
    /// keys go and so does the directory. `skill` is
    /// `agents_panel.orchestrate_skill`: off, the plugin is written without
    /// its orchestrate skill.
    ///
    /// The setting is on by default, so it is not consent by itself: the
    /// keys are written only into an account that already holds our hooks
    /// ([`hooks::partly_installed_in`]) — installing them is the consent, as
    /// it is for the live-usage statusline. Every other account is left
    /// byte-identical. Off removes only what is ours, wherever it is.
    pub fn set_agents_pane(&mut self, enable: bool, skill: bool, base: &Path) {
        if self.leave_accounts {
            return;
        }
        let dir = giverny_claude::plugin::marketplace_dir(base);
        if enable {
            match giverny_claude::plugin::sync(
                &dir,
                &giverny_claude::plugin::exe_candidates(),
                skill,
            ) {
                Ok(true) => tracing::info!("giverny plugin written to {}", dir.display()),
                Ok(false) => {}
                Err(err) => tracing::warn!("giverny plugin not written: {err}"),
            }
        }
        for p in &self.profiles {
            let settings = p.config_dir.join("settings.json");
            let enable = enable && hooks::partly_installed_in(&settings);
            match hooks::set_subagent_line(&settings, enable) {
                Ok(true) => tracing::info!(
                    "subagentStatusLine {} for {}",
                    if enable { "installed" } else { "removed" },
                    p.name
                ),
                Ok(false) => {}
                Err(err) => tracing::info!("subagentStatusLine skipped for {}: {err}", p.name),
            }
            match giverny_claude::plugin::set_plugin(&settings, &dir, enable) {
                Ok(true) => tracing::info!(
                    "giverny plugin {} for {}",
                    if enable { "enabled" } else { "removed" },
                    p.name
                ),
                Ok(false) => {}
                Err(err) => tracing::info!("giverny plugin skipped for {}: {err}", p.name),
            }
        }
        if !enable {
            match giverny_claude::plugin::remove_dir(&dir) {
                Ok(true) => tracing::info!("giverny plugin removed from {}", dir.display()),
                Ok(false) => {}
                Err(err) => tracing::warn!("giverny plugin dir not removed: {err}"),
            }
        }
    }

    /// Do all profiles have the live-usage statusline?
    pub fn statusline_on(&self) -> bool {
        !self.accounts.is_empty() && self.accounts.iter().all(|a| a.statusline_on)
    }

    /// When the Claude on `account` can work again, if anything says.
    ///
    /// The message on screen names a reset time too, in the local words of
    /// whoever is reading it ("resets 3pm"); this is the same moment as a
    /// timestamp, read the same way the usage bars read it — the status line
    /// push first, the cache behind it. The cache matters least here of
    /// anywhere: refreshing it means running `claude` against the very
    /// account that is out of limit.
    ///
    /// An account nobody could name falls back to whichever account reopens
    /// first. A tab whose session never fired a hook has no account against
    /// its name, and waiting forever is worse than waking on the wrong
    /// window.
    pub fn window_reopens(&self, account: Option<&str>) -> Option<jiff::Timestamp> {
        let now = jiff::Timestamp::now();
        let named = account
            .and_then(|name| self.accounts.iter().find(|a| a.profile.name == name))
            .and_then(|panel| Self::reopens_for(panel, now));
        named.or_else(|| {
            self.accounts
                .iter()
                .filter_map(|panel| Self::reopens_for(panel, now))
                .min()
        })
    }

    /// The window that is actually out, not merely the next one to come
    /// round: a session stopped by the weekly limit is not freed when the
    /// five-hour one resets.
    fn reopens_for(panel: &AccountPanel, now: jiff::Timestamp) -> Option<jiff::Timestamp> {
        let limits = panel.usage.as_ref().map(|u| u.limits.as_slice());
        let read = |kind: &str| {
            limits?
                .iter()
                .find(|l| l.kind == kind)
                .map(|l| Self::reading(panel, l, now))
        };
        let spent = ["session", "weekly_all"]
            .iter()
            .filter_map(|kind| read(kind))
            .filter(|r| r.percent >= 95.0)
            .filter_map(|r| r.resets)
            .max();
        spent
            .or_else(|| read("session").and_then(|r| r.resets))
            // No cache at all, which is every account whose numbers have only
            // ever come from the status line.
            .or_else(|| panel.live.as_ref().and_then(|l| l.five_hour_resets))
            .filter(|at| *at > now)
    }

    /// How fresh this account's numbers actually are, and from where.
    /// Reporting only the cache age reads as "stale" even when a live push
    /// has already overridden the bars.
    pub fn freshness(acc: &AccountPanel, now: jiff::Timestamp) -> Freshness {
        let cache_min = acc.usage.as_ref().map(|u| usage::age_minutes(u, now));
        let live_min = acc
            .live
            .as_ref()
            .map(|l| (l.at.elapsed().as_secs() / 60) as i64);
        match (live_min, cache_min) {
            (Some(l), Some(c)) if l <= c => Freshness::Live(l),
            (Some(l), None) => Freshness::Live(l),
            (_, Some(c)) => Freshness::Cache(c),
            (None, None) => Freshness::None,
        }
    }

    /// What one bar should show: the statusline push where it is fresher than
    /// the on-disk cache, the cache otherwise.
    /// What one bar shows: the freshest sample, never lower than this window
    /// has already been seen to reach.
    pub fn reading(
        acc: &AccountPanel,
        limit: &giverny_claude::usage::LimitEntry,
        now: jiff::Timestamp,
    ) -> Reading {
        let mut read = Self::sampled(acc, limit, now);
        if let Some(peak) = acc.peak.get(&limit.kind)
            && peak.place(&read, now) != Window::Other
            && peak.percent > read.percent
        {
            read.percent = peak.percent;
            read.critical = read.critical || peak.percent >= 95.0;
            read.resets = peak.resets.or(read.resets);
        }
        read
    }

    /// The freshest of the two sources, whichever that is right now.
    fn sampled(
        acc: &AccountPanel,
        limit: &giverny_claude::usage::LimitEntry,
        now: jiff::Timestamp,
    ) -> Reading {
        let pushed = acc.live.as_ref().filter(|live| {
            let cache_age_ms = acc
                .usage
                .as_ref()
                .map(|u| (now.as_millisecond() - u.fetched_at_ms as i64).max(0))
                .unwrap_or(i64::MAX);
            (live.at.elapsed().as_millis() as i64) < cache_age_ms
        });
        let window = |pick: fn(&LiveUsage) -> Option<f64>,
                      reset: fn(&LiveUsage) -> Option<jiff::Timestamp>| {
            (
                pushed.and_then(pick).map(|p| p.clamp(0.0, 100.0)),
                pushed.and_then(reset).filter(|at| *at > now),
            )
        };
        let (fresh, pushed_reset) = match limit.kind.as_str() {
            "session" => window(|l| l.five_hour, |l| l.five_hour_resets),
            "weekly_all" => window(|l| l.seven_day, |l| l.seven_day_resets),
            // A scoped window (one model's own allowance) is not in the push.
            _ => (None, None),
        };
        let percent = fresh.unwrap_or_else(|| limit.effective_percent(now));
        // The cache's severity describes the cache's percentage. Where that is
        // not the number being shown — a push took over, or the window it
        // measured has since lapsed — the number on screen decides for itself.
        let speaks_for_itself = fresh.is_some() || limit.rolled_over(now);
        Reading {
            percent,
            live: fresh.is_some(),
            critical: percent >= 95.0 || (!speaks_for_itself && limit.critical()),
            // The push's reset time first: it comes from the running Claude,
            // while the cache can be hours old — and a cache that has fallen
            // behind the window it describes claims the reset already
            // happened, which is why this line went missing for anyone whose
            // cache refresh was failing.
            resets: pushed_reset.or_else(|| limit.resets_at_ts().filter(|at| *at > now)),
        }
    }

    /// The user looked at the tab: done-markers clear.
    pub fn mark_viewed(&mut self, tab: TabId) {
        if let Some(entry) = self.tabs.get_mut(&tab)
            && entry.state == ClaudeState::DoneUnseen
        {
            entry.state = ClaudeState::Idle;
        }
    }

    /// The user typed in the tab: attention has been given, whatever the
    /// outcome. Declining a permission prompt (Escape) produces no hook at
    /// all, so without this the flag would blink forever.
    pub fn mark_attended(&mut self, tab: TabId) {
        if let Some(entry) = self.tabs.get_mut(&tab)
            && matches!(entry.state, ClaudeState::NeedsYou | ClaudeState::DoneUnseen)
        {
            entry.state = ClaudeState::Idle;
        }
    }

    pub fn state_of(&self, tab: TabId) -> ClaudeState {
        self.tabs.get(&tab).map(|t| t.state).unwrap_or_default()
    }

    /// The prompt to pin above this tab: its last one, while Claude runs.
    pub fn prompt_of(&self, tab: TabId) -> Option<&str> {
        let tab = self.tabs.get(&tab)?;
        if tab.state == ClaudeState::None {
            return None;
        }
        tab.last_prompt.as_deref()
    }

    /// Read these sessions' last prompts from their transcripts, off the UI
    /// thread: for an account inside WSL the file is across a share.
    fn read_prompts(&self, sessions: Vec<(String, PathBuf)>) {
        if sessions.is_empty() {
            return;
        }
        let tx = self.prompts_found.0.clone();
        let _ = std::thread::Builder::new()
            .name("giverny last prompt".into())
            .spawn(move || {
                for (session, dir) in sessions {
                    if let Some(prompt) = registry::find_transcript(&dir, &session)
                        .and_then(|path| registry::last_prompt(&path))
                    {
                        let _ = tx.send((session, prompt));
                    }
                }
            });
    }

    /// Prompts read back from transcripts. A hook that reported one since is
    /// newer, and wins; a tab no hook speaks for takes the latest read.
    fn apply_found_prompts(&mut self) {
        let found: Vec<FoundPrompt> = self.prompts_found.1.try_iter().collect();
        for (session, prompt) in found {
            for tab in self.tabs.values_mut() {
                if tab.session_id.as_deref() == Some(session.as_str())
                    && (tab.last_prompt.is_none() || tab.last_hook.is_none())
                {
                    tab.last_prompt = Some(prompt.clone());
                }
            }
        }
    }

    /// Test seam: a watcher with no listener and no profiles.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        ClaudeWatch {
            profiles: Vec::new(),
            tabs: HashMap::new(),
            jobs: Vec::new(),
            parked: HashMap::new(),
            parked_by: HashMap::new(),
            all_jobs: Vec::new(),
            attached: HashMap::new(),
            viewing: HashMap::new(),
            accounts: Vec::new(),
            hooks_installed: true,
            hook_rx: None,
            last_scan: Instant::now(),
            scan_rx: None,
            scanned: ScanResult::default(),
            last_jobs: Instant::now(),
            last_usage: Instant::now(),
            cache_mtimes: Arc::new(Mutex::new(HashMap::new())),
            last_cache_stat: Instant::now(),
            stat_in_flight: Arc::new(AtomicFlag::default()),
            late: Arc::new(Mutex::new(None)),
            last_look: Instant::now(),
            late_in_flight: Arc::new(AtomicFlag::default()),
            extra_dirs: Vec::new(),
            prompts_asked: HashSet::new(),
            prompts_found: crossbeam_channel::unbounded(),
            refreshing: Arc::new(Mutex::new(HashSet::new())),
            attempted: Arc::new(Mutex::new(HashMap::new())),
            cache_dirty: Arc::new(AtomicBool::new(false)),
            agents: AgentsLive::in_memory(),
            leave_accounts: false,
        }
    }

    /// Is the hook relay socket actually listening?
    pub fn relay_listening(&self) -> bool {
        self.hook_rx.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAB: TabId = TabId(7);

    fn msg(json: &str) -> RelayMsg {
        serde_json::from_str(json).expect("relay msg fixture")
    }

    /// A relayed `subagentStatusLine` tick lands in that tab's tracker and
    /// says nothing about the tab's own state; `/clear` empties the table.
    #[test]
    fn subagent_line_ticks_feed_the_tabs_tracker() {
        let mut w = ClaudeWatch::for_tests();
        let tick = msg(&format!(
            r#"{{"tab_id":"giverny-7","config_dir":"/tmp/giverny-nowhere",
                "event":{{"hook_event_name":"{}","session_id":"s-1",
                          "tasks":[{{"id":"a1","status":"running","startTime":1790000000000}}]}}}}"#,
            hooks::SUBAGENT_LINE_EVENT
        ));
        feed(&mut w, &tick, Some(TAB));
        let t = w.agents.tracker(TAB).expect("a tracker for the tab");
        assert_eq!(t.rows().len(), 1);
        assert_eq!(t.session_id.as_deref(), Some("s-1"));
        assert_eq!(w.state_of(TAB), ClaudeState::None, "no state change");

        feed(
            &mut w,
            &hook("SessionStart", r#","source":"clear""#),
            Some(TAB),
        );
        assert!(
            w.agents.tracker(TAB).unwrap().is_empty(),
            "/clear empties it"
        );
    }

    #[test]
    fn clear_done_empties_the_tabs_done_rows_only() {
        let mut w = ClaudeWatch::for_tests();
        let tick = |ids: &str| {
            msg(&format!(
                r#"{{"tab_id":"giverny-7","config_dir":"/tmp/giverny-nowhere",
                    "event":{{"hook_event_name":"{}","session_id":"s-1","tasks":[{ids}]}}}}"#,
                hooks::SUBAGENT_LINE_EVENT
            ))
        };
        let a = r#"{"id":"a1","status":"running","startTime":1790000000000}"#;
        let b = r#"{"id":"a2","status":"running","startTime":1790000000000}"#;
        feed(&mut w, &tick(&format!("{a},{b}")), Some(TAB));
        feed(&mut w, &tick(a), Some(TAB));
        assert_eq!(w.agents.tracker(TAB).unwrap().rows().len(), 2);
        feed(
            &mut w,
            &hook(hooks::CLEAR_DONE_EVENT, r#","at_ms":1790000100000"#),
            Some(TAB),
        );
        let t = w.agents.tracker(TAB).unwrap();
        assert_eq!(
            t.rows().len(),
            1,
            "the Done row went, the running one stayed"
        );
        assert_eq!(t.done_cleared_ms, Some(1_790_000_100_000));
        assert_eq!(w.state_of(TAB), ClaudeState::None, "no state change");
    }

    fn hook(event: &str, extra: &str) -> RelayMsg {
        msg(&format!(
            r#"{{"tab_id":"giverny-7","config_dir":null,
                "event":{{"hook_event_name":"{event}","session_id":"s-1"{extra}}}}}"#
        ))
    }

    fn feed(w: &mut ClaudeWatch, m: &RelayMsg, active: Option<TabId>) -> WatchEffects {
        let mut fx = WatchEffects::default();
        w.handle_msg(m, active, "tab", &mut fx);
        fx
    }

    #[test]
    fn turn_lifecycle_drives_states() {
        let mut w = ClaudeWatch::for_tests();
        assert_eq!(w.state_of(TAB), ClaudeState::None);

        let fx = feed(&mut w, &hook("SessionStart", ""), Some(TAB));
        assert_eq!(w.state_of(TAB), ClaudeState::Idle);
        assert_eq!(fx.captured.len(), 1, "session id captured for resume");

        feed(&mut w, &hook("UserPromptSubmit", ""), Some(TAB));
        assert_eq!(w.state_of(TAB), ClaudeState::Busy, "spinner while working");

        // Finishing in the FOCUSED tab returns to idle...
        feed(&mut w, &hook("Stop", ""), Some(TAB));
        assert_eq!(w.state_of(TAB), ClaudeState::Idle);

        // ...but finishing in a background tab leaves a done marker.
        feed(&mut w, &hook("UserPromptSubmit", ""), Some(TabId(1)));
        feed(&mut w, &hook("Stop", ""), Some(TabId(1)));
        assert_eq!(w.state_of(TAB), ClaudeState::DoneUnseen);
        w.mark_viewed(TAB);
        assert_eq!(
            w.state_of(TAB),
            ClaudeState::Idle,
            "viewing clears the marker"
        );
    }

    #[test]
    fn attention_notifications_only_for_needs_you() {
        let mut w = ClaudeWatch::for_tests();
        feed(&mut w, &hook("SessionStart", ""), Some(TabId(1)));

        for kind in [
            "permission_prompt",
            "elicitation_dialog",
            "agent_needs_input",
        ] {
            let m = hook("Notification", &format!(r#","notification_type":"{kind}""#));
            let fx = feed(&mut w, &m, Some(TabId(1)));
            assert_eq!(w.state_of(TAB), ClaudeState::NeedsYou, "{kind}");
            assert_eq!(
                fx.notify.len(),
                1,
                "{kind} must raise a desktop notification"
            );
        }

        // Completion kinds never notify; they only mark done.
        let m = hook("Notification", r#","notification_type":"agent_completed""#);
        let fx = feed(&mut w, &m, Some(TabId(1)));
        assert!(fx.notify.is_empty(), "completions must not notify");
        assert_eq!(w.state_of(TAB), ClaudeState::DoneUnseen);
    }

    /// A background job's hooks name the job, not a tab: they reach the tab
    /// parked on it, and a `/clear` there empties that tab's pane
    /// (giverny#242). A job no tab shows reaches nothing.
    #[test]
    fn a_background_jobs_hooks_reach_the_tab_parked_on_it() {
        let mut w = ClaudeWatch::for_tests();
        w.agents.apply_live(
            TAB,
            Some("/tmp/giverny-nowhere".into()),
            &serde_json::json!({"session_id": "s-1", "tasks": [{"id": "a1", "status": "running"}]}),
        );
        let clear = msg(r#"{"job":"34c55b2c","config_dir":null,
            "event":{"hook_event_name":"SessionStart","source":"clear","session_id":"s-2"}}"#);

        feed(&mut w, &clear, None);
        assert_eq!(
            w.agents.tracker(TAB).unwrap().rows().len(),
            1,
            "no tab is parked on the job yet"
        );

        w.viewing.insert("34c55b2c".into(), TAB);
        let fx = feed(&mut w, &clear, None);
        let t = w.agents.tracker(TAB).unwrap();
        assert!(t.is_empty(), "the job's /clear empties its tab's pane");
        assert_eq!(t.session_id.as_deref(), Some("s-2"));
        assert_eq!(fx.captured.len(), 1);
        assert_eq!(fx.captured[0].0, TAB);
    }

    /// A job that a tab shows is that tab's, not one more row under
    /// BACKGROUND (giverny#243).
    #[test]
    fn a_job_a_tab_shows_is_not_in_the_background_list() {
        let mut w = ClaudeWatch::for_tests();
        let job = |id: &str, sid: &str| giverny_claude::jobs::Job {
            id: id.into(),
            name: id.into(),
            state: giverny_claude::jobs::JobState::Working,
            detail: None,
            tasks: 0,
            queued: 0,
            cwd: None,
            session_id: Some(sid.into()),
            resume_session_id: None,
            updated_at_ms: 0,
            config_dir: "/c".into(),
            live: true,
            pinned: false,
            forked_from: None,
            untouched: false,
        };
        let parked = job("34c55b2c", "s-parked");
        let held = job("0a1f39b3", "s-held");
        let alone = job("29ab7872", "s-alone");
        assert!(!w.shown_in_a_tab(&parked));

        w.viewing.insert("34c55b2c".into(), TAB);
        w.tabs.entry(TabId(8)).or_default().session_id = Some("s-held".into());
        assert!(w.shown_in_a_tab(&parked), "parked on in a tab");
        assert!(
            !w.shown_in_a_tab(&held),
            "a tab with no claude in it holds nothing"
        );
        w.tabs.entry(TabId(8)).or_default().seen_in_scan = true;
        assert!(w.shown_in_a_tab(&held), "its conversation is a tab's");
        assert!(!w.shown_in_a_tab(&alone), "no tab shows it");
    }

    fn bg_job(id: &str, name: &str, sid: &str) -> Job {
        Job {
            id: id.into(),
            name: name.into(),
            state: giverny_claude::jobs::JobState::Working,
            detail: None,
            tasks: 0,
            queued: 0,
            cwd: None,
            session_id: Some(sid.into()),
            resume_session_id: None,
            updated_at_ms: 0,
            config_dir: "/c".into(),
            live: true,
            pinned: false,
            forked_from: None,
            untouched: false,
        }
    }

    /// A `claude --resume` of a running job's conversation (a restart's):
    /// Claude Code makes the client's own fresh session an untouched job,
    /// parks on it, and shows the job asked for. The placeholder is not one
    /// more agent under BACKGROUND, gets no hooks, and is never what the
    /// tab shows; the conversation is the job's, so a restart attaches
    /// (giverny#243).
    #[test]
    fn a_resumes_placeholder_job_is_nobodys() {
        let mut w = ClaudeWatch::for_tests();
        let mut placeholder = bg_job("f68bc6cd", "f68bc6cd", "f68bc6cd-7c66");
        placeholder.untouched = true;
        placeholder.forked_from = Some("f705f9e7-19c5".into());
        let mut real = bg_job(
            "34c55b2c",
            "Open bugs in panel/orchestrator",
            "34c55b2c-d6db",
        );
        real.resume_session_id = Some("1ec991d3-fec9".into());
        let jobs = vec![
            placeholder,
            real,
            bg_job("6e7e56e0", "count rust lines", "s-6e"),
        ];
        w.parked.insert(TAB, "f68bc6cd".into());
        w.parked_by.insert(TAB, "f705f9e7-19c5".into());
        w.tabs.entry(TAB).or_default().seen_in_scan = true;
        let none = HashMap::new();
        let mut fx = WatchEffects::default();
        let hook = msg(r#"{"job":"f68bc6cd","config_dir":null,
            "event":{"hook_event_name":"UserPromptSubmit","session_id":"f68bc6cd-7c66"}}"#);

        let titles: HashMap<TabId, String> =
            [(TAB, "◐ Open bugs in panel/orchestrator".to_string())].into();
        w.apply_jobs(jobs.clone(), &titles, &none, &mut fx);
        assert_eq!(
            background(&w),
            ["6e7e56e0"],
            "neither the shown job nor the placeholder"
        );
        assert_eq!(w.viewing.get("34c55b2c"), Some(&TAB));
        assert_eq!(
            w.tab_of(&hook),
            None,
            "the placeholder's hooks are no tab's"
        );

        let titles: HashMap<TabId, String> = [(TAB, "~/giverny".to_string())].into();
        w.apply_jobs(jobs.clone(), &titles, &none, &mut fx);
        assert!(w.viewing.is_empty(), "a placeholder is never the fallback");
        assert!(!background(&w).contains(&"f68bc6cd"));
        // Its client gone, it is still nobody's agent.
        w.parked.clear();
        w.parked_by.clear();
        w.apply_jobs(jobs.clone(), &titles, &none, &mut fx);
        assert!(!background(&w).contains(&"f68bc6cd"), "untouched");
        w.parked.insert(TAB, "f68bc6cd".into());
        w.parked_by.insert(TAB, "f705f9e7-19c5".into());

        // Not a placeholder: a job someone asked for, or another session's.
        let mut asked = jobs.clone();
        asked[0].untouched = false;
        w.apply_jobs(asked, &titles, &none, &mut fx);
        assert_eq!(w.viewing.get("f68bc6cd"), Some(&TAB));
        w.parked_by.insert(TAB, "someone-else".into());
        w.apply_jobs(jobs, &titles, &none, &mut fx);
        assert_eq!(w.viewing.get("f68bc6cd"), Some(&TAB));

        // What a restart asks: whose conversation is this?
        assert_eq!(
            w.live_job_holding("1ec991d3-fec9").map(|j| j.id.as_str()),
            Some("34c55b2c")
        );
        assert_eq!(
            w.live_job_holding("34c55b2c-d6db").map(|j| j.id.as_str()),
            Some("34c55b2c")
        );
        assert!(w.live_job_holding("f705f9e7-19c5").is_none());
    }

    fn background(w: &ClaudeWatch) -> Vec<&str> {
        let mut ids: Vec<&str> = w.jobs.iter().map(|j| j.id.as_str()).collect();
        ids.sort();
        ids
    }

    /// A `claude attach` under a tab's shell — typed, a BACKGROUND click's,
    /// or a restart's resume re-exec'd as one — writes no registry entry.
    /// The job it names, by any name `claude attach` takes, is that tab's:
    /// out of BACKGROUND, its hooks routed there; back once it detaches
    /// (giverny#243).
    #[test]
    fn a_tab_attached_to_a_job_shows_it_and_gets_its_hooks() {
        let mut w = ClaudeWatch::for_tests();
        let jobs = vec![
            bg_job("6e7e56e0", "count rust lines giverny#243", "s-6e"),
            bg_job("34c55b2c", "Open bugs in panel/orchestrator", "s-34"),
        ];
        let titles: HashMap<TabId, String> = [(TAB, "~/giverny".to_string())].into();
        let none = HashMap::new();
        let mut fx = WatchEffects::default();
        let hook = msg(r#"{"job":"6e7e56e0","config_dir":null,
            "event":{"hook_event_name":"UserPromptSubmit","session_id":"s-6e"}}"#);
        // The daemon's worker for the job registers the job's conversation;
        // it is nobody's tab, whatever tab holds that conversation.
        w.scanned.live.push(registry::LiveSession {
            entry: serde_json::from_str(
                r#"{"pid":1,"sessionId":"s-6e","kind":"bg","jobId":"6e7e56e0","status":"idle"}"#,
            )
            .expect("worker entry"),
            config_dir: "/c".into(),
        });

        w.apply_jobs(jobs.clone(), &titles, &none, &mut fx);
        assert_eq!(background(&w), ["34c55b2c", "6e7e56e0"]);
        assert_eq!(w.tab_of(&hook), None, "no tab shows it");

        for target in ["6e7e56e0", "6e7e", "s-6e", "rust lines"] {
            w.scanned.attached = [(TAB, target.to_string())].into();
            w.merge_scan(&HashMap::new(), &mut fx);
            w.apply_jobs(jobs.clone(), &titles, &none, &mut fx);
            assert_eq!(background(&w), ["34c55b2c"], "attached by {target:?}");
            assert_eq!(w.tab_of(&hook), Some(TAB), "attached by {target:?}");
        }
        assert_eq!(
            w.tabs[&TAB].session_id.as_deref(),
            Some("s-6e"),
            "a restart resumes, and so attaches to, the job"
        );
        assert!(
            w.agents.shown(TAB).is_some(),
            "its pane shows the job's feed (giverny#244)"
        );
        feed(&mut w, &hook, None);
        assert_eq!(w.state_of(TAB), ClaudeState::Busy, "its hook is the tab's");
        w.merge_scan(&HashMap::new(), &mut fx);
        assert_eq!(w.state_of(TAB), ClaudeState::Busy, "an attach is a claude");

        // Detached: the next scan finds no attach, and the hooks are old.
        w.scanned.attached.clear();
        w.tabs.get_mut(&TAB).unwrap().last_hook = Some(Instant::now() - Duration::from_secs(10));
        w.merge_scan(&HashMap::new(), &mut fx);
        w.apply_jobs(jobs.clone(), &titles, &none, &mut fx);
        assert_eq!(
            background(&w),
            ["34c55b2c", "6e7e56e0"],
            "back in BACKGROUND"
        );
        assert_eq!(w.tab_of(&hook), None);
        assert!(w.agents.shown(TAB).is_none(), "the job's pane went with it");
    }

    /// A tab opened from BACKGROUND shows its job before the scan sees the
    /// `claude attach` typed into it, and an attach seen there wins.
    #[test]
    fn a_tab_opened_from_background_shows_its_job_at_once() {
        let mut w = ClaudeWatch::for_tests();
        let jobs = vec![
            bg_job("6e7e56e0", "count rust lines giverny#243", "s-6e"),
            bg_job("34c55b2c", "Open bugs in panel/orchestrator", "s-34"),
        ];
        let titles: HashMap<TabId, String> = [(TAB, "~".to_string())].into();
        let opened: HashMap<TabId, String> = [(TAB, "6e7e56e0".to_string())].into();
        let mut fx = WatchEffects::default();
        w.apply_jobs(jobs.clone(), &titles, &opened, &mut fx);
        assert_eq!(background(&w), ["34c55b2c"]);
        assert_eq!(w.viewing.get("6e7e56e0"), Some(&TAB));

        // Its claude switched jobs in the agents view and re-attached.
        w.scanned.attached = [(TAB, "34c55b2c".to_string())].into();
        w.merge_scan(&HashMap::new(), &mut fx);
        w.apply_jobs(jobs, &titles, &opened, &mut fx);
        assert_eq!(w.viewing.get("34c55b2c"), Some(&TAB));
        assert_eq!(w.viewing.get("6e7e56e0"), None);
    }

    /// Claude Code's agents view switches a parked claude between jobs and
    /// says so only in the tab's title: the job named there is the one on
    /// screen, the parked one when none is.
    #[test]
    fn the_job_on_screen_is_the_one_the_title_names() {
        let job = |id: &str, name: &str, live: bool| Job {
            id: id.into(),
            name: name.into(),
            state: giverny_claude::jobs::JobState::Working,
            detail: None,
            tasks: 0,
            queued: 0,
            cwd: None,
            session_id: None,
            resume_session_id: None,
            updated_at_ms: 0,
            config_dir: "/c".into(),
            live,
            pinned: false,
            forked_from: None,
            untouched: false,
        };
        let jobs = [
            job("970bf052", "Open bugs in panel/orchestrator (2)", true),
            job("34c55b2c", "Open bugs in panel/orchestrator", true),
            job("29ab7872", "Winversion", false),
        ];
        let parked: HashMap<TabId, String> = [(TAB, "970bf052".to_string())].into();
        let on = |title: &str| {
            let titles: HashMap<TabId, String> = [(TAB, title.to_string())].into();
            let mut v: Vec<(String, TabId)> =
                jobs_on_screen(&parked, &titles, &jobs, &HashSet::new())
                    .into_iter()
                    .collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        };
        assert_eq!(
            on("◐ Open bugs in panel/orchestrator"),
            [("34c55b2c".into(), TAB)]
        );
        assert_eq!(
            on("✳ Open bugs in panel/orchestrator (2)"),
            [("970bf052".into(), TAB)]
        );
        assert_eq!(
            on("⠂ Winversion"),
            [("970bf052".into(), TAB)],
            "not a live job"
        );
        assert_eq!(
            on("~/giverny"),
            [("970bf052".into(), TAB)],
            "the parked one"
        );
        assert_eq!(bare_title("⠐ ✳ Name (2) "), "Name (2)");
    }

    fn session(status: &str) -> giverny_claude::registry::SessionEntry {
        serde_json::from_str(&format!(
            r#"{{"pid":1,"sessionId":"s-1","status":"{status}"}}"#
        ))
        .expect("session fixture")
    }

    #[test]
    fn the_last_prompt_is_kept_per_tab() {
        let mut w = ClaudeWatch::for_tests();
        let other = |event: &str, extra: &str| {
            msg(&format!(
                r#"{{"tab_id":"giverny-8","config_dir":"/home/u/.claude-work",
                    "event":{{"hook_event_name":"{event}","session_id":"s-2"{extra}}}}}"#
            ))
        };
        feed(&mut w, &hook("SessionStart", ""), Some(TAB));
        assert_eq!(w.prompt_of(TAB), None, "nothing asked yet");

        feed(
            &mut w,
            &hook(
                "UserPromptSubmit",
                r#","prompt":"fix the build\nthen test""#,
            ),
            Some(TAB),
        );
        feed(
            &mut w,
            &other("UserPromptSubmit", r#","prompt":"other account""#),
            Some(TAB),
        );
        assert_eq!(w.prompt_of(TAB), Some("fix the build\nthen test"));
        assert_eq!(w.prompt_of(TabId(8)), Some("other account"));

        // Still shown once the turn is over; replaced by the next prompt.
        feed(&mut w, &hook("Stop", ""), Some(TAB));
        assert_eq!(w.prompt_of(TAB), Some("fix the build\nthen test"));
        feed(
            &mut w,
            &hook("UserPromptSubmit", r#","prompt":"  and lint  ""#),
            Some(TAB),
        );
        assert_eq!(w.prompt_of(TAB), Some("and lint"));
        // A payload without one leaves the last one standing.
        feed(&mut w, &hook("UserPromptSubmit", ""), Some(TAB));
        assert_eq!(w.prompt_of(TAB), Some("and lint"));

        // Claude gone: no bar, and the next session starts with none.
        feed(&mut w, &hook("SessionEnd", ""), Some(TAB));
        assert_eq!(w.prompt_of(TAB), None);
        feed(&mut w, &hook("SessionStart", ""), Some(TAB));
        assert_eq!(w.prompt_of(TAB), None);
        assert_eq!(w.prompt_of(TabId(8)), Some("other account"));
    }

    #[test]
    fn a_new_conversation_in_the_tab_drops_the_old_prompt() {
        let mut w = ClaudeWatch::for_tests();
        feed(&mut w, &hook("SessionStart", ""), Some(TAB));
        feed(
            &mut w,
            &hook("UserPromptSubmit", r#","prompt":"old""#),
            Some(TAB),
        );
        let resumed = msg(r#"{"tab_id":"giverny-7","config_dir":null,
            "event":{"hook_event_name":"SessionStart","session_id":"s-9"}}"#);
        feed(&mut w, &resumed, Some(TAB));
        assert_eq!(w.prompt_of(TAB), None);
    }

    /// A session no hook has reported a prompt for — adopted mid-way, or
    /// resumed after a restart — has it read back from its transcript.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_adopted_session_reads_its_prompt_from_the_transcript() {
        let dir = std::env::temp_dir().join(format!("giverny-adopt-{}", std::process::id()));
        let proj = dir.join("projects").join("-home-u-proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("s-1.jsonl"),
            concat!(
                r#"{"type":"user","message":{"content":"what changed\nsince friday"}}"#,
                "\n",
                r#"{"type":"last-prompt","lastPrompt":"what changed since friday"}"#,
                "\n",
            ),
        )
        .unwrap();

        let mut w = ClaudeWatch::for_tests();
        let me = std::process::id();
        let mut live = session("busy");
        live.pid = me;
        w.scanned = ScanResult {
            live: vec![registry::LiveSession {
                entry: live,
                config_dir: dir.clone(),
            }],
            ..ScanResult::default()
        };
        let shells = HashMap::from([(TAB, me)]);
        w.merge_scan(&shells, &mut WatchEffects::default());
        let deadline = Instant::now() + Duration::from_secs(5);
        while w.prompt_of(TAB).is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            w.apply_found_prompts();
        }
        assert_eq!(w.prompt_of(TAB), Some("what changed\nsince friday"));

        // Read once per session, not on every scan.
        w.merge_scan(&shells, &mut WatchEffects::default());
        assert!(w.prompts_asked.contains("s-1"));

        // A hook's prompt is newer than the transcript's and is not replaced.
        feed(
            &mut w,
            &hook("UserPromptSubmit", r#","prompt":"from the hook""#),
            Some(TAB),
        );
        w.prompts_found
            .0
            .send(("s-1".into(), "stale".into()))
            .unwrap();
        w.apply_found_prompts();
        assert_eq!(w.prompt_of(TAB), Some("from the hook"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_background_shell_is_not_the_agent_working() {
        // Measured against a live session: `busy` holds through minutes of
        // back-to-back tool calls, so `shell` is not "running a command" — it
        // is a shell left running while the agent waits at its prompt, often
        // with a question for you. Claude Code's own session list calls that
        // working; a spinner must not, or the tab that wants you looks like
        // the tab that doesn't.
        assert!(session("busy").busy());
        assert!(!session("shell").busy(), "the agent is at its prompt");
        assert!(session("shell").background_shell());
        assert!(!session("idle").busy());
        assert!(!session("waiting").busy());
        assert!(session("waiting").waiting(), "blocked on the user");
    }

    #[test]
    fn hooks_stay_authoritative_over_the_registry() {
        use ClaudeState::*;
        // Hooks mark the end of a turn exactly. The registry may not re-open
        // one they closed — a session left with a background shell reads as
        // "shell" for as long as that shell lives, which is hours.
        assert_eq!(merge_registry(Idle, true, &session("shell")), Idle);
        assert_eq!(merge_registry(Idle, true, &session("busy")), Idle);
        assert_eq!(merge_registry(Busy, true, &session("idle")), Busy);
        assert_eq!(
            merge_registry(DoneUnseen, true, &session("idle")),
            DoneUnseen
        );
        // The one exception: a working session clears a stale attention flag,
        // because declining a prompt emits no hook to clear it.
        assert_eq!(merge_registry(NeedsYou, true, &session("busy")), Busy);
        assert_eq!(merge_registry(NeedsYou, true, &session("idle")), NeedsYou);
    }

    #[test]
    fn without_hooks_the_registry_drives_every_state() {
        use ClaudeState::*;
        assert_eq!(merge_registry(Idle, false, &session("busy")), Busy);
        assert_eq!(merge_registry(Busy, false, &session("idle")), Idle);
        // A background shell leaves the agent idle: marked in the rail, not
        // spun.
        assert_eq!(merge_registry(Busy, false, &session("shell")), Idle);
        // A session blocked on a permission prompt with no hook to report it
        // used to read as idle: the flag now comes from the registry.
        assert_eq!(merge_registry(Idle, false, &session("waiting")), NeedsYou);
        // Attention states are the user's to clear, not the registry's.
        assert_eq!(
            merge_registry(DoneUnseen, false, &session("idle")),
            DoneUnseen
        );
    }

    #[test]
    fn a_subagent_finishing_does_not_end_the_turn() {
        let mut w = ClaudeWatch::for_tests();
        feed(&mut w, &hook("SessionStart", ""), Some(TabId(1)));
        feed(&mut w, &hook("UserPromptSubmit", ""), Some(TabId(1)));
        assert_eq!(w.state_of(TAB), ClaudeState::Busy);

        // These fire mid-turn, while the main agent carries on with the
        // result. Treating them as "finished" is what stopped the spinner
        // half way through the work.
        for kind in ["agent_completed", "task_completed"] {
            let m = hook("Notification", &format!(r#","notification_type":"{kind}""#));
            feed(&mut w, &m, Some(TabId(1)));
            assert_eq!(w.state_of(TAB), ClaudeState::Busy, "{kind} mid-turn");
        }

        // Once the session is genuinely at its prompt, it is done.
        let m = hook("Notification", r#","notification_type":"idle_prompt""#);
        feed(&mut w, &m, Some(TabId(1)));
        assert_eq!(w.state_of(TAB), ClaudeState::DoneUnseen);
    }

    #[test]
    fn typing_clears_a_stale_attention_flag() {
        // Declining a permission prompt (Escape) emits no hook at all — only
        // the user's keystroke tells us the flag is stale.
        let mut w = ClaudeWatch::for_tests();
        feed(&mut w, &hook("SessionStart", ""), Some(TAB));
        let m = hook(
            "Notification",
            r#","notification_type":"permission_prompt""#,
        );
        feed(&mut w, &m, Some(TAB));
        assert_eq!(w.state_of(TAB), ClaudeState::NeedsYou);

        w.mark_attended(TAB);
        assert_eq!(w.state_of(TAB), ClaudeState::Idle, "typing clears the flag");

        // Working tabs keep their spinner when the user types.
        feed(&mut w, &hook("UserPromptSubmit", ""), Some(TAB));
        w.mark_attended(TAB);
        assert_eq!(w.state_of(TAB), ClaudeState::Busy);
    }

    #[test]
    fn session_end_clears_state_and_resume_target() {
        let mut w = ClaudeWatch::for_tests();
        feed(&mut w, &hook("SessionStart", ""), Some(TAB));
        let fx = feed(&mut w, &hook("SessionEnd", ""), Some(TAB));
        assert_eq!(w.state_of(TAB), ClaudeState::None);
        assert_eq!(
            fx.captured,
            vec![(TAB, None, None)],
            "resume target cleared"
        );
    }

    #[test]
    fn statusline_push_updates_live_usage_not_tab_state() {
        let mut w = ClaudeWatch::for_tests();
        w.accounts.push(AccountPanel {
            profile: Profile {
                name: "acct".into(),
                config_dir: PathBuf::from("/tmp/giverny-test-acct"),
                email: None,
                account_uuid: None,
            },
            usage: None,
            live: None,
            peak: HashMap::new(),
            statusline_on: true,
        });
        let m = msg(
            r#"{"tab_id":"giverny-7","config_dir":"/tmp/giverny-test-acct",
                "event":{"hook_event_name":"GivernyStatusLine",
                         "rate_limits":{"five_hour":{"used_percentage":42.0},
                                        "seven_day":{"used_percentage":13.0}}}}"#,
        );
        feed(&mut w, &m, Some(TAB));
        let live = w.accounts[0].live.as_ref().expect("live usage recorded");
        assert_eq!(live.five_hour, Some(42.0));
        assert_eq!(live.seven_day, Some(13.0));
        assert_eq!(
            w.state_of(TAB),
            ClaudeState::None,
            "statusline is not tab state"
        );
    }

    #[test]
    fn live_percent_wins_only_when_fresher_than_cache() {
        use giverny_claude::usage::{AccountUsage, LimitEntry};
        let now = jiff::Timestamp::now();
        let limit: LimitEntry = serde_json::from_str(
            r#"{"kind":"session","percent":5,"severity":"normal","is_active":true}"#,
        )
        .unwrap();

        let mk = |cache_age_min: i64, live: Option<f64>| AccountPanel {
            profile: Profile {
                name: "a".into(),
                config_dir: PathBuf::from("/tmp/x"),
                email: None,
                account_uuid: None,
            },
            usage: Some(AccountUsage {
                fetched_at_ms: (now.as_millisecond() - cache_age_min * 60_000) as u64,
                limits: vec![],
            }),
            live: live.map(|p| LiveUsage {
                at: Instant::now(),
                five_hour: Some(p),
                seven_day: None,
                five_hour_resets: None,
                seven_day_resets: None,
            }),
            peak: HashMap::new(),
            statusline_on: true,
        };

        // Stale cache + fresh push ⇒ push wins and is flagged live.
        let read = ClaudeWatch::reading(&mk(120, Some(77.0)), &limit, now);
        assert_eq!((read.percent, read.live), (77.0, true));
        // No push ⇒ cache value, not flagged.
        let read = ClaudeWatch::reading(&mk(120, None), &limit, now);
        assert_eq!((read.percent, read.live), (5.0, false));
    }

    /// Why a rate-limited session never woke up again: the reopening time was
    /// read from the on-disk cache alone, and a rate-limited account is
    /// exactly the one whose cache cannot refresh, because refreshing it
    /// means running `claude` against the account that is out of limit.
    #[test]
    fn a_reopening_comes_from_the_push_when_the_cache_has_lapsed() {
        use giverny_claude::usage::{AccountUsage, LimitEntry};
        let now = jiff::Timestamp::now();
        let at = |mins: i64| now + jiff::Span::new().minutes(mins);
        let limit = |kind: &str, percent: u32, resets: jiff::Timestamp| -> LimitEntry {
            serde_json::from_str(&format!(
                r#"{{"kind":"{kind}","percent":{percent},"severity":"critical",
                     "is_active":true,"resets_at":"{resets}"}}"#
            ))
            .unwrap()
        };
        let panel = |name: &str, limits: Vec<LimitEntry>, live: Option<LiveUsage>| AccountPanel {
            profile: Profile {
                name: name.into(),
                config_dir: PathBuf::from("/tmp").join(name),
                email: None,
                account_uuid: None,
            },
            usage: Some(AccountUsage {
                fetched_at_ms: (now.as_millisecond() - 6 * 3_600_000) as u64,
                limits,
            }),
            live,
            peak: HashMap::new(),
            statusline_on: true,
        };

        // The cache is six hours old: its five-hour window "reopened" an hour
        // ago, which reads as no reopening at all. The push knows better.
        let mut w = ClaudeWatch::for_tests();
        w.accounts = vec![panel(
            "a",
            vec![limit("session", 100, at(-60))],
            Some(LiveUsage {
                at: Instant::now(),
                five_hour: Some(100.0),
                seven_day: Some(40.0),
                five_hour_resets: Some(at(35)),
                seven_day_resets: None,
            }),
        )];
        assert_eq!(w.window_reopens(Some("a")), Some(at(35)));

        // A tab whose session never fired a hook has no account against its
        // name. Waking on another account's window beats waiting forever.
        assert_eq!(w.window_reopens(None), Some(at(35)));
        assert_eq!(w.window_reopens(Some("nobody")), Some(at(35)));

        // Stopped by the weekly limit: the five-hour window coming round in
        // half an hour does not free it.
        let mut w = ClaudeWatch::for_tests();
        w.accounts = vec![panel(
            "a",
            vec![
                limit("session", 100, at(30)),
                limit("weekly_all", 100, at(4_000)),
            ],
            None,
        )];
        assert_eq!(w.window_reopens(Some("a")), Some(at(4_000)));

        // Only the five-hour window is out: the weekly one is not the answer.
        let mut w = ClaudeWatch::for_tests();
        w.accounts = vec![panel(
            "a",
            vec![
                limit("session", 100, at(30)),
                limit("weekly_all", 20, at(4_000)),
            ],
            None,
        )];
        assert_eq!(w.window_reopens(Some("a")), Some(at(30)));

        // Nothing known at all is still nothing: no guessing.
        let w = ClaudeWatch::for_tests();
        assert_eq!(w.window_reopens(Some("a")), None);
    }

    /// ita's bar bouncing between 90 and 99: two sources sampled at different
    /// moments, taking turns at being the fresher one. Within a window usage
    /// only goes up, so the bar does too.
    #[test]
    fn a_window_never_walks_backwards() {
        use giverny_claude::usage::{AccountUsage, LimitEntry};
        let now = jiff::Timestamp::now();
        let resets = now + jiff::Span::new().hours(2);
        let limit = |percent: u32, at: jiff::Timestamp| -> LimitEntry {
            serde_json::from_str(&format!(
                r#"{{"kind":"session","percent":{percent},"severity":"normal",
                     "is_active":true,"resets_at":"{at}"}}"#
            ))
            .unwrap()
        };
        let panel =
            |cache_age_min: i64, cache: LimitEntry, push: Option<(f64, u64)>| AccountPanel {
                profile: Profile {
                    name: "a".into(),
                    config_dir: PathBuf::from("/tmp/x"),
                    email: None,
                    account_uuid: None,
                },
                usage: Some(AccountUsage {
                    fetched_at_ms: (now.as_millisecond() - cache_age_min * 60_000) as u64,
                    limits: vec![cache],
                }),
                live: push.map(|(percent, age_s)| LiveUsage {
                    at: Instant::now() - Duration::from_secs(age_s),
                    five_hour: Some(percent),
                    seven_day: None,
                    five_hour_resets: Some(resets),
                    seven_day_resets: None,
                }),
                peak: HashMap::new(),
                statusline_on: true,
            };

        let mut w = ClaudeWatch::for_tests();
        // The cache is ten minutes old and the push just arrived: 99.
        w.accounts = vec![panel(10, limit(90, resets), Some((99.0, 1)))];
        w.remember_peaks();
        let acc = &w.accounts[0];
        let read = ClaudeWatch::reading(acc, &acc.usage.as_ref().unwrap().limits[0], now);
        assert_eq!(read.percent, 99.0);

        // `/usage` refreshes, writing a number it fetched minutes ago: the
        // freshest *sample* is now the lower one. The bar holds.
        let peak = w.accounts[0].peak.clone();
        w.accounts = vec![panel(0, limit(90, resets), Some((99.0, 600)))];
        w.accounts[0].peak = peak;
        w.remember_peaks();
        let acc = &w.accounts[0];
        let limits = &acc.usage.as_ref().unwrap().limits;
        assert_eq!(ClaudeWatch::sampled(acc, &limits[0], now).percent, 90.0);
        assert_eq!(ClaudeWatch::reading(acc, &limits[0], now).percent, 99.0);

        // The window resets: a different reset time is a different window, and
        // the mark goes with it.
        let later = now + jiff::Span::new().hours(7);
        let peak = w.accounts[0].peak.clone();
        w.accounts = vec![panel(0, limit(3, later), None)];
        w.accounts[0].peak = peak;
        w.remember_peaks();
        let acc = &w.accounts[0];
        let limits = &acc.usage.as_ref().unwrap().limits;
        assert_eq!(ClaudeWatch::reading(acc, &limits[0], now).percent, 3.0);
    }

    /// ita's 5h bar cycling 44 → 70 → 75 every second or two. Every running
    /// `claude` pushes the percentage its own last request was answered with,
    /// so a session idle since the window stood at 44 keeps saying 44 — with
    /// the current window's reset time — between the pushes of the busy ones.
    /// Thirty-one points below the mark read as a new window, and the mark
    /// started again from 44 on every lap. The cache, meanwhile, spells the
    /// same reset with microseconds the push does not have.
    #[test]
    fn an_idle_session_does_not_restart_the_window() {
        use giverny_claude::usage::{AccountUsage, LimitEntry};
        let now = jiff::Timestamp::now();
        // The push's reset, whole seconds; the cache's, the same moment
        // written the way `/usage` writes it.
        let resets = jiff::Timestamp::from_second(now.as_second() + 5_400).unwrap();
        let cache_resets = resets + jiff::SignedDuration::from_micros(62_934);
        let limit: LimitEntry = serde_json::from_str(&format!(
            r#"{{"kind":"session","percent":72,"severity":"warning",
                 "is_active":true,"resets_at":"{cache_resets}"}}"#
        ))
        .unwrap();
        let mut w = ClaudeWatch::for_tests();
        w.accounts.push(AccountPanel {
            profile: Profile {
                name: "acct".into(),
                config_dir: PathBuf::from("/tmp/giverny-test-acct"),
                email: None,
                account_uuid: None,
            },
            usage: Some(AccountUsage {
                fetched_at_ms: (now.as_millisecond() - 120_000) as u64,
                limits: vec![limit],
            }),
            live: None,
            peak: HashMap::new(),
            statusline_on: true,
        });
        // What `giverny statusline` relays: Claude Code's own payload shape.
        let push = |percent: u32| {
            msg(&format!(
                r#"{{"tab_id":"giverny-7","config_dir":"/tmp/giverny-test-acct",
                    "event":{{"hook_event_name":"GivernyStatusLine",
                             "rate_limits":{{"five_hour":{{"used_percentage":{percent},
                                                         "resets_at":{}}}}}}}}}"#,
                resets.as_second()
            ))
        };
        let shown = |w: &ClaudeWatch| {
            let acc = &w.accounts[0];
            ClaudeWatch::reading(acc, &acc.usage.as_ref().unwrap().limits[0], now).percent
        };

        // Before any push the cache is the only source: 72.
        w.remember_peaks();
        assert_eq!(shown(&w), 72.0);
        let mut seen = Vec::new();
        for _lap in 0..3 {
            for percent in [44, 70, 75] {
                // A tick: the peaks are brought up to date, then the push lands.
                w.remember_peaks();
                feed(&mut w, &push(percent), Some(TAB));
                seen.push(shown(&w));
                w.remember_peaks();
                seen.push(shown(&w));
            }
        }
        // It climbed to 75 once and stayed.
        assert!(
            seen.windows(2).all(|p| p[1] >= p[0]),
            "walked back: {seen:?}"
        );
        assert_eq!(seen.last(), Some(&75.0));

        // The window renews: a reset five hours on is a new window, and the
        // bar starts again from what the new window says.
        let next = jiff::Timestamp::from_second(resets.as_second() + 5 * 3600).unwrap();
        w.accounts[0].usage = None;
        w.accounts[0].peak.insert(
            "session".into(),
            Peak {
                percent: 75.0,
                resets: Some(resets),
            },
        );
        let fresh: LimitEntry = serde_json::from_str(&format!(
            r#"{{"kind":"session","percent":3,"severity":"normal",
                 "is_active":true,"resets_at":"{next}"}}"#
        ))
        .unwrap();
        w.accounts[0].live = None;
        w.accounts[0].usage = Some(AccountUsage {
            fetched_at_ms: now.as_millisecond() as u64,
            limits: vec![fresh],
        });
        w.remember_peaks();
        assert_eq!(shown(&w), 3.0);
    }

    /// An account whose cache has stopped being refreshed, which is every
    /// account whose `claude` Giverny cannot run: the statusline push is the
    /// only thing still telling the truth, and the cache holds whatever it
    /// last managed to fetch.
    #[test]
    fn a_stale_cache_decides_nothing_the_push_has_answered() {
        use giverny_claude::usage::{AccountUsage, LimitEntry};
        let now: jiff::Timestamp = "2025-10-09T12:00:00Z".parse().unwrap();
        let at = |s: &str| -> jiff::Timestamp { s.parse().unwrap() };
        // What his cache still held: a week that ran out, days ago.
        let limit: LimitEntry = serde_json::from_str(
            r#"{"kind":"weekly_all","percent":97,"severity":"critical","is_active":true,
                "resets_at":"2025-10-06T00:00:00Z"}"#,
        )
        .unwrap();
        let mk = |cache_age_min: i64, live: Option<LiveUsage>| AccountPanel {
            profile: Profile {
                name: "a".into(),
                config_dir: PathBuf::from("/tmp/x"),
                email: None,
                account_uuid: None,
            },
            usage: Some(AccountUsage {
                fetched_at_ms: (now.as_millisecond() - cache_age_min * 60_000) as u64,
                limits: vec![],
            }),
            live,
            peak: HashMap::new(),
            statusline_on: true,
        };
        let push = |percent: f64, resets: Option<&str>| LiveUsage {
            at: Instant::now(),
            five_hour: None,
            seven_day: Some(percent),
            five_hour_resets: None,
            seven_day_resets: resets.map(at),
        };

        // The bar ita saw red at 21%: the percentage was the push's, the
        // colour the cache's. One source answers for a window, or none does.
        let read = ClaudeWatch::reading(
            &mk(4_000, Some(push(21.0, Some("2025-10-12T13:00:00Z")))),
            &limit,
            now,
        );
        assert_eq!(
            (read.percent, read.live, read.critical),
            (21.0, true, false)
        );
        assert_eq!(read.resets, Some(at("2025-10-12T13:00:00Z")));

        // Still critical when the number itself says so.
        let read = ClaudeWatch::reading(&mk(4_000, Some(push(99.0, None))), &limit, now);
        assert!(read.critical);

        // No push at all: the window the cache measured has lapsed, so it
        // reports neither its percentage nor its severity nor its reset.
        let read = ClaudeWatch::reading(&mk(4_000, None), &limit, now);
        assert_eq!(
            (read.percent, read.live, read.critical),
            (0.0, false, false)
        );
        assert_eq!(read.resets, None);
    }

    /// A cache that is still describing the window it is in keeps its say.
    #[test]
    fn a_current_cache_is_believed() {
        use giverny_claude::usage::{AccountUsage, LimitEntry};
        let now: jiff::Timestamp = "2025-10-09T12:00:00Z".parse().unwrap();
        let limit: LimitEntry = serde_json::from_str(
            r#"{"kind":"weekly_all","percent":88,"severity":"critical","is_active":true,
                "resets_at":"2025-10-09T14:30:00Z"}"#,
        )
        .unwrap();
        let acc = AccountPanel {
            profile: Profile {
                name: "a".into(),
                config_dir: PathBuf::from("/tmp/x"),
                email: None,
                account_uuid: None,
            },
            usage: Some(AccountUsage {
                fetched_at_ms: (now.as_millisecond() - 60_000) as u64,
                limits: vec![],
            }),
            live: None,
            peak: HashMap::new(),
            statusline_on: true,
        };
        let read = ClaudeWatch::reading(&acc, &limit, now);
        assert_eq!(
            (read.percent, read.live, read.critical),
            (88.0, false, true)
        );
        assert_eq!(read.resets, Some("2025-10-09T14:30:00Z".parse().unwrap()));
    }

    #[test]
    fn a_reset_is_read_however_it_is_written() {
        let at = |v: serde_json::Value| super::reset_time(&v).map(|t| t.as_second());
        // Epoch seconds, epoch milliseconds, and RFC 3339 all mean the moment.
        assert_eq!(
            at(serde_json::json!({ "resets_at": 1_760_000_000 })),
            Some(1_760_000_000)
        );
        assert_eq!(
            at(serde_json::json!({ "resets_at_ms": 1_760_000_000_000i64 })),
            Some(1_760_000_000)
        );
        assert_eq!(
            at(serde_json::json!({ "reset_at": "2025-10-09T08:53:20Z" })),
            Some(1_760_000_000)
        );
        // Nothing to read, or something unreadable, is no reset rather than a
        // wrong one — the bar then simply says nothing about renewal.
        assert_eq!(at(serde_json::json!({ "used_percentage": 12 })), None);
        assert_eq!(at(serde_json::json!({ "resets_at": "soon" })), None);
    }

    #[test]
    fn refresh_waits_for_the_numbers_to_age() {
        let mins = |m: u64| Some(Duration::from_secs(m * 60));
        // Young numbers are left alone however long ago we last asked.
        assert!(!ClaudeWatch::refresh_due(3, None, 10));
        assert!(!ClaudeWatch::refresh_due(9, mins(60), 10));
        // Aged out and never asked, or asked long enough ago.
        assert!(ClaudeWatch::refresh_due(10, None, 10));
        assert!(ClaudeWatch::refresh_due(45, mins(11), 10));
    }

    #[test]
    fn an_account_that_never_caches_is_not_retried_in_a_loop() {
        // No readable cache reads as infinitely old, so only the attempt clock
        // stands between us and spawning `claude -p /usage` every tick.
        assert!(ClaudeWatch::refresh_due(i64::MAX, None, 10));
        for secs in [1, 30, 120, 599] {
            assert!(
                !ClaudeWatch::refresh_due(i64::MAX, Some(Duration::from_secs(secs)), 10),
                "retried {secs}s after the last attempt"
            );
        }
        assert!(ClaudeWatch::refresh_due(
            i64::MAX,
            Some(Duration::from_secs(600)),
            10
        ));
    }

    /// A side instance writes nothing into an account — not at
    /// startup, not from the UI — while the same calls on the installed
    /// instance do rewrite it (so this test would see a write).
    #[test]
    fn a_side_instance_leaves_the_account_alone() {
        use std::ffi::OsStr;
        assert!(!leaves_accounts_alone(None));
        assert!(!leaves_accounts_alone(Some(OsStr::new(""))));
        assert!(!leaves_accounts_alone(Some(OsStr::new("0"))));
        assert!(leaves_accounts_alone(Some(OsStr::new("1"))));
        assert!(leaves_accounts_alone(Some(OsStr::new("yes"))));

        let root = std::env::temp_dir().join(format!(
            "giverny-side-instance-{}-{}",
            std::process::id(),
            jiff::Timestamp::now().as_nanosecond()
        ));
        let account = root.join("claude");
        let base = root.join("giverny");
        std::fs::create_dir_all(account.join("plugins")).unwrap();
        let settings = account.join("settings.json");
        let known = account.join("plugins/known_marketplaces.json");
        // Hooks that point at some other Giverny: exactly what a side
        // instance would otherwise "refresh" to its own executable.
        std::fs::write(
            &settings,
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/elsewhere/giverny relay"}]}]},"statusLine":{"type":"command","command":"/elsewhere/giverny relay --statusline"},"extraKnownMarketplaces":{"giverny":{"source":{"source":"directory","path":"/elsewhere/plugin"}}},"enabledPlugins":{"giverny@giverny":true}}"#,
        )
        .unwrap();
        std::fs::write(
            &known,
            r#"{"giverny":{"source":{"source":"directory","path":"/elsewhere/plugin"}}}"#,
        )
        .unwrap();
        let before = (
            std::fs::read(&settings).unwrap(),
            std::fs::read(&known).unwrap(),
        );
        let profile = Profile {
            name: "test".into(),
            config_dir: account.clone(),
            email: None,
            account_uuid: None,
        };

        let mut w = ClaudeWatch::for_tests();
        w.profiles = vec![profile.clone()];
        w.leave_accounts = true;
        ClaudeWatch::adopt_statusline_where_hooked(&w.profiles, true);
        assert!(w.install_hooks().is_err());
        assert!(w.set_statusline(true).is_err());
        assert!(w.set_statusline(false).is_err());
        w.set_auto_mode(true);
        w.ensure_auto_mode();
        w.set_agents_pane(true, true, &base);
        w.set_agents_pane(false, true, &base);
        let after = (
            std::fs::read(&settings).unwrap(),
            std::fs::read(&known).unwrap(),
        );
        assert!(before == after, "a side instance rewrote the account");
        assert!(!base.exists(), "a side instance wrote a plugin dir");

        // The installed instance, same calls: the account does change.
        w.leave_accounts = false;
        w.set_agents_pane(false, true, &base);
        w.set_auto_mode(true);
        assert_ne!(std::fs::read(&settings).unwrap(), before.0);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The pane is on by default, so the setting alone is not consent: an
    /// account without our hooks is left byte-identical, and one with them
    /// gets the pane's keys, which go again when the pane goes off.
    #[test]
    fn the_pane_writes_only_where_the_hooks_are() {
        let root = std::env::temp_dir().join(format!(
            "giverny-pane-consent-{}-{}",
            std::process::id(),
            jiff::Timestamp::now().as_nanosecond()
        ));
        let base = root.join("giverny");
        let profile = |name: &str| {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            Profile {
                name: name.into(),
                config_dir: dir,
                email: None,
                account_uuid: None,
            }
        };
        let (plain, hooked) = (profile("plain"), profile("hooked"));
        let plain_settings = plain.config_dir.join("settings.json");
        let hooked_settings = hooked.config_dir.join("settings.json");
        let text = "{\n  \"model\": \"opus\"\n}\n";
        std::fs::write(&plain_settings, text).unwrap();
        std::fs::write(&hooked_settings, text).unwrap();
        hooks::install_into(&hooked_settings).unwrap();

        let mut w = ClaudeWatch::for_tests();
        w.profiles = vec![plain.clone(), hooked.clone()];
        w.leave_accounts = false;
        w.set_agents_pane(true, true, &base);
        assert_eq!(std::fs::read_to_string(&plain_settings).unwrap(), text);
        assert!(
            !plain.config_dir.join("settings.json.giverny-bak").exists(),
            "no backup for a write that never happened"
        );
        assert!(hooks::subagent_line_installed_in(&hooked_settings));
        assert!(giverny_claude::plugin::installed_in(&hooked_settings));
        let skill =
            giverny_claude::plugin::marketplace_dir(&base).join(giverny_claude::plugin::SKILL_PATH);
        assert!(skill.exists());

        // The skill off: only the skill goes; the plugin and the pane's
        // keys stay, and the plain account is still untouched.
        w.set_agents_pane(true, false, &base);
        assert!(!skill.exists());
        assert!(giverny_claude::plugin::installed_in(&hooked_settings));
        assert!(hooks::subagent_line_installed_in(&hooked_settings));
        assert_eq!(std::fs::read_to_string(&plain_settings).unwrap(), text);
        w.set_agents_pane(true, true, &base);
        assert!(skill.exists());

        w.set_agents_pane(false, true, &base);
        assert_eq!(std::fs::read_to_string(&plain_settings).unwrap(), text);
        assert!(!hooks::subagent_line_installed_in(&hooked_settings));
        assert!(!giverny_claude::plugin::installed_in(&hooked_settings));

        let _ = std::fs::remove_dir_all(&root);
    }
}

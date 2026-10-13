//! WSLg watchdog: after a sleep, bring the window back by itself.
//!
//! A Linux window on WSLg is drawn on Windows by `msrdc.exe`, an RDP client
//! that WSLGd starts and starts again whenever it exits. Weston, WSLg's
//! compositor, logs the link to it in `/mnt/wslg/weston.log`: `Client
//! HandShake` when msrdc connects, `unable to checkDescriptor` when it goes,
//! and `ClientGetAppidReq: pid:<pid>` when msrdc creates a window on its side
//! for a Linux process's window. On a healthy reconnect every window is
//! created again within a second of the handshake (2026-10-12: 15:39:39 and
//! 16:15:37, 0.8 s each).
//!
//! The wedge: after the laptop resumed at 15:55:29 msrdc reconnected
//! (handshake 15:55:31) and this window was not created on Windows' side. The
//! window was gone, though the app ran on, until a second launch made it come
//! forward (`come_forward`), whose unmap and map is a new window to Windows:
//! that announcement came at 15:55:51, at a click on the Start menu. The other gaps in
//! that log (15:22 to 15:39, 15:48 to 15:55) are the VM itself frozen while
//! the laptop slept: the journal has nothing in them either.
//!
//! So, polling the log every few seconds:
//! - connected, armed (this window was shown on Windows before), and not shown
//!   [`ANNOUNCE_WITHIN`] after the handshake: unmap and map the window again,
//!   the cure the 15:55 click applied by hand;
//! - still not shown [`AFTER_REMAP`] later: end WSLg's msrdc, so WSLGd
//!   starts a new one (the 16:15 path; measured: the window is back 1.1 s
//!   after the kill);
//! - with `GIVERNY_WSLG_WATCHDOG_LINK_DOWN=1` only, also the link down for
//!   [`DOWN_FOR`] with the VM awake. Off by default: no link says nothing
//!   about this window, and a VM that runs on while Windows sleeps or its
//!   display is off looks the same.
//!
//! Killing msrdc blinks every WSLg window, so it never happens on a healthy
//! link: only after the remap failed or with no link at all, at most once per
//! [`KILL_EVERY`], and never more than [`KILLS_UNANSWERED`] times in a row
//! without the window coming back. Time is the wall clock, and a poll that
//! comes much later than it should means the VM was frozen: every timer
//! starts again rather than count the sleep as time the link was broken.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// Where WSLg's compositor logs; rewritten each time WSLg starts.
const WESTON_LOG: &str = "/mnt/wslg/weston.log";

/// How often the log is read.
const POLL: Duration = Duration::from_secs(5);

/// A poll this much later than the last one means the VM was frozen (the
/// laptop asleep): its monotonic clock stood still, the wall clock did not.
const FROZEN_AFTER: f64 = 20.0;

/// Seconds after a handshake by which this window should be shown again. A
/// healthy reconnect takes under one.
const ANNOUNCE_WITHIN: f64 = 10.0;

/// Seconds after the remap by which the window should be shown, or msrdc is
/// past helping.
const AFTER_REMAP: f64 = 15.0;

/// Seconds of no link at all, with the VM awake, before msrdc is ended.
/// WSLGd starts a new one within a second of the old one exiting, so a link
/// down this long is an msrdc that is alive and not reconnecting.
const DOWN_FOR: f64 = 90.0;

/// At most one kill per this many seconds.
const KILL_EVERY: f64 = 600.0;

/// Kills in a row that did not bring the window back before the watchdog
/// stops killing, until it is shown again.
const KILLS_UNANSWERED: u32 = 2;

/// How long ending msrdc may take before it is given up on (interop to a
/// Windows that is still waking can hang).
const KILL_TIMEOUT: Duration = Duration::from_secs(20);

/// What a line of weston's log says about the link and this window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    /// msrdc connected.
    Up,
    /// The connection is gone.
    Down,
    /// msrdc created a window on Windows' side for this process.
    Shown,
}

fn parse_line(line: &str, pid: u32) -> Option<Event> {
    if line.contains("Client HandShake") {
        return Some(Event::Up);
    }
    if line.contains("unable to checkDescriptor")
        || line.contains("stopRdpNotifyEvent is signalled")
    {
        return Some(Event::Down);
    }
    let rest = line.split("ClientGetAppidReq: pid:").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (digits.parse::<u32>().ok()? == pid).then_some(Event::Shown)
}

/// The VM's id, which WSLg's msrdc carries on its command line.
fn parse_vm_id(log: &str) -> Option<String> {
    log.lines().find_map(|line| {
        let id = line.split("WSL2_VM_ID=").nth(1)?.trim();
        (!id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
            .then(|| id.to_string())
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Link {
    Unknown,
    Up {
        since: f64,
        shown: bool,
        remapped: Option<f64>,
        /// The kill for this connection was decided (or held back).
        escalated: bool,
    },
    Down {
        since: f64,
    },
}

/// What the watchdog decided at a poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Unmap and map the window again.
    Remap,
    /// End WSLg's msrdc.
    Kill,
    /// A kill was called for and held back by the limits.
    HeldBack,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub step: Step,
    pub why: String,
}

/// The decisions, apart from reading the log and acting, so that they can be
/// replayed over a real log.
#[derive(Debug)]
pub struct Watchdog {
    pid: u32,
    /// This window has been shown on Windows: under this WSLg, and so it is
    /// missing when it is not.
    armed: bool,
    link: Link,
    last_poll: Option<f64>,
    last_kill: Option<f64>,
    unanswered: u32,
    /// A held-back kill was reported since the link last changed.
    held_said: bool,
    /// Also end msrdc after [`DOWN_FOR`] with no link: opt-in, see the
    /// module docs.
    kills_on_link_down: bool,
}

impl Watchdog {
    pub fn new(pid: u32) -> Self {
        Watchdog {
            pid,
            armed: false,
            link: Link::Unknown,
            last_poll: None,
            last_kill: None,
            unanswered: 0,
            held_said: false,
            kills_on_link_down: false,
        }
    }

    /// Also end msrdc when the link has been down for [`DOWN_FOR`].
    pub fn killing_on_link_down(self) -> Self {
        Watchdog {
            kills_on_link_down: true,
            ..self
        }
    }

    /// WSLg started again with a new log: what was known of the link is gone,
    /// the kill limits are not.
    pub fn log_restarted(&mut self) {
        self.armed = false;
        self.link = Link::Unknown;
    }

    /// Take what the log gained since the last poll, at wall-clock `now` in
    /// seconds, and decide.
    pub fn poll(&mut self, now: f64, new_text: &str) -> Option<Decision> {
        if let Some(last) = self.last_poll
            && !(0.0..=FROZEN_AFTER).contains(&(now - last))
        {
            // Asleep in between: nothing that happened in the log before
            // the sleep has been broken for as long as the clock says.
            self.link = match self.link {
                Link::Up { shown, .. } => Link::Up {
                    since: now,
                    shown,
                    remapped: None,
                    escalated: false,
                },
                Link::Down { .. } => Link::Down { since: now },
                Link::Unknown => Link::Unknown,
            };
        }
        self.last_poll = Some(now);
        for event in new_text.lines().filter_map(|l| parse_line(l, self.pid)) {
            self.held_said = false;
            self.link = match event {
                Event::Up => Link::Up {
                    since: now,
                    shown: false,
                    remapped: None,
                    escalated: false,
                },
                Event::Down => Link::Down { since: now },
                Event::Shown => {
                    self.armed = true;
                    self.unanswered = 0;
                    match self.link {
                        Link::Up { since, .. } => Link::Up {
                            since,
                            shown: true,
                            remapped: None,
                            escalated: false,
                        },
                        _ => Link::Up {
                            since: now,
                            shown: true,
                            remapped: None,
                            escalated: false,
                        },
                    }
                }
            };
        }
        if !self.armed {
            return None;
        }
        let kills_on_link_down = self.kills_on_link_down;
        match &mut self.link {
            Link::Up {
                since,
                shown: false,
                remapped: remapped @ None,
                ..
            } if now - *since >= ANNOUNCE_WITHIN => {
                *remapped = Some(now);
                Some(Decision {
                    step: Step::Remap,
                    why: format!(
                        "WSLg reconnected {:.0} s ago and has not shown this window",
                        now - *since
                    ),
                })
            }
            Link::Up {
                shown: false,
                remapped: Some(at),
                escalated: escalated @ false,
                ..
            } if now - *at >= AFTER_REMAP => {
                *escalated = true;
                let why = format!(
                    "this window was mapped again {:.0} s ago and WSLg still has not shown it",
                    now - *at
                );
                self.kill(now, why)
            }
            Link::Down { since } if kills_on_link_down && now - *since >= DOWN_FOR => {
                let why = format!(
                    "WSLg's link to Windows has been down for {:.0} s",
                    now - *since
                );
                *since = now;
                self.kill(now, why)
            }
            _ => None,
        }
    }

    /// A kill, if the limits allow one; if not, say so once.
    fn kill(&mut self, now: f64, why: String) -> Option<Decision> {
        let held = |dog: &mut Self, why: String| {
            (!std::mem::replace(&mut dog.held_said, true)).then_some(Decision {
                step: Step::HeldBack,
                why,
            })
        };
        if self.unanswered >= KILLS_UNANSWERED {
            let why = format!(
                "{why}; not ending msrdc: {} kills in a row did not bring the window back",
                self.unanswered
            );
            return held(self, why);
        }
        if let Some(last) = self.last_kill
            && now - last < KILL_EVERY
        {
            let why = format!(
                "{why}; not ending msrdc: the last kill was {:.0} s ago",
                now - last
            );
            return held(self, why);
        }
        self.last_kill = Some(now);
        self.unanswered += 1;
        Some(Decision {
            step: Step::Kill,
            why,
        })
    }
}

/// Reads what a log file gained since the last read, whole lines only, and
/// notices when it was rewritten.
struct Tail {
    path: PathBuf,
    offset: u64,
    partial: String,
}

impl Tail {
    fn new(path: PathBuf) -> Self {
        Tail {
            path,
            offset: 0,
            partial: String::new(),
        }
    }

    /// The new whole lines, and whether the file started again since.
    fn read(&mut self) -> std::io::Result<(String, bool)> {
        let mut file = std::fs::File::open(&self.path)?;
        let len = file.metadata()?.len();
        let restarted = len < self.offset;
        if restarted {
            self.offset = 0;
            self.partial.clear();
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        file.take(len - self.offset).read_to_end(&mut bytes)?;
        self.offset += bytes.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&bytes));
        let whole = match self.partial.rfind('\n') {
            Some(end) => {
                let rest = self.partial.split_off(end + 1);
                std::mem::replace(&mut self.partial, rest)
            }
            None => String::new(),
        };
        Ok((whole, restarted))
    }
}

/// The UI's end: whether the watchdog wants the window mapped again.
pub struct Handle {
    remap: Arc<AtomicBool>,
}

impl Handle {
    /// True once per remap the watchdog asked for.
    pub fn take_remap(&self) -> bool {
        self.remap.swap(false, Ordering::AcqRel)
    }
}

/// Whether the watchdog runs here: on WSLg, unless `GIVERNY_WSLG_WATCHDOG=0`.
pub fn applies() -> bool {
    crate::wslg::is_wslg()
        && !matches!(
            std::env::var("GIVERNY_WSLG_WATCHDOG").as_deref(),
            Ok("0" | "off")
        )
}

/// Which log to read: WSLg's, or (debug builds) a fixture to replay.
fn log_path() -> PathBuf {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("GIVERNY_DEBUG_WESTON_LOG") {
        return PathBuf::from(path);
    }
    PathBuf::from(WESTON_LOG)
}

/// Kills are only logged, not done, with `GIVERNY_WSLG_WATCHDOG=dry-run`.
fn dry_run() -> bool {
    std::env::var("GIVERNY_WSLG_WATCHDOG").as_deref() == Ok("dry-run")
}

/// The no-link kill is asked for with `GIVERNY_WSLG_WATCHDOG_LINK_DOWN=1`.
fn kills_on_link_down() -> bool {
    std::env::var("GIVERNY_WSLG_WATCHDOG_LINK_DOWN").as_deref() == Ok("1")
}

/// Start watching, on WSLg; `wake` brings the UI round to take a remap.
pub fn start(record: PathBuf, wake: impl Fn() + Send + 'static) -> Option<Handle> {
    if !applies() {
        return None;
    }
    let remap = Arc::new(AtomicBool::new(false));
    let flag = remap.clone();
    let path = log_path();
    let dry = dry_run();
    std::thread::Builder::new()
        .name("wslg watchdog".into())
        .spawn(move || watch(&path, &record, dry, &flag, &wake))
        .ok()?;
    tracing::info!(
        "WSLg watchdog: watching {}{}",
        log_path().display(),
        if dry { " (dry run: no kills)" } else { "" }
    );
    Some(Handle { remap })
}

fn wall_now() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn watch(path: &Path, record: &Path, dry: bool, remap: &AtomicBool, wake: &dyn Fn()) {
    let mut dog = Watchdog::new(std::process::id());
    if kills_on_link_down() {
        dog = dog.killing_on_link_down();
    }
    let mut tail = Tail::new(path.to_path_buf());
    let mut vm_id = None;
    loop {
        match tail.read() {
            Ok((text, restarted)) => {
                if restarted {
                    dog.log_restarted();
                }
                if vm_id.is_none() {
                    vm_id = parse_vm_id(&text);
                }
                if let Some(decision) = dog.poll(wall_now(), &text) {
                    act(&decision, vm_id.as_deref(), dry, record, remap, wake);
                }
            }
            Err(err) => tracing::debug!("WSLg watchdog: {}: {err}", path.display()),
        }
        std::thread::sleep(POLL);
    }
}

fn act(
    decision: &Decision,
    vm_id: Option<&str>,
    dry: bool,
    record: &Path,
    remap: &AtomicBool,
    wake: &dyn Fn(),
) {
    let line = match decision.step {
        Step::Remap => {
            remap.store(true, Ordering::Release);
            wake();
            format!("{}: mapping the window again", decision.why)
        }
        Step::Kill if dry => format!("{}: would end msrdc (dry run)", decision.why),
        Step::Kill => {
            let started = Instant::now();
            let outcome = match kill_msrdc(vm_id) {
                Ok(pids) if pids.is_empty() => "no WSLg msrdc was running".to_string(),
                Ok(pids) => format!("ended msrdc (Windows pid {})", pids.join(", ")),
                Err(why) => format!("could not end msrdc: {why}"),
            };
            format!(
                "{}: {outcome} in {:.1} s; WSLGd starts a new one",
                decision.why,
                started.elapsed().as_secs_f64()
            )
        }
        Step::HeldBack => decision.why.clone(),
    };
    tracing::warn!("WSLg watchdog: {line}");
    remember(record, &line);
}

/// End WSLg's msrdc, and only it: the same program is Windows' Remote Desktop
/// client, so it is picked by its `/wslg` command line and this VM's id.
fn kill_msrdc(vm_id: Option<&str>) -> Result<Vec<String>, String> {
    let pattern = match vm_id {
        Some(id) => format!("*/wslg *{id}*"),
        None => "*/wslg *".to_string(),
    };
    let script = format!(
        "Get-CimInstance Win32_Process -Filter \"Name='msrdc.exe'\" | \
         Where-Object {{ $_.CommandLine -like '{pattern}' }} | \
         ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force; $_.ProcessId }}"
    );
    let ps = ["/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .unwrap_or("powershell.exe");
    let mut child = Command::new(ps)
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{ps} did not start: {e}"))?;
    let deadline = Instant::now() + KILL_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("PowerShell still running after {KILL_TIMEOUT:?}"));
            }
            Err(e) => return Err(format!("waiting for PowerShell: {e}")),
        }
    };
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut err);
    }
    if !status.success() {
        return Err(format!("PowerShell: {status}: {}", err.trim()));
    }
    Ok(out
        .split_whitespace()
        .filter(|w| w.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_string)
        .collect())
}

/// Where what the watchdog did is kept, for `giverny doctor`.
pub fn record_path(base: &Path) -> PathBuf {
    base.join("state").join("wslg-watchdog.log")
}

/// The most this keeps; older lines go first.
const RECORD_MAX: usize = 32 * 1024;

fn remember(record: &Path, line: &str) {
    let stamp = jiff::Zoned::now().strftime("%Y-%m-%d %H:%M:%S");
    let mut text = std::fs::read_to_string(record).unwrap_or_default();
    text.push_str(&format!("{stamp}  {line}\n"));
    if text.len() > RECORD_MAX {
        let cut = text.len() - RECORD_MAX / 2;
        let cut = text[cut..].find('\n').map_or(cut, |n| cut + n + 1);
        text.drain(..cut);
    }
    if let Some(dir) = record.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(err) = std::fs::write(record, text) {
        tracing::debug!("WSLg watchdog: {}: {err}", record.display());
    }
}

/// The last few things the watchdog did, newest last, for `giverny doctor`.
pub fn recent(base: &Path, n: usize) -> Vec<String> {
    let text = std::fs::read_to_string(record_path(base)).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WSLg's log from the day this was found (2026-10-12), cut to the lines
    /// that are about the link, with the environment dump left out. Giverny
    /// was pid 825 throughout; other pids are test windows.
    const REAL: &str = include_str!("../testdata/weston-2026-10-12.txt");
    const PID: u32 = 825;

    /// When the VM was frozen, from the gaps in that day's journal (nothing
    /// at all logged, not even the timers that run every 30 s).
    const ASLEEP: [(&str, &str); 3] = [
        ("15:22:51", "15:39:33"),
        ("15:48:42", "15:55:29"),
        ("16:15:42", "16:22:26"),
    ];

    fn secs(hms: &str) -> f64 {
        let mut parts = hms.split(':').map(|p| p.parse::<f64>().unwrap());
        let (h, m, s) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        h * 3600.0 + m * 60.0 + s
    }

    /// A line's `[HH:MM:SS.mmm]`, in seconds; continuation lines have none.
    fn stamp(line: &str) -> Option<f64> {
        let inner = line.strip_prefix('[')?.split(']').next()?;
        inner.contains(':').then(|| secs(inner))
    }

    /// Run the watchdog over a log as it would have run live: a poll every
    /// five seconds, each taking the lines stamped up to then, and no polls
    /// at all while the VM was frozen.
    fn replay(log: &str, pid: u32, asleep: &[(&str, &str)]) -> Vec<(String, Decision)> {
        replay_with(Watchdog::new(pid), log, asleep)
    }

    fn replay_with(
        mut dog: Watchdog,
        log: &str,
        asleep: &[(&str, &str)],
    ) -> Vec<(String, Decision)> {
        let asleep: Vec<(f64, f64)> = asleep.iter().map(|(a, b)| (secs(a), secs(b))).collect();
        let mut lines = Vec::new();
        let mut at = 0.0;
        for line in log.lines() {
            at = stamp(line).unwrap_or(at);
            lines.push((at, line));
        }
        let start = lines.iter().find(|(t, _)| *t > 0.0).map_or(0.0, |l| l.0);
        let end = lines.last().map_or(0.0, |l| l.0) + 60.0;
        let mut next = 0;
        let mut out = Vec::new();
        let mut now = start;
        while now < end {
            if let Some(&(_, wake)) = asleep.iter().find(|(a, b)| (*a..*b).contains(&now)) {
                now = wake;
                continue;
            }
            let mut text = String::new();
            while next < lines.len() && lines[next].0 <= now {
                text.push_str(lines[next].1);
                text.push('\n');
                next += 1;
            }
            if let Some(d) = dog.poll(now, &text) {
                let t = now as u32;
                out.push((
                    format!("{:02}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60),
                    d,
                ));
            }
            now += POLL.as_secs_f64();
        }
        out
    }

    fn steps(decisions: &[(String, Decision)]) -> Vec<(&str, Step)> {
        decisions
            .iter()
            .map(|(t, d)| (t.as_str(), d.step))
            .collect()
    }

    #[test]
    fn lines_read_as_the_link_and_this_window() {
        let at = |l: &str| parse_line(l, PID);
        assert_eq!(
            at("[15:55:31.024] Client HandShake buildNumber:26200"),
            Some(Event::Up)
        );
        assert_eq!(
            at("[15:48:41.618] unable to checkDescriptor for 0x6264cfd235b0"),
            Some(Event::Down)
        );
        assert_eq!(
            at("[16:15:37.912] Client: ClientGetAppidReq: pid:825 appId:giverny WindowId:0x1"),
            Some(Event::Shown)
        );
        // Another process's window, and a pid that only starts like ours.
        assert_eq!(
            at("[12:16:01.678] Client: ClientGetAppidReq: pid:100385 appId:giverny WindowId:0x9"),
            None
        );
        assert_eq!(
            at("[12:16:01.678] Client: ClientGetAppidReq: pid:8250 appId:giverny WindowId:0x9"),
            None
        );
        assert_eq!(
            at("[16:28:11.249] rdp_rail_idle_handler is called on peer_ctx:0x1"),
            None
        );
        assert_eq!(
            at("[16:32:01.989] rdp_rail_wake_handler is called on peer_ctx:0x1"),
            None
        );
    }

    #[test]
    fn the_day_it_happened_remaps_once_when_the_window_stayed_missing() {
        // 15:55:31 reconnect, window not shown until a click at 15:55:51:
        // remapped on the first poll ten seconds after the handshake was seen.
        // The 15:39 and 16:15 reconnects, the kill measured at 16:47, the idle
        // and wake churn all day, and the three sleeps decide nothing.
        let decisions = replay(REAL, PID, &ASLEEP);
        let steps = steps(&decisions);
        assert_eq!(steps.len(), 1, "{decisions:#?}");
        let (at, step) = steps[0];
        assert_eq!(step, Step::Remap);
        assert!(("15:55:41"..="15:55:50").contains(&at), "{at}");
    }

    #[test]
    fn a_window_that_never_comes_back_gets_msrdc_ended_once() {
        // The same morning, but nobody clicked: cut the log before 15:55:51.
        let cut: String = REAL
            .lines()
            .take_while(|l| stamp(l).is_none_or(|t| t < secs("15:55:50")))
            .map(|l| format!("{l}\n"))
            .collect();
        let decisions = replay(&cut, PID, &ASLEEP);
        let steps = steps(&decisions);
        assert_eq!(
            steps.iter().map(|s| s.1).collect::<Vec<_>>(),
            [Step::Remap, Step::Kill],
            "{decisions:#?}"
        );
        assert!(
            decisions[1].1.why.contains("mapped again"),
            "{decisions:#?}"
        );
    }

    #[test]
    fn the_sleeps_are_not_counted_as_the_link_being_down() {
        // Had the VM stayed awake from 15:48:41 to 15:55:31 with no link: by
        // default nothing comes of it, since no link says nothing about this
        // window.
        let decisions = replay(REAL, PID, &ASLEEP[..1]);
        assert!(
            decisions.iter().all(|(_, d)| d.step != Step::Kill),
            "{decisions:#?}"
        );
        // Asked for, that is an msrdc that is not reconnecting, and it is
        // ended once.
        let decisions = replay_with(
            Watchdog::new(PID).killing_on_link_down(),
            REAL,
            &ASLEEP[..1],
        );
        let steps = steps(&decisions);
        assert_eq!(steps[0].1, Step::Kill, "{decisions:#?}");
        assert!(decisions[0].1.why.contains("down for"), "{decisions:#?}");
        assert!(
            ("15:50:10"..="15:50:20").contains(&steps[0].0),
            "{decisions:#?}"
        );
        // With the sleep known, no kill (the full-day test above).
    }

    #[test]
    fn a_window_never_shown_here_is_left_alone() {
        // A test window on another pid, or this app before its first frame
        // reached Windows: not armed, so nothing, whatever the link does.
        assert!(replay(REAL, 4_000_000, &ASLEEP).is_empty());
        assert!(replay(REAL, 4_000_000, &[]).is_empty());
    }

    #[test]
    fn kills_are_rate_limited_and_stop_when_they_do_not_help() {
        let shown = "Client: ClientGetAppidReq: pid:825 appId:giverny WindowId:0x2\n";
        let up = "Client HandShake buildNumber:26200\n";
        let mut dog = Watchdog::new(PID);
        let mut now = 1000.0;
        let mut tick = |dog: &mut Watchdog, text: &str| {
            now += 5.0;
            dog.poll(now, text).map(|d| d.step)
        };
        tick(&mut dog, shown);
        // Reconnect, never shown: remap, then kill.
        let mut seen = Vec::new();
        seen.extend(tick(&mut dog, up));
        for _ in 0..10 {
            seen.extend(tick(&mut dog, ""));
        }
        assert_eq!(seen, [Step::Remap, Step::Kill]);
        // msrdc came back and still did not show it: remap, then the kill is
        // held back, a minute after the last one.
        let mut seen = Vec::new();
        seen.extend(tick(&mut dog, up));
        for _ in 0..10 {
            seen.extend(tick(&mut dog, ""));
        }
        assert_eq!(seen, [Step::Remap, Step::HeldBack]);
        // Ten minutes on, the second kill is allowed; a third is not, though
        // the clock would allow it, until the window is shown again.
        for round in 0..2 {
            for _ in 0..120 {
                tick(&mut dog, "");
            }
            let mut seen = Vec::new();
            seen.extend(tick(&mut dog, up));
            for _ in 0..10 {
                seen.extend(tick(&mut dog, ""));
            }
            let want = if round == 0 {
                Step::Kill
            } else {
                Step::HeldBack
            };
            assert_eq!(seen, [Step::Remap, want], "round {round}");
        }
        // Shown again: the count starts over.
        tick(&mut dog, shown);
        for _ in 0..120 {
            tick(&mut dog, "");
        }
        let mut seen = Vec::new();
        seen.extend(tick(&mut dog, up));
        for _ in 0..10 {
            seen.extend(tick(&mut dog, ""));
        }
        assert_eq!(seen, [Step::Remap, Step::Kill]);
    }

    #[test]
    fn a_window_shown_late_by_itself_is_only_remapped() {
        let mut dog = Watchdog::new(PID);
        let shown = "Client: ClientGetAppidReq: pid:825 appId:giverny WindowId:0x2\n";
        assert_eq!(dog.poll(0.0, shown), None);
        assert_eq!(dog.poll(5.0, "Client HandShake buildNumber:26200\n"), None);
        assert_eq!(dog.poll(10.0, ""), None);
        assert_eq!(dog.poll(15.0, "").map(|d| d.step), Some(Step::Remap));
        assert_eq!(dog.poll(20.0, shown), None);
        for t in 5..40 {
            assert_eq!(dog.poll(f64::from(t) * 5.0, ""), None);
        }
    }

    #[test]
    fn the_tail_reads_whole_lines_and_notices_a_new_log() {
        let dir = std::env::temp_dir().join(format!("giverny-wslg-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("weston.log");
        std::fs::write(&path, "one\ntw").unwrap();
        let mut tail = Tail::new(path.clone());
        assert_eq!(tail.read().unwrap(), ("one\n".to_string(), false));
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        assert_eq!(tail.read().unwrap(), ("two\nthree\n".to_string(), false));
        assert_eq!(tail.read().unwrap(), (String::new(), false));
        std::fs::write(&path, "new\n").unwrap();
        assert_eq!(tail.read().unwrap(), ("new\n".to_string(), true));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_vm_id_comes_from_the_environment_dump() {
        let log = "[11:05:04.191]   WSL2_VM_ID=F512C2A2-A53A-4208-92B5-764F9FC6BF8E\n";
        assert_eq!(
            parse_vm_id(log).as_deref(),
            Some("F512C2A2-A53A-4208-92B5-764F9FC6BF8E")
        );
        assert_eq!(parse_vm_id("[11:05:04.191]   WSL2_VM_ID=; rm -rf\n"), None);
    }

    #[test]
    fn the_record_keeps_the_newest_lines() {
        let dir = std::env::temp_dir().join(format!("giverny-wslg-rec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = record_path(&dir);
        for i in 0..2000 {
            remember(&path, &format!("line {i} {}", "x".repeat(40)));
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.len() <= RECORD_MAX);
        assert!(text.lines().last().unwrap().contains("line 1999"));
        assert!(
            text.lines().next().unwrap().len() > 20,
            "cut on a line boundary"
        );
        assert_eq!(recent(&dir, 2).len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

//! A Running row's open, started on the hover.
//!
//! Opening a Running agents-pane row waits first on Claude Code's agent
//! strip: the relay draws it only when asked, and Claude Code runs the
//! relay 300 ms after a width change ([`agent_open::Nudge`]), so the click
//! pays that trip before the first key can go. When the pointer rests on a
//! Running row, [`Prearm`] makes the same ask and the same nudge early, so
//! by the click the strip is usually there and the walk starts stepping at
//! once.
//!
//! Nothing new is ever drawn. The strip is asked for only while the tab's
//! picture is held, exactly as a walk holds it ([`agent_open::Settle`]),
//! and the pty alone is narrowed, never the grid. A held picture must
//! never be a stale one, so a pre-arm starts only on a screen that has
//! held still ([`Watch`]), and lets go the moment the part of the screen
//! that matters moves under it ([`screen_key`]): new output, a spinner, a
//! keystroke in the prompt. A hover that ends without a click withdraws the
//! ask, brings the relay's run forward again to hide the strip, and lets
//! the picture go only once the screen is back to the one it held.

use std::time::{Duration, Instant};

use crate::agent_open::{self, Nudge, Width};

/// How long the pointer rests on a Running row before its open is started.
pub const DWELL: Duration = Duration::from_millis(100);
/// How long the tab's screen must have held still before a pre-arm may
/// hold its picture: a screen that is changing would be seen to freeze.
/// Past a second, so a once-a-second clock (Claude Code's `(12s · …)`
/// while it works) never arms between two of its ticks.
pub const STILL: Duration = Duration::from_millis(1200);
/// The longest a hover holds the picture waiting for a click. A longer
/// rest lets go (and does not arm again until the pointer leaves the pane).
pub const HOLD_MAX: Duration = Duration::from_millis(2000);
/// How long the screen may differ from the held one before the pre-arm
/// lets go: Claude Code writes a frame in more than one read.
const STALE_AFTER: Duration = Duration::from_millis(150);
/// After a width change, how long before the screen is compared again:
/// Claude Code redraws at the new width, and at the old one after.
const WIDTH_SETTLE: Duration = Duration::from_millis(120);
/// After the last width change, how long a let-go waits before trusting
/// the screen: Claude Code's run 300 ms after it may still be coming, and
/// may still draw the strip from an ask read before it was withdrawn.
const QUIET: Duration = Duration::from_millis(420);
/// The longest a let-go holds the picture.
const LEAVE_MAX: Duration = Duration::from_millis(1500);
/// How many rows up from the prompt box's bottom rule [`screen_key`] reads.
const KEY_ROWS: usize = 12;

/// The part of a Claude Code screen a hover must not hold stale: the prompt
/// box and the rows above it, up to its bottom rule. What is under the rule
/// (the status line, the footer, the strip) is left out, since the strip
/// the pre-arm asks for is drawn there. Rules are read by their label, not
/// their length, and trailing blanks go. `None` with no prompt box on
/// screen: nothing to pre-arm.
pub fn screen_key(screen: &str) -> Option<String> {
    let prompt = agent_open::prompt_box(screen, screen)?;
    let rows: Vec<&str> = screen.lines().collect();
    let end = prompt.rows.end.min(rows.len().checked_sub(1)?);
    let top = end.saturating_sub(KEY_ROWS);
    let key: Vec<String> = rows[top..=end]
        .iter()
        .map(|r| {
            let r = r.trim_end();
            if agent_open::is_rule(r) {
                format!("─{}", r.trim().trim_matches('─').trim())
            } else {
                r.to_string()
            }
        })
        .collect();
    Some(key.join("\n"))
}

/// Since when a tab's [`screen_key`] has been what it is. Read every
/// frame the tab is on show: a screen only changes by output, and output
/// always brings a frame, so an idle screen needs no frames to be timed.
#[derive(Debug, Clone)]
pub struct Still {
    key: Option<String>,
    since: Instant,
}

impl Still {
    pub fn new(now: Instant, key: Option<String>) -> Still {
        Still { key, since: now }
    }

    /// Since when the screen has held still, without a new look.
    pub fn look_since(&self) -> Option<Instant> {
        self.key.as_ref().map(|_| self.since)
    }

    /// One frame's key. Returns since when the screen has held still,
    /// `None` while it has no prompt box to pre-arm on.
    pub fn look(&mut self, now: Instant, key: Option<String>) -> Option<Instant> {
        if key != self.key {
            self.key = key;
            self.since = now;
        }
        self.key.as_ref().map(|_| self.since)
    }
}

/// The pointer over a tab's agents pane: whether a Running row has been
/// rested on long enough, on a screen still long enough, to pre-arm.
/// Arms once per visit: moving between rows, or back onto one, while the
/// pointer stays on the pane does not arm again.
#[derive(Debug, Clone, Default)]
pub struct Watch {
    row: Option<(String, Instant)>,
    spent: bool,
}

impl Watch {
    /// One frame: `still` is since when the tab's screen has held still
    /// ([`Still::look`]), `row` the armable row under the pointer (its
    /// agent id), if any. True when a pre-arm should start now.
    pub fn ready(&mut self, now: Instant, still: Option<Instant>, row: Option<&str>) -> bool {
        match (&self.row, row) {
            (Some((id, _)), Some(r)) if id == r => {}
            (_, Some(r)) => self.row = Some((r.to_string(), now)),
            (_, None) => self.row = None,
        }
        match (&self.row, still) {
            (Some((_, since)), Some(still)) => {
                !self.spent && now >= *since + DWELL && now >= still + STILL
            }
            _ => false,
        }
    }

    /// A pre-arm started on this visit: no other until the pointer leaves.
    pub fn spend(&mut self) {
        self.spent = true;
    }

    /// Whether this visit has armed already.
    pub fn spent(&self) -> bool {
        self.spent
    }

    /// How long until the next frame should look, while a pre-arm is still
    /// to come: the dwell's end, or the screen's stillness's.
    pub fn wake(&self, now: Instant, still: Option<Instant>) -> Option<Duration> {
        if self.spent {
            return None;
        }
        let (_, since) = self.row.as_ref()?;
        let at = (*since + DWELL).max(still? + STILL);
        Some(at.saturating_duration_since(now))
    }
}

/// Why a pre-arm let go, for the timing log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LetGo {
    /// The pointer left, or something else took the tab.
    Left,
    /// No click within [`HOLD_MAX`].
    TimedOut,
    /// The screen moved under the held picture.
    Changed,
}

/// A hover's early strip ask on one tab, and the hold that hides it.
#[derive(Debug, Clone)]
pub struct Prearm {
    nudge: Nudge,
    armed: Instant,
    /// The [`screen_key`] the picture was held on.
    base: String,
    /// Whether the strip's `main` row was on screen when armed: on a
    /// worker's view it stays when the relay hides the agent rows.
    base_main: bool,
    changed: Option<Instant>,
    last_width: Option<Instant>,
    /// Letting go, since when, and why.
    leaving: Option<(Instant, LetGo)>,
    good_since: Option<Instant>,
    /// The strip was on screen while letting go: once it is gone, the run
    /// that hid it has been, and no other is waited out.
    hid: bool,
    /// How long after arming the strip's rows were first on screen.
    pub drawn: Option<Duration>,
}

impl Prearm {
    /// Start on `screen`, or `None` when it is not one to pre-arm on: no
    /// prompt box, the focus not in it (a dialog, the strip), or the
    /// strip's agent rows already drawn.
    pub fn new(now: Instant, screen: &str) -> Option<Prearm> {
        let base = screen_key(screen)?;
        if !matches!(
            agent_open::read_view(screen, screen),
            agent_open::View::Prompt { .. }
        ) || !agent_open::strip_agents(screen).is_empty()
        {
            return None;
        }
        // Narrow until the strip is drawn: Claude Code's timer runs from
        // the first frame on, as a click's walk does.
        let mut nudge = Nudge::default();
        nudge.ask_held();
        Some(Prearm {
            nudge,
            armed: now,
            base,
            base_main: agent_open::strip_shown(screen),
            changed: None,
            last_width: None,
            leaving: None,
            good_since: None,
            hid: false,
            drawn: None,
        })
    }

    /// Whether the relay is asked to draw the strip.
    pub fn wants_strip(&self) -> bool {
        self.leaving.is_none()
    }

    /// Why it is letting go, once it is.
    pub fn letting_go(&self) -> Option<LetGo> {
        self.leaving.map(|(_, why)| why)
    }

    /// The pty is narrow right now and must be put back.
    pub fn is_narrow(&self) -> bool {
        self.nudge.is_narrow()
    }

    /// Let go: the ask is withdrawn (by the caller, from
    /// [`Prearm::wants_strip`]) and the relay's run brought forward to
    /// hide the strip again.
    pub fn leave(&mut self, now: Instant, why: LetGo) {
        if self.leaving.is_none() {
            self.leaving = Some((now, why));
            self.good_since = None;
            self.nudge.ask(now);
        }
    }

    /// Whether `screen` is back to the one held: the strip as it was.
    fn restored(&self, screen: &str) -> bool {
        agent_open::strip_agents(screen).is_empty()
            && agent_open::strip_shown(screen) == self.base_main
    }

    /// One frame: the width change to make, if any, and whether it is over
    /// (let go, and the screen back to the one held, so the picture may go;
    /// a pty still narrow then is put back by the caller).
    pub fn tick(&mut self, now: Instant, screen: &str) -> (Option<Width>, bool) {
        if self.leaving.is_none() {
            if now >= self.armed + HOLD_MAX {
                self.leave(now, LetGo::TimedOut);
            } else if !self.nudge.is_narrow()
                && self.last_width.is_none_or(|w| now >= w + WIDTH_SETTLE)
            {
                if screen_key(screen).as_deref() == Some(self.base.as_str()) {
                    self.changed = None;
                } else {
                    let since = *self.changed.get_or_insert(now);
                    if now >= since + STALE_AFTER {
                        self.leave(now, LetGo::Changed);
                    }
                }
            }
        }
        let shown = !agent_open::strip_agents(screen).is_empty();
        if self.leaving.is_none() && shown && self.drawn.is_none() {
            self.drawn = Some(now.duration_since(self.armed));
        }
        let restored = self.restored(screen);
        let waiting = match self.leaving {
            None => !shown,
            Some(_) => !restored,
        };
        let width = self.nudge.tick(now, waiting);
        if width.is_some() {
            self.last_width = Some(now);
        }
        let Some((since, _)) = self.leaving else {
            return (width, false);
        };
        if now >= since + LEAVE_MAX {
            return (width, true);
        }
        if !restored {
            self.hid = true;
        }
        let quiet = self.hid || self.last_width.is_none_or(|w| now >= w + QUIET);
        if self.nudge.is_narrow() || !quiet || !restored {
            self.good_since = None;
            return (width, false);
        }
        let good = *self.good_since.get_or_insert(now);
        (width, now >= good + agent_open::SETTLE_CALM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: &str = "──────────────────────────────";

    /// Claude Code's main view with the strip hidden, and `draft` typed.
    fn main_screen(draft: &str) -> String {
        format!(
            "● Two workers are running.\n✻ Crunched for 18s\n{RULE}\n❯ {draft}\n{RULE}\n  \
             Haiku 4.5  ·  5h 11%\n  ⏸ manual mode on · 2 shells\n"
        )
    }

    /// The same with the strip's rows drawn under it, the pty `narrow`
    /// or not (the rules one column short).
    fn with_strip(draft: &str, narrow: bool) -> String {
        let rule = if narrow { &RULE[3..] } else { RULE };
        format!(
            "● Two workers are running.\n✻ Crunched for 18s\n{rule}\n❯ {draft}\n{rule}\n  \
             Haiku 4.5  ·  5h 11%\n  ⏸ manual mode on · 2 shells · ← 2 agents\n  \
             ● main\n  ◯ [a1] eta worker\n  ◯ [b2] theta worker\n"
        )
    }

    #[test]
    fn the_key_is_the_prompt_and_above_whatever_the_strip_and_width() {
        let bare = screen_key(&main_screen("")).unwrap();
        assert_eq!(screen_key(&with_strip("", false)).unwrap(), bare);
        assert_eq!(screen_key(&with_strip("", true)).unwrap(), bare);
        assert_ne!(screen_key(&main_screen("h")).unwrap(), bare, "a keystroke");
        let more = format!("● new output\n{}", main_screen(""));
        // New output above moves every row up by one within the window.
        let out = main_screen("").replace("✻ Crunched for 18s", "✻ Crunched for 19s");
        assert_ne!(screen_key(&out).unwrap(), bare, "a spinner");
        assert!(screen_key(&more).is_some());
        assert_eq!(screen_key("no prompt here\n"), None);
    }

    #[test]
    fn a_watch_waits_for_the_dwell_and_a_still_screen_and_arms_once() {
        let t = Instant::now();
        let key = || Some("k".to_string());
        let mut still = Still::new(t, key());
        let quiet = still.look(t + STILL, key());
        assert_eq!(quiet, Some(t));
        let mut w = Watch::default();
        let on = t + STILL;
        assert!(!w.ready(on, quiet, None), "on the pane, not on a row");
        assert!(!w.ready(on, quiet, Some("a1")), "just arrived");
        assert_eq!(w.wake(on, quiet), Some(DWELL));
        assert!(w.ready(on + DWELL, quiet, Some("a1")));
        w.spend();
        assert!(!w.ready(on + DWELL * 3, quiet, Some("b2")), "once a visit");
        assert_eq!(w.wake(on, quiet), None);

        // A screen that changes more often than STILL never arms.
        let mut still = Still::new(t, key());
        let mut w = Watch::default();
        for i in 0..40u32 {
            let at = t + STILL / 2 * i;
            let since = still.look(at, Some(format!("k{i}")));
            assert!(!w.ready(at, since, Some("a1")));
        }
        // Moving between rows starts the dwell over before it arms.
        let mut w = Watch::default();
        let quiet = Some(t);
        assert!(!w.ready(on, quiet, Some("a1")));
        assert!(!w.ready(on + DWELL / 2, quiet, Some("b2")));
        assert!(!w.ready(on + DWELL, quiet, Some("b2")));
        assert!(w.ready(on + DWELL * 3 / 2, quiet, Some("b2")));
        // No prompt on screen: nothing to arm.
        let mut still = Still::new(t, None);
        let mut w = Watch::default();
        let since = still.look(on, None);
        assert_eq!(since, None);
        assert!(!w.ready(on + DWELL * 4, since, Some("a1")));
    }

    #[test]
    fn a_prearm_narrows_until_the_strip_then_holds_for_the_click() {
        let t = Instant::now();
        let mut p = Prearm::new(t, &main_screen("")).expect("armable");
        assert!(p.wants_strip());
        assert_eq!(p.tick(t, &main_screen("")), (Some(Width::Narrow), false));
        // Narrow, the prompt redrawn one column short: nothing is compared.
        let ms = Duration::from_millis;
        assert_eq!(p.tick(t + ms(200), &main_screen("")), (None, false));
        // The relay's answer: the strip, still narrow. The pty comes back.
        let at = t + ms(340);
        assert_eq!(
            p.tick(at, &with_strip("", true)),
            (Some(Width::Restore), false)
        );
        assert_eq!(p.drawn, Some(ms(340)));
        // Full width again, strip still up (the ask stands): held, waiting.
        for i in 1..10 {
            let now = at + ms(50 * i);
            assert_eq!(p.tick(now, &with_strip("", false)), (None, false));
        }
        assert!(p.wants_strip() && p.letting_go().is_none());
    }

    #[test]
    fn a_hover_that_never_clicks_lets_go_only_once_the_strip_is_gone() {
        let t = Instant::now();
        let ms = Duration::from_millis;
        let mut p = Prearm::new(t, &main_screen("")).unwrap();
        p.tick(t, &main_screen(""));
        p.tick(t + ms(340), &with_strip("", true));
        let left = t + ms(600);
        p.leave(left, LetGo::Left);
        assert!(!p.wants_strip());
        // The strip still drawn: nudged to bring the hiding run forward.
        let mut now = left;
        let mut narrowed = false;
        let mut done = false;
        while now < left + ms(700) {
            let (w, d) = p.tick(now, &with_strip("", false));
            narrowed |= w == Some(Width::Narrow);
            done |= d;
            now += ms(16);
        }
        assert!(narrowed && !done, "held while the strip is up");
        // The relay hid it: over once the screen has held.
        let gone = now;
        let mut over = None;
        while now < gone + ms(800) {
            if p.tick(now, &main_screen("")).1 {
                over = Some(now);
                break;
            }
            now += ms(16);
        }
        let over = over.expect("lets go");
        assert!(!p.is_narrow());
        assert!(over < gone + ms(600), "{:?}", over - gone);
    }

    #[test]
    fn a_hover_left_before_the_answer_waits_out_the_run_already_coming() {
        let t = Instant::now();
        let ms = Duration::from_millis;
        let mut p = Prearm::new(t, &main_screen("")).unwrap();
        assert_eq!(p.tick(t, &main_screen("")).0, Some(Width::Narrow));
        p.leave(t + ms(150), LetGo::Left);
        // The narrow pty comes back, and the screen is the held one, but
        // Claude Code's run is still to come: not over yet.
        let (w, done) = p.tick(t + ms(150), &main_screen(""));
        assert_eq!(w, Some(Width::Restore));
        assert!(!done);
        assert!(!p.tick(t + ms(400), &main_screen("")).1);
        // That run drew the strip from the ask it read: held on.
        assert!(!p.tick(t + ms(460), &with_strip("", false)).1);
        // And hid again: over.
        let mut now = t + ms(1000);
        let mut over = false;
        while now < t + ms(1400) && !over {
            over = p.tick(now, &main_screen("")).1;
            now += ms(16);
        }
        assert!(over);
    }

    #[test]
    fn a_screen_that_moves_under_the_hold_lets_go_at_once() {
        let t = Instant::now();
        let ms = Duration::from_millis;
        let mut p = Prearm::new(t, &main_screen("")).unwrap();
        p.tick(t, &main_screen(""));
        p.tick(t + ms(340), &with_strip("", true));
        // A keystroke in the prompt, full width: a blip is not enough...
        let at = t + ms(500);
        p.tick(at, &with_strip("h", false));
        assert!(p.letting_go().is_none());
        p.tick(at + ms(20), &with_strip("", false));
        assert!(p.letting_go().is_none(), "back as it was");
        // ...but a change that stays is.
        p.tick(at + ms(40), &with_strip("h", false));
        p.tick(at + ms(40) + STALE_AFTER, &with_strip("h", false));
        assert_eq!(p.letting_go(), Some(LetGo::Changed));
        assert!(!p.wants_strip());
    }

    #[test]
    fn a_rest_past_hold_max_lets_go() {
        let t = Instant::now();
        let mut p = Prearm::new(t, &main_screen("")).unwrap();
        p.tick(t, &main_screen(""));
        p.tick(t + Duration::from_millis(340), &with_strip("", true));
        p.tick(t + HOLD_MAX, &with_strip("", false));
        assert_eq!(p.letting_go(), Some(LetGo::TimedOut));
    }

    #[test]
    fn nothing_to_prearm_on_a_dialog_or_a_drawn_strip() {
        let t = Instant::now();
        assert!(Prearm::new(t, "some dialog\n  Esc to close\n").is_none());
        assert!(Prearm::new(t, &with_strip("", false)).is_none());
    }
}

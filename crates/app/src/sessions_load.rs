//! The app's one sampler: what Giverny runs, read every [`EVERY`] (a
//! second) from a thread of its own
//! ([`giverny_claude::use_reading::Sampler`]: one pass over `/proc`, and
//! `nvidia-smi` now and then), so no frame waits on it.
//!
//! Every figure shown comes from the same pass, measured the same way: the
//! sidebar's line is its total (the app and everything under it), the
//! management panel's rows are its runs, and each tab's Claude Code status line
//! reads its session from the snapshot the pass leaves beside the socket
//! ([`giverny_claude::use_reading::snapshot_path`]). So a row never shows
//! more than its session, nor a session more than the total.
//!
//! The status line leads (giverny#235): Claude Code runs it on a timer of
//! its own, so it says which reading it showed, and the tab's pane and the
//! sidebar draw that one ([`for_tab`]) from the frame its figures are on
//! the screen. The last few passes are kept ([`KEEP`]) to look it up.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use giverny_claude::run_live::{RunLive, TaskLive};
use giverny_claude::use_reading::{self, Reading, Sampler, Use};

use crate::chrome::Chrome;

/// How often the pass is taken.
const EVERY: Duration = Duration::from_secs(1);

/// How many passes are kept, newest last: more than a status line that is
/// still followed can be behind ([`use_reading::QUIET_MS`]).
pub const KEEP: usize = 8;

type Recent = VecDeque<Arc<Reading>>;

static LAST: OnceLock<Arc<Mutex<Recent>>> = OnceLock::new();

/// Start the sampler (once; later calls do nothing).
pub fn start(ctx: &egui::Context) {
    LAST.get_or_init(|| {
        let last = Arc::new(Mutex::new(VecDeque::with_capacity(KEEP)));
        let (l, ctx) = (last.clone(), ctx.clone());
        if let Err(err) = std::thread::Builder::new()
            .name("use-sampler".into())
            .spawn(move || read_loop(&l, &ctx))
        {
            tracing::warn!("use sampler: did not start: {err}");
        }
        last
    });
}

/// The last pass, starting the sampler on the first ask. `None` until its
/// first pass, and where there is nothing to read (no `/proc`).
pub fn latest(ctx: &egui::Context) -> Option<Arc<Reading>> {
    start(ctx);
    LAST.get()?
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .back()
        .cloned()
}

/// The pass numbered `seq` among `recent`, or the newest where it is not
/// one of them (`None`, or one already let go).
pub fn pick(recent: &Recent, seq: Option<u64>) -> Option<Arc<Reading>> {
    seq.and_then(|n| recent.iter().rev().find(|r| r.seq == n))
        .or_else(|| recent.back())
        .cloned()
}

/// Which pass each tab's figures are drawn from ([`for_tab`]).
#[derive(Debug, Default)]
pub struct Follow {
    adopted: HashMap<String, u64>,
}

/// The reading to draw the tab `tab` (its `$GIVERNY_TAB_ID`) from: the one
/// its Claude Code status line shows, from the frame `screen` (the tab's
/// screen text) has the line's figures on it; the newest where the tab has
/// no such line, or it has gone quiet ([`use_reading::follow`]). Asked once
/// a frame, before anything is drawn, so all of the frame agrees.
pub fn for_tab(
    ctx: &egui::Context,
    follow: &mut Follow,
    tab: &str,
    screen: impl FnOnce() -> Option<String>,
) -> Option<Arc<Reading>> {
    start(ctx);
    let shown = use_reading::read_shown(&use_reading::shown_dir(), tab);
    let now_ms = jiff::Timestamp::now().as_millisecond().max(0) as u64;
    let seq = use_reading::follow(
        follow.adopted.get(tab).copied(),
        shown.as_ref(),
        now_ms,
        |text| {
            let text = text.trim();
            !text.is_empty() && screen().is_some_and(|s| s.contains(text))
        },
    );
    match seq {
        Some(n) => follow.adopted.insert(tab.to_string(), n),
        None => follow.adopted.remove(tab),
    };
    pick(&LAST.get()?.lock().unwrap_or_else(|p| p.into_inner()), seq)
}

/// A reading's runs, as the management panel keys them.
pub fn task_lives(r: &Reading) -> Vec<TaskLive> {
    r.runs
        .iter()
        .map(|run| TaskLive {
            session: run.session.clone(),
            task: run.task.clone(),
            live: RunLive {
                cpu_pct: run.used.cpu_pct,
                mem_mb: run.used.mem_mb,
                gpu_mb: run.used.gpu_mb,
            },
            agent: run.agent.clone(),
        })
        .collect()
}

/// A reading's workers' use, by agent id, as the management panel shows it.
pub fn workers(r: &Reading) -> HashMap<String, RunLive> {
    r.agents
        .iter()
        .map(|(id, u)| {
            (
                id.clone(),
                RunLive {
                    cpu_pct: u.cpu_pct,
                    mem_mb: u.mem_mb,
                    gpu_mb: u.gpu_mb,
                },
            )
        })
        .collect()
}

fn read_loop(last: &Mutex<Recent>, ctx: &egui::Context) {
    // Each pass also caps every tab's claude (giverny#262).
    let mut sampler = Sampler::capping_tabs();
    let app = std::process::id();
    let runs_dir = giverny_claude::run_live::runs_dir(&giverny_claude::resources::ledger_path(
        &giverny_claude::feed::feed_dir(),
    ));
    let snapshot = giverny_claude::use_reading::snapshot_path();
    // A last run's lines numbered its passes, not this one's.
    let _ = std::fs::remove_dir_all(use_reading::shown_dir());
    // On a steady beat, whatever a pass costs.
    let mut next = std::time::Instant::now();
    loop {
        if let Some(r) = sampler.sample(app, &runs_dir) {
            let r = Arc::new(r);
            // Kept first, then published: a status line never shows a
            // reading its tab cannot look up.
            let mut recent = last.lock().unwrap_or_else(|p| p.into_inner());
            if recent.len() == KEEP {
                recent.pop_front();
            }
            recent.push_back(r.clone());
            drop(recent);
            if let Err(err) = giverny_claude::use_reading::write_snapshot(&snapshot, &r) {
                tracing::debug!("use sampler: {}: {err}", snapshot.display());
            }
            ctx.request_repaint();
        }
        next += EVERY;
        let now = std::time::Instant::now();
        if next < now {
            next = now;
        }
        std::thread::sleep(next - now);
    }
}

/// The line's figures: `23% CPU  2.0G RAM`, then `  40% GPU` with a GPU
/// that reports per-process utilisation, `  1.2G GPU` (its memory) with
/// one that does not, nothing with none. In the pieces the rail colours one
/// by one (giverny#295), each with its share of the whole machine as
/// [`machine_shares`] takes it of `ram_mb` (GPU compute as it is); the gaps,
/// and GPU memory, whose total is not known here, at none.
///
/// Unpadded: the status line pads its figures to fixed widths to hold
/// them in their columns, which a line read from the left has no use for.
pub fn figure_parts(t: &Use, ram_mb: u64) -> Vec<(String, f64)> {
    use giverny_claude::session_use::gb;
    let (cpu, mem) = machine_shares(t, ram_mb);
    let mut parts = vec![
        (format!("{}% CPU", t.cpu_pct.min(100)), cpu),
        ("  ".to_string(), 0.0),
        (format!("{} RAM", gb(t.mem_mb)), mem),
    ];
    match (t.gpu_pct, t.gpu_mb) {
        (Some(p), _) => {
            parts.extend([("  ".to_string(), 0.0), (format!("{p}% GPU"), f64::from(p))])
        }
        (None, Some(g)) => parts.extend([("  ".to_string(), 0.0), (format!("{} GPU", gb(g)), 0.0)]),
        (None, None) => {}
    }
    parts
}

/// The use of the whole machine a tab's (or a group's) figures are
/// coloured by (giverny#289): `(CPU, memory)`, percent. Its CPU figure is
/// already a share of every core; its memory is taken of all the RAM
/// (`ram_mb`), and with no RAM known it is never high.
pub fn machine_shares(u: &Use, ram_mb: u64) -> (f64, f64) {
    let mem = if ram_mb > 0 {
        u.mem_mb as f64 * 100.0 / ram_mb as f64
    } else {
        0.0
    };
    (f64::from(u.cpu_pct), mem)
}

/// How heavy a figure is on the whole machine, as every use figure is
/// coloured — a tab's and the total in the rail (giverny#289, #295), a
/// Running row's in the management panel (giverny#305): light under 20 %,
/// busy from 20 % to 50 %, heavy over it. The share is a
/// [`machine_shares`] figure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Load {
    Light,
    Busy,
    Heavy,
}

impl Load {
    pub fn of(share: f64) -> Self {
        if share > 50.0 {
            Load::Heavy
        } else if share >= 20.0 {
            Load::Busy
        } else {
            Load::Light
        }
    }

    /// The figure's colour: its own (`light`, dim in the rail, the row's
    /// in the panel) while light, amber when busy, poppy when heavy.
    pub fn color(self, light: egui::Color32, c: Chrome) -> egui::Color32 {
        match self {
            Load::Light => light,
            Load::Busy => c.amber,
            Load::Heavy => c.poppy,
        }
    }
}

/// This machine's total RAM, MiB, read once.
pub fn machine_ram_mb() -> u64 {
    use giverny_core::limits::Machine;
    static RAM: OnceLock<u64> = OnceLock::new();
    *RAM.get_or_init(|| Machine::detect_cpu_ram().ram.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use giverny_claude::use_reading::RunUse;

    /// The line as read.
    fn figures(t: &Use) -> String {
        figure_parts(t, 0).into_iter().map(|(s, _)| s).collect()
    }

    #[test]
    fn the_line_is_cpu_ram_and_gpu_when_there_is_one() {
        let mut u = Use {
            cpu_pct: 23,
            mem_mb: 2048,
            gpu_mb: None,
            gpu_pct: None,
        };
        assert_eq!(figures(&u), "23% CPU  2.0G RAM", "no GPU: nothing of it");
        u.gpu_mb = Some(1229);
        assert_eq!(
            figures(&u),
            "23% CPU  2.0G RAM  1.2G GPU",
            "no utilisation: memory"
        );
        u.gpu_pct = Some(40);
        assert_eq!(figures(&u), "23% CPU  2.0G RAM  40% GPU");
    }

    /// The pieces the rail colours carry their own shares: CPU as it is,
    /// RAM of all of it, GPU compute as it is; the gaps none.
    #[test]
    fn each_piece_of_the_line_carries_its_share() {
        let u = Use {
            cpu_pct: 23,
            mem_mb: 12_000,
            gpu_mb: Some(512),
            gpu_pct: Some(60),
        };
        assert_eq!(
            figure_parts(&u, 24_000),
            vec![
                ("23% CPU".to_string(), 23.0),
                ("  ".to_string(), 0.0),
                ("12G RAM".to_string(), 50.0),
                ("  ".to_string(), 0.0),
                ("60% GPU".to_string(), 60.0),
            ]
        );
    }

    /// Both shares are of the whole machine: the CPU figure as it is (a
    /// share of every core), the memory of all the RAM.
    #[test]
    fn a_tabs_share_is_of_the_whole_machine() {
        let u = Use {
            cpu_pct: 25,
            mem_mb: 2048,
            gpu_mb: None,
            gpu_pct: None,
        };
        assert_eq!(machine_shares(&u, 8192), (25.0, 25.0));
        assert_eq!(machine_shares(&u, 0), (25.0, 0.0), "no RAM known");
        assert!(machine_ram_mb() > 0, "this machine's RAM");
    }

    #[test]
    fn a_pass_is_found_by_its_number_or_the_newest_is() {
        let recent: Recent = (1..=KEEP as u64)
            .map(|seq| {
                Arc::new(Reading {
                    seq,
                    ..Reading::default()
                })
            })
            .collect();
        let seq = |r: Option<Arc<Reading>>| r.map(|r| r.seq);
        assert_eq!(seq(pick(&recent, Some(3))), Some(3));
        assert_eq!(seq(pick(&recent, None)), Some(KEEP as u64), "the newest");
        assert_eq!(seq(pick(&recent, Some(99))), Some(KEEP as u64), "let go");
        assert_eq!(seq(pick(&Recent::new(), Some(3))), None, "none yet");
    }

    #[test]
    fn a_runs_figure_reaches_the_pane_as_it_was_read() {
        let used = Use {
            cpu_pct: 39,
            mem_mb: 928,
            gpu_mb: Some(2048),
            gpu_pct: Some(12),
        };
        let r = Reading {
            runs: vec![RunUse {
                session: "s1".into(),
                task: "demo#1".into(),
                used,
                agent: Some("a1".into()),
            }],
            ..Reading::default()
        };
        let t = task_lives(&r);
        assert_eq!(
            (t[0].session.as_str(), t[0].task.as_str()),
            ("s1", "demo#1")
        );
        assert_eq!(t[0].agent.as_deref(), Some("a1"));
        assert_eq!(
            t[0].live,
            RunLive {
                cpu_pct: 39,
                mem_mb: 928,
                gpu_mb: Some(2048)
            }
        );
    }
}

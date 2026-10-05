//! The app's one sampler: what Giverny runs, read every [`EVERY`] (a
//! second) from a thread of its own
//! ([`giverny_claude::use_reading::Sampler`]: one pass over `/proc`, and
//! `nvidia-smi` now and then), so no frame waits on it.
//!
//! Every figure shown comes from the same pass, measured the same way: the
//! sidebar's line is its total (the app and everything under it), the
//! agents pane's rows are its runs, and each tab's Claude Code status line
//! reads its session from the snapshot the pass leaves beside the socket
//! ([`giverny_claude::use_reading::snapshot_path`]). So a row never shows
//! more than its session, nor a session more than the total.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use giverny_claude::run_live::{RunLive, TaskLive};
use giverny_claude::use_reading::{Reading, Sampler, Use};

/// How often the pass is taken.
const EVERY: Duration = Duration::from_secs(1);

static LAST: OnceLock<Arc<Mutex<Option<Arc<Reading>>>>> = OnceLock::new();

/// Start the sampler (once; later calls do nothing).
pub fn start(ctx: &egui::Context) {
    LAST.get_or_init(|| {
        let last = Arc::new(Mutex::new(None));
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
        .clone()
}

/// A reading's runs, as the agents pane keys them.
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

/// A reading's workers' use, by agent id, as the agents pane shows it.
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

fn read_loop(last: &Mutex<Option<Arc<Reading>>>, ctx: &egui::Context) {
    let mut sampler = Sampler::default();
    let app = std::process::id();
    let runs_dir = giverny_claude::run_live::runs_dir(&giverny_claude::resources::ledger_path(
        &giverny_claude::feed::feed_dir(),
    ));
    let snapshot = giverny_claude::use_reading::snapshot_path();
    // On a steady beat, whatever a pass costs.
    let mut next = std::time::Instant::now();
    loop {
        if let Some(r) = sampler.sample(app, &runs_dir) {
            // Published first, then shown: what the pane and the sidebar
            // draw is always the reading the status lines can read.
            if let Err(err) = giverny_claude::use_reading::write_snapshot(&snapshot, &r) {
                tracing::debug!("use sampler: {}: {err}", snapshot.display());
            }
            *last.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(r));
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
/// one that does not, nothing with none.
///
/// Unpadded: the status line pads its figures to fixed widths to hold
/// them in their columns, which a line read from the left has no use for.
pub fn figures(t: &Use) -> String {
    use giverny_claude::session_use::gb;
    let mut s = format!("{}% CPU  {} RAM", t.cpu_pct.min(100), gb(t.mem_mb));
    match (t.gpu_pct, t.gpu_mb) {
        (Some(p), _) => s.push_str(&format!("  {p}% GPU")),
        (None, Some(g)) => s.push_str(&format!("  {} GPU", gb(g))),
        (None, None) => {}
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use giverny_claude::use_reading::RunUse;

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

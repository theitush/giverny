//! Every Claude Code session's use right now, summed, for the sidebar's
//! line under the account bars.
//!
//! A thread of its own reads it ([`giverny_claude::session_use::AllSessions`]:
//! all of `/proc`, and `nvidia-smi` now and then) every [`EVERY`]; the
//! sidebar only takes the last reading, so a frame never waits on it. The
//! thread starts with the first ask and rests while nobody asks (the
//! sidebar hidden).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use giverny_claude::session_use::{AllSessions, AllUse};

/// How often the sessions are read.
const EVERY: Duration = Duration::from_secs(2);

/// Nobody asked for this long: the thread rests.
const IDLE_MS: u64 = 10_000;

#[derive(Default)]
struct Shared {
    last: Mutex<Option<AllUse>>,
    wanted_ms: AtomicU64,
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The last reading, starting the reader on the first ask. `None` until
/// its first reading, and where there is nothing to read (no `/proc`).
pub fn latest(ctx: &egui::Context) -> Option<AllUse> {
    let shared = SHARED.get_or_init(|| {
        let shared = Arc::new(Shared::default());
        let (s, ctx) = (shared.clone(), ctx.clone());
        if let Err(err) = std::thread::Builder::new()
            .name("sessions-load".into())
            .spawn(move || read_loop(&s, &ctx))
        {
            tracing::warn!("sessions load: the reader did not start: {err}");
        }
        shared
    });
    shared.wanted_ms.store(now_ms(), Ordering::Relaxed);
    *shared.last.lock().unwrap_or_else(|p| p.into_inner())
}

fn read_loop(shared: &Shared, ctx: &egui::Context) {
    let mut sampler = AllSessions::default();
    loop {
        if now_ms().saturating_sub(shared.wanted_ms.load(Ordering::Relaxed)) <= IDLE_MS {
            let now = sampler.sample();
            let mut last = shared.last.lock().unwrap_or_else(|p| p.into_inner());
            if *last != now {
                *last = now;
                drop(last);
                ctx.request_repaint();
            }
        } else {
            // Rested: the next reading's CPU would span the rest.
            sampler = AllSessions::default();
        }
        std::thread::sleep(EVERY);
    }
}

/// The line's figures: `23% CPU  2.0G RAM`, then `  40% GPU` with a GPU
/// that reports per-process utilisation, `  1.2G GPU` (its memory) with
/// one that does not, nothing with none.
///
/// Unpadded: the status line pads its figures to fixed widths to hold
/// them in their columns, which a line read from the left has no use for.
pub fn figures(u: &AllUse) -> String {
    use giverny_claude::session_use::gb;
    let t = &u.total;
    let mut s = format!("{}% CPU  {} RAM", t.cpu_pct.min(100), gb(t.mem_mb));
    match (u.gpu_pct, t.gpu_mb) {
        (Some(p), _) => s.push_str(&format!("  {p}% GPU")),
        (None, Some(g)) => s.push_str(&format!("  {} GPU", gb(g))),
        (None, None) => {}
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use giverny_claude::session_use::SessionUse;

    #[test]
    fn the_line_is_cpu_ram_and_gpu_when_there_is_one() {
        let mut u = AllUse {
            sessions: 3,
            total: SessionUse {
                cpu_pct: 23,
                mem_mb: 2048,
                gpu_mb: None,
            },
            gpu_pct: None,
        };
        assert_eq!(figures(&u), "23% CPU  2.0G RAM", "no GPU: nothing of it");
        u.total.gpu_mb = Some(1229);
        assert_eq!(
            figures(&u),
            "23% CPU  2.0G RAM  1.2G GPU",
            "no utilisation: memory"
        );
        u.gpu_pct = Some(40);
        assert_eq!(figures(&u), "23% CPU  2.0G RAM  40% GPU");
    }
}

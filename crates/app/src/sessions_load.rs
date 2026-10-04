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

/// The line's figures: `45% CPU  4.2G`, and `  gpu 1.2G` with a GPU.
pub fn figures(u: &AllUse) -> String {
    giverny_claude::session_use::segments(&u.total).join("  ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use giverny_claude::session_use::SessionUse;

    #[test]
    fn the_line_reads_like_the_status_line() {
        let mut u = AllUse {
            sessions: 3,
            total: SessionUse {
                cpu_pct: 45,
                mem_mb: 4300,
                gpu_mb: None,
            },
        };
        assert_eq!(figures(&u), "45% CPU  4.2G");
        u.total.gpu_mb = Some(1229);
        assert_eq!(figures(&u), "45% CPU  4.2G  gpu 1.2G");
    }
}

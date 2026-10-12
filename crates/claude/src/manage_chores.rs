//! A manager's chores, told to the manager rather than drawn for the person
//! (giverny#310, giverny#284).
//!
//! Three of the management panel's states are the dispatcher's to act on,
//! not the person's: a Running row whose worker has **finished** and that
//! nobody has landed; a Next up row that has sat with **nothing recorded to
//! wait on** (no hold, no queued lease, no task before it); and a Running row
//! with **no worker** at all, that its manager has not paused (giverny#334).
//! The panel draws each as an ordinary row (the last under Next up, `no
//! worker yet`); the manager session that owns them hears of each once, as
//! one line on its next tool call.
//!
//! **Who knows what.** Only the app sees Claude Code's live worker list (the
//! relayed `subagentStatusLine`), so only the app can tell a finished worker
//! from a busy one. It runs the panel's own row logic over every manager
//! tab ([`Chore`]s, `management_panel::chores`) and keeps what is due in
//! `<feed dir>/chores/<session>.json` ([`publish`]); the file is there only
//! while something is due. The plugin's `PostToolUse` hook ([`deliver`]),
//! on the dispatcher's own calls, `stat`s that file, and hands each due chore
//! to the session as `additionalContext`.
//!
//! **Once per row.** Delivering stamps the row in the manager session's file
//! (`land_nudged` / `start_nudged`, the chore's [`Chore::since`]), and the
//! app drops a stamped chore from what it keeps. A row that becomes a chore
//! again from a new instant — a worker messaged again that finishes again,
//! a hold that ends — is told again; the same one never is.
//!
//! **Thresholds.** A Next up row is the manager's after [`IDLE_PLANNED_MS`]
//! (the panel's idle mark since giverny#282). A finished worker's row after
//! [`LAND_GRACE_MS`]. A row with no worker [`feed::HANDOFF_WINDOW_MS`] after
//! it was handed out, when a spawn or a pick-up would have come.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::feed::{self, FeedRow, Stage};
use crate::manage::{self, Lock};

/// How long a Next up row may sit with nothing recorded to wait on before
/// its manager is told to start or hold it (giverny#282).
pub const IDLE_PLANNED_MS: u64 = 5 * 60 * 1000;

/// How long after its worker finished an unlanded Running row is its
/// manager's to be told about. The dispatcher hears of the end at once
/// (Claude Code's task notification reaches its next turn), and reading the
/// report and running `land` takes it a turn or two, a minute or so; three
/// minutes is past that without leaving a finished task sitting for long.
/// It is the span [`feed::HANDOFF_WINDOW_MS`] gives the other end of a
/// task, between `start` and the worker's spawn.
pub const LAND_GRACE_MS: u64 = 3 * 60 * 1000;

/// Where the chores live, under the feed directory.
pub const CHORES_DIR: &str = "chores";

/// What the manager is to do about a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Its worker finished: read the report and land it.
    Land,
    /// It waits on nothing recorded: start it, or hold it with a reason.
    Start,
    /// It is Running with no worker: spawn or hand it one, or pause or drop
    /// it.
    Worker,
}

impl Kind {
    /// The row field delivering it stamps.
    fn field(self) -> &'static str {
        match self {
            Kind::Land => "land_nudged",
            Kind::Start => "start_nudged",
            Kind::Worker => "worker_nudged",
        }
    }

    /// Whether a row of the manager session's file in `stage` is still
    /// this chore's: a Next up row still planned; a finished worker's row
    /// not landed — Running, or still planned in the file for a task the
    /// dispatcher handed over by message and never `start`ed; a row with
    /// no worker still Running.
    fn stands_in(self, stage: Option<Stage>) -> bool {
        match self {
            Kind::Land => matches!(stage, Some(Stage::Running | Stage::Planned)),
            Kind::Start => stage == Some(Stage::Planned),
            Kind::Worker => stage == Some(Stage::Running),
        }
    }

    fn threshold_ms(self) -> u64 {
        match self {
            Kind::Land => LAND_GRACE_MS,
            Kind::Start => IDLE_PLANNED_MS,
            Kind::Worker => feed::HANDOFF_WINDOW_MS,
        }
    }
}

/// One row the manager has to act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chore {
    /// The row's key in the manager session's file.
    pub key: String,
    pub kind: Kind,
    /// Since when, epoch ms: the worker's end ([`Kind::Land`]), the last
    /// instant the row could have started ([`Kind::Start`]: planned, unheld,
    /// the task before it landed, its lease granted), or when it was handed
    /// out ([`Kind::Worker`]: started, or handed to a worker that never
    /// picked it up).
    pub since: u64,
    /// How the worker ended (`finished`, `failed`, `killed`, `stopped`):
    /// [`Kind::Land`] only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub how: Option<String>,
}

impl Chore {
    /// Past its threshold at `now`.
    pub fn is_due(&self, now: u64) -> bool {
        now >= self.since.saturating_add(self.kind.threshold_ms())
    }

    /// The manager has been told of it already: `row` carries its stamp.
    pub fn told(&self, row: &FeedRow) -> bool {
        let stamp = match self.kind {
            Kind::Land => row.land_nudged_ms,
            Kind::Start => row.start_nudged_ms,
            Kind::Worker => row.worker_nudged_ms,
        };
        stamp == Some(self.since)
    }

    /// The line the manager reads, at `now`.
    pub fn line(&self, now: u64) -> String {
        let key = &self.key;
        let ago = feed::fmt_span((now.saturating_sub(self.since) / 1000) as i64);
        match self.kind {
            Kind::Land => {
                let how = match self.how.as_deref() {
                    None | Some("finished") => "finished",
                    Some("killed") => "was killed",
                    Some(h) => h,
                };
                format!(
                    "Giverny: `{key}`'s worker {how} {ago} ago: check its report and \
                     `giverny-manage land {key}`."
                )
            }
            Kind::Start => format!(
                "Giverny: `{key}` has waited {ago} with a free lane: start it, or \
                 `giverny-manage hold {key} --why \"<reason>\"`."
            ),
            Kind::Worker => format!(
                "Giverny: `{key}` is Running with no worker ({ago} since it was handed \
                 out): spawn one for it, hand it to an idle one (`giverny-manage start \
                 {key} --agent <id>`), or `giverny-manage pause {key}` / \
                 `giverny-manage drop {key}`."
            ),
        }
    }
}

/// `<feed dir>/chores/<session>.json`: what `session`'s manager has to be
/// told.
pub fn path(feed_dir: &Path, session: &str) -> PathBuf {
    feed_dir.join(CHORES_DIR).join(format!("{session}.json"))
}

/// A session id safe to name a file by.
fn safe(session: &str) -> bool {
    !session.is_empty()
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn read(path: &Path) -> Vec<Chore> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("chores").cloned())
        .and_then(|c| serde_json::from_value(c).ok())
        .unwrap_or_default()
}

/// Write `chores` (or remove the file when there are none), under its lock.
fn write(path: &Path, chores: &[Chore]) -> std::io::Result<()> {
    if chores.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    manage::write(path, &json!({ "chores": chores }))
}

/// The app's side: keep `chores` — the ones due and not yet told — as what
/// `session`'s manager is to hear. No file when there are none.
pub fn publish(feed_dir: &Path, session: &str, chores: &[Chore]) -> Result<(), String> {
    if !safe(session) {
        return Err(format!("{session:?} is no session id"));
    }
    let path = path(feed_dir, session);
    if chores.is_empty() && std::fs::metadata(&path).is_err() {
        return Ok(());
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let _lock = Lock::take(&path)?;
    write(&path, chores).map_err(|e| format!("{}: {e}", path.display()))
}

/// The hook's side, on a dispatcher's own call: `session`'s due chores
/// that still stand, each stamped on its row and returned once as the
/// lines to put in the session's context. One `stat` when there are none.
///
/// A chore stands while its row is still in its stage under its key in the
/// session's file (this writer's), unstamped for that instant, and — for a
/// Next up row, or a Running one with no worker — still not held (nor, for
/// the latter, paused). Every chore read leaves the file but the
/// ones not yet due.
pub fn deliver(feed_dir: &Path, session: &str, now: u64) -> Option<String> {
    if !safe(session) {
        return None;
    }
    let path = path(feed_dir, session);
    std::fs::metadata(&path).ok()?;
    let _lock = Lock::take(&path).ok()?;
    let chores = read(&path);
    let (due, later): (Vec<Chore>, Vec<Chore>) = chores.into_iter().partition(|c| c.is_due(now));
    let lines = if due.is_empty() {
        Vec::new()
    } else {
        stamp(feed_dir, session, &due, now)
    };
    let _ = write(&path, &later);
    (!lines.is_empty()).then(|| lines.join("\n\n"))
}

/// Stamp each of `due` that still stands on its row in `session`'s file, and
/// say it.
fn stamp(feed_dir: &Path, session: &str, due: &[Chore], now: u64) -> Vec<String> {
    let file = manage::file_for(feed_dir, session);
    let Ok(_lock) = Lock::take(&file) else {
        return Vec::new();
    };
    let Some(mut doc) = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(|d| manage::writer_of(d) == Some(manage::WRITER))
    else {
        return Vec::new();
    };
    let Some(rows) = doc.get_mut("rows").and_then(Value::as_array_mut) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for c in due {
        let row = rows.iter_mut().filter_map(Value::as_object_mut).find(|r| {
            r.get("key").and_then(Value::as_str) == Some(c.key.as_str())
                && c.kind.stands_in(manage::stage_of(r))
        });
        let Some(row) = row else { continue };
        if manage::u64_of(row, c.kind.field()) == Some(c.since) {
            continue; // told already
        }
        if c.kind == Kind::Start && row.contains_key("held") {
            continue; // it waits on a reason now
        }
        if c.kind == Kind::Worker && (row.contains_key("held") || row.contains_key("paused_since"))
        {
            continue; // set aside now
        }
        row.insert(c.kind.field().into(), json!(c.since));
        lines.push(c.line(now));
    }
    if !lines.is_empty() && manage::write(&file, &doc).is_err() {
        return Vec::new();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_790_000_000_000;
    const MIN: u64 = 60_000;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("giverny-chores-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn doc(rows: Value) -> Value {
        json!({"version": 1, "session": "s1", "writer": manage::WRITER, "rows": rows})
    }

    fn land(key: &str, since: u64) -> Chore {
        Chore {
            key: key.into(),
            kind: Kind::Land,
            since,
            how: Some("finished".into()),
        }
    }

    fn start(key: &str, since: u64) -> Chore {
        Chore {
            key: key.into(),
            kind: Kind::Start,
            since,
            how: None,
        }
    }

    #[test]
    fn each_kind_is_due_past_its_own_threshold() {
        let l = land("a", T0);
        assert!(!l.is_due(T0 + 2 * MIN + 59_000), "in its grace");
        assert!(l.is_due(T0 + 3 * MIN));
        let s = start("b", T0);
        assert!(!s.is_due(T0 + 4 * MIN));
        assert!(s.is_due(T0 + 5 * MIN));
        assert_eq!(LAND_GRACE_MS, 3 * MIN);
        assert_eq!(IDLE_PLANNED_MS, 5 * MIN);
    }

    #[test]
    fn the_lines_name_the_task_the_time_and_the_command() {
        assert_eq!(
            land("demo#7", T0).line(T0 + 4 * MIN),
            "Giverny: `demo#7`'s worker finished 4m ago: check its report and \
             `giverny-manage land demo#7`."
        );
        let mut failed = land("demo#7", T0);
        failed.how = Some("failed".into());
        assert!(failed.line(T0 + 4 * MIN).contains("worker failed 4m ago"));
        assert_eq!(
            start("demo#8", T0).line(T0 + 12 * MIN),
            "Giverny: `demo#8` has waited 12m with a free lane: start it, or \
             `giverny-manage hold demo#8 --why \"<reason>\"`."
        );
    }

    #[test]
    fn a_chore_reaches_its_manager_once_and_only_once_due() {
        let dir = temp("once");
        let f = feed::feed_path(&dir, "s1");
        manage::write(
            &f,
            &doc(json!([
                {"key": "a", "stage": "running", "started": manage::stamp(T0)},
                {"key": "b", "stage": "planned", "planned": manage::stamp(T0)},
                {"key": "c", "stage": "planned", "planned": manage::stamp(T0)}
            ])),
        )
        .unwrap();
        // Nothing kept: one stat, nothing said.
        assert_eq!(deliver(&dir, "s1", T0 + 10 * MIN), None);
        let chores = [
            land("a", T0 + 8 * MIN),
            start("b", T0),
            start("c", T0 + 7 * MIN),
        ];
        publish(&dir, "s1", &chores).unwrap();
        // At 10m: `a` is 2m into its grace, `c` 3m idle; only `b` is due.
        let said = deliver(&dir, "s1", T0 + 10 * MIN).unwrap();
        assert!(said.contains("`b` has waited 10m"), "{said}");
        assert!(!said.contains("`a`") && !said.contains("`c`"), "{said}");
        let back = feed::read(&f).unwrap();
        assert_eq!(back.rows[1].start_nudged_ms, Some(T0));
        // The ones not yet due stay; `b` is gone from the file.
        assert_eq!(
            read(&path(&dir, "s1")),
            [chores[0].clone(), chores[2].clone()]
        );
        let said = deliver(&dir, "s1", T0 + 12 * MIN).unwrap();
        assert!(said.contains("`a`'s worker finished 4m ago"), "{said}");
        assert!(said.contains("`c` has waited 5m"), "{said}");
        assert!(!path(&dir, "s1").exists(), "nothing left to tell");
        // The app publishing the same chores again (its feed a poll behind)
        // tells nothing twice.
        publish(&dir, "s1", &chores).unwrap();
        assert_eq!(deliver(&dir, "s1", T0 + 13 * MIN), None);
        let back = feed::read(&f).unwrap();
        assert!(chores.iter().zip(&back.rows).all(|(c, r)| c.told(r)));
        // A new instant is a new chore: the worker finished again.
        publish(&dir, "s1", &[land("a", T0 + 20 * MIN)]).unwrap();
        assert!(
            deliver(&dir, "s1", T0 + 24 * MIN)
                .unwrap()
                .contains("`a`'s worker")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn worker(key: &str, since: u64) -> Chore {
        Chore {
            key: key.into(),
            kind: Kind::Worker,
            since,
            how: None,
        }
    }

    /// giverny#334: a Running row with no worker is its manager's, told once
    /// past the hand-off window; set aside (paused, held) or landed before
    /// the hook runs, it is dropped unsaid.
    #[test]
    fn a_row_with_no_worker_reaches_its_manager_unless_set_aside() {
        assert!(!worker("a", T0).is_due(T0 + 2 * MIN));
        assert!(worker("a", T0).is_due(T0 + 3 * MIN));
        assert_eq!(
            worker("demo#9", T0).line(T0 + 7 * MIN),
            "Giverny: `demo#9` is Running with no worker (7m since it was handed out): \
             spawn one for it, hand it to an idle one (`giverny-manage start demo#9 \
             --agent <id>`), or `giverny-manage pause demo#9` / `giverny-manage drop demo#9`."
        );
        let dir = temp("worker");
        let f = feed::feed_path(&dir, "s1");
        manage::write(
            &f,
            &doc(json!([
                {"key": "a", "stage": "running", "started": manage::stamp(T0)},
                {"key": "b", "stage": "running", "started": manage::stamp(T0),
                 "paused_since": manage::stamp(T0 + MIN)},
                {"key": "c", "stage": "done"}
            ])),
        )
        .unwrap();
        publish(
            &dir,
            "s1",
            &[worker("a", T0), worker("b", T0), worker("c", T0)],
        )
        .unwrap();
        let said = deliver(&dir, "s1", T0 + 5 * MIN).unwrap();
        assert!(said.contains("`a` is Running with no worker"), "{said}");
        assert!(!said.contains("`b`") && !said.contains("`c`"), "{said}");
        let back = feed::read(&f).unwrap();
        assert_eq!(back.rows[0].worker_nudged_ms, Some(T0));
        assert_eq!(back.rows[1].worker_nudged_ms, None);
        publish(&dir, "s1", &[worker("a", T0)]).unwrap();
        assert_eq!(deliver(&dir, "s1", T0 + 6 * MIN), None, "told once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_chore_that_no_longer_stands_is_dropped_unsaid() {
        let dir = temp("moot");
        let f = feed::feed_path(&dir, "s1");
        manage::write(
            &f,
            &doc(json!([
                {"key": "landed", "stage": "done"},
                {"key": "held", "stage": "planned", "held": "Ita to answer"}
            ])),
        )
        .unwrap();
        publish(
            &dir,
            "s1",
            &[land("landed", T0), start("held", T0), start("gone", T0)],
        )
        .unwrap();
        assert_eq!(deliver(&dir, "s1", T0 + 10 * MIN), None);
        assert!(!path(&dir, "s1").exists());
        // Another writer's file: none of ours to stamp or tell.
        std::fs::write(
            &f,
            r#"{"version":1,"session":"s1","writer":"other/w","rows":[{"key":"x","stage":"running"}]}"#,
        )
        .unwrap();
        publish(&dir, "s1", &[land("x", T0)]).unwrap();
        assert_eq!(deliver(&dir, "s1", T0 + 10 * MIN), None);
        // Publishing nothing clears the file; a bad session id names none.
        publish(&dir, "s1", &[start("y", T0)]).unwrap();
        publish(&dir, "s1", &[]).unwrap();
        assert!(!path(&dir, "s1").exists());
        assert!(publish(&dir, "../s1", &[start("y", T0)]).is_err());
        assert_eq!(deliver(&dir, "../s1", T0), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

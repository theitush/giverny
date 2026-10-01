//! What `giverny pass` learns its estimates from (giverny#143).
//!
//! Every task that lands appends one line to `history.jsonl` beside the
//! feeds: its raw estimate (the guess as given, before any correction), its
//! wall-clock time, and its *working* time — wall time minus the spans it was
//! paused and the spans its worker said it was waiting. Append-only, one JSON
//! object per line, so concurrent passes never lose each other's lines and a
//! line this version cannot read is skipped, not fatal.
//!
//! [`correct`] then scales a new guess by the median working-time ÷ estimate
//! ratio over the most recent landed tasks of the same kind: the same repo and
//! the same type word in the title (`BUG:`, `FEATURE:` …), else the same repo,
//! else every task. With too little history the guess stands as given.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The history's file name inside the feed directory. Not `*.json`, so the
/// feed scan ([`crate::feed::find`]) never reads it as a feed.
pub const FILE: &str = "history.jsonl";

/// Overrides where the history lives (an empty value turns learning off).
pub const ENV: &str = "GIVERNY_PASS_HISTORY";

/// How many matching tasks a level needs before it corrects anything.
pub const MIN_SAMPLES: usize = 5;
/// How many of the most recent matching tasks the median is taken over.
pub const RECENT: usize = 20;
/// The ratio is held inside this range, so one odd history cannot turn a
/// guess into nonsense.
pub const RATIO_MIN: f64 = 0.25;
pub const RATIO_MAX: f64 = 4.0;

/// One landed task.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The guess as given to `plan`/`start --eta`, before correction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate_s: Option<u64>,
    /// What the pane counted down from at the start (the corrected figure).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_s: Option<u64>,
    /// The last estimate, after any re-estimates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_final_s: Option<u64>,
    pub wall_s: u64,
    #[serde(default)]
    pub paused_s: u64,
    #[serde(default)]
    pub wait_s: u64,
    pub work_s: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

impl Record {
    /// Does this task teach anything about estimates? It needs a guess, a
    /// measured span, and an outcome that means the work was finished
    /// (`Done`, `Review`): a blocked or cancelled task stopped early.
    fn usable(&self) -> bool {
        let finished = self
            .outcome
            .as_deref()
            .is_none_or(|o| o.starts_with("Done") || o.starts_with("Review"));
        finished && self.estimate_s.is_some_and(|e| e > 0) && self.work_s > 0
    }

    fn ratio(&self) -> f64 {
        self.work_s as f64 / self.estimate_s.unwrap_or(1).max(1) as f64
    }
}

/// Where the history lives: `$GIVERNY_PASS_HISTORY`, else
/// `<feed dir>/history.jsonl`. `None` when the variable is set but empty.
pub fn path(feed_dir: &Path) -> Option<PathBuf> {
    match std::env::var_os(ENV) {
        Some(v) if v.is_empty() => None,
        Some(v) => Some(PathBuf::from(v)),
        None => Some(feed_dir.join(FILE)),
    }
}

/// Append one record. A single `write` of a whole line on a file opened for
/// appending, so concurrent writers interleave lines, never bytes.
pub fn append(file: &Path, rec: &Record) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_vec(rec)?;
    line.push(b'\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)?
        .write_all(&line)
}

/// Every record that parses, oldest first.
pub fn load(file: &Path) -> Vec<Record> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The type word a title starts with: `BUG: x` → `BUG`. An upper-case word
/// of two or more letters followed by a colon; anything else has no kind.
pub fn kind_of(title: &str) -> Option<String> {
    let (head, _) = title.trim_start().split_once(':')?;
    let ok = head.len() >= 2 && head.len() <= 16 && head.chars().all(|c| c.is_ascii_uppercase());
    ok.then(|| head.to_string())
}

/// The repo a task is from: `<repo>#<n>` in its key names it; otherwise the
/// repository the command runs in (`cwd`), a worktree counting as its main
/// checkout; otherwise `cwd`'s own name.
pub fn repo_of(key: &str, cwd: &Path) -> Option<String> {
    if let Some((name, n)) = key.split_once('#') {
        let name = name.rsplit('/').next().unwrap_or(name);
        if !name.is_empty() && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
            return Some(name.to_string());
        }
    }
    for dir in cwd.ancestors() {
        let git = dir.join(".git");
        if git.is_dir() {
            return name_of(dir);
        }
        if git.is_file() {
            // A worktree: `gitdir: <main>/.git/worktrees/<name>`.
            let main = std::fs::read_to_string(&git).ok().and_then(|s| {
                let gd = s.trim().strip_prefix("gitdir:")?.trim().to_string();
                let cut = gd.find("/.git/")?;
                Some(PathBuf::from(&gd[..cut]))
            });
            return main.as_deref().and_then(name_of).or_else(|| name_of(dir));
        }
    }
    name_of(cwd)
}

fn name_of(dir: &Path) -> Option<String> {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
}

/// A corrected estimate and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Correction {
    pub eta_s: u64,
    pub ratio: f64,
    pub samples: usize,
    /// Which level matched: `"FEATURE tasks in giverny"`, `"tasks in
    /// giverny"`, `"tasks"`.
    pub basis: String,
}

impl Correction {
    /// `×0.39 from the last 12 FEATURE tasks in giverny`.
    pub fn describe(&self) -> String {
        format!(
            "×{:.2} from the last {} {}",
            self.ratio, self.samples, self.basis
        )
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Correct `guess_s` from `history`, trying the task's repo and kind, then
/// its repo, then everything. `None` when no level has [`MIN_SAMPLES`]
/// usable tasks: the guess stands.
pub fn correct(
    history: &[Record],
    repo: Option<&str>,
    kind: Option<&str>,
    guess_s: u64,
) -> Option<Correction> {
    if guess_s == 0 {
        return None;
    }
    let levels: [(Option<&str>, Option<&str>); 3] = [(repo, kind), (repo, None), (None, None)];
    for (i, (r, k)) in levels.into_iter().enumerate() {
        // Skip a level that would only repeat the next one.
        if (i == 0 && (r.is_none() || k.is_none())) || (i == 1 && r.is_none()) {
            continue;
        }
        let ratios: Vec<f64> = history
            .iter()
            .rev()
            .filter(|h| h.usable())
            .filter(|h| r.is_none_or(|r| h.repo.as_deref() == Some(r)))
            .filter(|h| k.is_none_or(|k| h.kind.as_deref() == Some(k)))
            .take(RECENT)
            .map(Record::ratio)
            .collect();
        if ratios.len() < MIN_SAMPLES {
            continue;
        }
        let samples = ratios.len();
        let ratio = median(ratios).clamp(RATIO_MIN, RATIO_MAX);
        // Whole minutes, never under one: the pane shows minutes.
        let mins = ((guess_s as f64 * ratio) / 60.0).round().max(1.0);
        let basis = match (r, k) {
            (Some(r), Some(k)) => format!("{k} tasks in {r}"),
            (Some(r), None) => format!("tasks in {r}"),
            _ => "tasks".to_string(),
        };
        return Some(Correction {
            eta_s: mins as u64 * 60,
            ratio,
            samples,
            basis,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(repo: &str, kind: Option<&str>, est_m: u64, work_m: u64) -> Record {
        Record {
            key: "t".into(),
            repo: Some(repo.into()),
            kind: kind.map(String::from),
            estimate_s: Some(est_m * 60),
            wall_s: work_m * 60,
            work_s: work_m * 60,
            outcome: Some("Done".into()),
            ..Record::default()
        }
    }

    #[test]
    fn kinds_and_repos() {
        assert_eq!(kind_of("BUG: pane flickers").as_deref(), Some("BUG"));
        assert_eq!(kind_of("FEATURE: x: y").as_deref(), Some("FEATURE"));
        assert_eq!(kind_of("Fix: lower case"), None);
        assert_eq!(kind_of("no colon"), None);
        assert_eq!(kind_of("A: one letter"), None);
        let nowhere = Path::new("/nonexistent/place/proj");
        assert_eq!(repo_of("giverny#143", nowhere).as_deref(), Some("giverny"));
        assert_eq!(
            repo_of("theitush/inbar#7", nowhere).as_deref(),
            Some("inbar")
        );
        assert_eq!(repo_of("auth-fix", nowhere).as_deref(), Some("proj"));
        assert_eq!(repo_of("#12", nowhere).as_deref(), Some("proj"));

        // A worktree names its main checkout.
        let d = std::env::temp_dir().join(format!("giverny-hist-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let main = d.join("myrepo");
        let wt = main.join(".claude/worktrees/b1");
        std::fs::create_dir_all(main.join(".git/worktrees/b1")).unwrap();
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}/.git/worktrees/b1\n", main.display()),
        )
        .unwrap();
        assert_eq!(repo_of("x", &wt.join("src")).as_deref(), Some("myrepo"));
        assert_eq!(repo_of("x", &main).as_deref(), Some("myrepo"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn too_little_history_leaves_the_guess() {
        let h: Vec<Record> = (0..MIN_SAMPLES - 1)
            .map(|_| rec("g", Some("BUG"), 30, 10))
            .collect();
        assert_eq!(correct(&h, Some("g"), Some("BUG"), 1800), None);
        assert_eq!(correct(&[], None, None, 1800), None);
        assert_eq!(correct(&h, Some("g"), Some("BUG"), 0), None);
    }

    #[test]
    fn the_narrowest_level_with_enough_history_wins() {
        let mut h = Vec::new();
        // giverny FEATUREs run at 0.4×, giverny BUGs at 1×, inbar at 2×.
        for _ in 0..5 {
            h.push(rec("giverny", Some("FEATURE"), 50, 20));
            h.push(rec("giverny", Some("BUG"), 20, 20));
            h.push(rec("inbar", Some("BUG"), 10, 20));
        }
        let c = correct(&h, Some("giverny"), Some("FEATURE"), 3000).unwrap();
        assert_eq!(c.eta_s, 20 * 60);
        assert_eq!(c.samples, 5);
        assert_eq!(c.basis, "FEATURE tasks in giverny");
        assert_eq!(
            c.describe(),
            "×0.40 from the last 5 FEATURE tasks in giverny"
        );

        // No RESEARCH history in giverny: repo alone, median of 0.4 and 1.0.
        let c = correct(&h, Some("giverny"), Some("RESEARCH"), 3000).unwrap();
        assert_eq!(c.basis, "tasks in giverny");
        assert_eq!(c.samples, 10);
        assert!((c.ratio - 0.7).abs() < 1e-9, "{}", c.ratio);
        assert_eq!(c.eta_s, 35 * 60);

        // A repo with no history at all: everything (0.4, 1, 2 → 1).
        let c = correct(&h, Some("planets"), Some("BUG"), 600).unwrap();
        assert_eq!(c.basis, "tasks");
        assert_eq!(c.samples, 15);
        assert_eq!(c.eta_s, 600);

        // No repo known and no kind: straight to everything.
        assert_eq!(correct(&h, None, Some("BUG"), 600).unwrap().basis, "tasks");
    }

    #[test]
    fn only_recent_finished_tasks_with_a_guess_count_and_the_ratio_is_held() {
        let mut h = Vec::new();
        // Twenty old tasks at 3×, then twenty recent ones at 0.5×.
        for _ in 0..RECENT {
            h.push(rec("g", None, 10, 30));
        }
        for _ in 0..RECENT {
            h.push(rec("g", None, 10, 5));
        }
        // Blocked, guessless and zero-work tasks teach nothing.
        let mut blocked = rec("g", None, 10, 100);
        blocked.outcome = Some("Blocked — needs a key".into());
        let mut guessless = rec("g", None, 10, 100);
        guessless.estimate_s = None;
        let mut review = rec("g", None, 10, 5);
        review.outcome = Some("Review — ita".into());
        h.extend([blocked, guessless, review]);
        let c = correct(&h, Some("g"), None, 600).unwrap();
        assert_eq!(c.samples, RECENT);
        assert!((c.ratio - 0.5).abs() < 1e-9);
        assert_eq!(c.eta_s, 300);

        // A wild history is clamped.
        let wild: Vec<Record> = (0..5).map(|_| rec("w", None, 10, 1000)).collect();
        let c = correct(&wild, Some("w"), None, 600).unwrap();
        assert_eq!(c.ratio, RATIO_MAX);
        assert_eq!(c.eta_s, 2400);
        // Never under a minute.
        let fast: Vec<Record> = (0..5).map(|_| rec("f", None, 100, 1)).collect();
        assert_eq!(correct(&fast, Some("f"), None, 90).unwrap().eta_s, 60);
    }

    #[test]
    fn append_and_load_round_trip_and_skip_bad_lines() {
        let d = std::env::temp_dir().join(format!("giverny-hist-io-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let f = d.join(FILE);
        let a = rec("g", Some("BUG"), 10, 5);
        append(&f, &a).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&f)
            .unwrap()
            .write_all(b"not json\n\n")
            .unwrap();
        append(&f, &rec("g", None, 20, 5)).unwrap();
        let got = load(&f);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], a);
        assert!(load(&d.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}

//! What `giverny orchestrator-session` learns its estimates from.
//!
//! Every task that lands appends one line to `history.jsonl` beside the
//! feeds: its estimate (the guess as given), its
//! wall-clock time, and its *working* time — wall time minus the spans it was
//! paused and the spans its worker said it was waiting. Append-only, one JSON
//! object per line, so concurrent orchestrator sessions never lose each other's lines and a
//! line this version cannot read is skipped, not fatal.
//!
//! Nothing is corrected: every figure goes on the pane and into the history
//! as given. Each estimator is scored on its own **track** — the
//! dispatcher's guess against the whole working time, and the worker's first
//! re-estimate (made after reading the code) against the working time that
//! was still to come when it was made — and is *told* how its past estimates
//! fared ([`track_record`]): the median working-time ÷ estimate ratio over
//! the most recent landed tasks of the same kind (the same repo and the same
//! type word in the title, `BUG:`, `FEATURE:` …, else the same repo, else
//! every task). Plain arithmetic over the file, nothing else. [`accuracy`]
//! reports every track's error, older against recent, so whether estimates
//! improve is read from the data.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The history's file name inside the feed directory. Not `*.json`, so the
/// feed scan ([`crate::feed::find`]) never reads it as a feed.
pub const FILE: &str = "history.jsonl";

/// Overrides where the history lives (an empty value turns learning off).
pub const ENV: &str = "GIVERNY_ORCHESTRATOR_SESSION_HISTORY";

/// [`ENV`]'s name from when an orchestrator session was a "pass", still read
/// when [`ENV`] is unset.
pub const OLD_ENV: &str = "GIVERNY_PASS_HISTORY";

/// How many matching tasks a level needs before its track record is told.
pub const MIN_SAMPLES: usize = 5;
/// How many of the most recent matching tasks the median is taken over.
pub const RECENT: usize = 20;

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
    /// The guess as given to `plan`/`start --eta`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate_s: Option<u64>,
    /// What the pane counted down from at the start: the guess itself, or
    /// on lines written before giverny#229, the guess corrected. Kept so old
    /// lines read; no track scores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_s: Option<u64>,
    /// The last estimate, after any re-estimates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_final_s: Option<u64>,
    /// The worker's first re-estimate of the time left, as given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reest_s: Option<u64>,
    /// Working time already spent when the re-estimate was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reest_at_s: Option<u64>,
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
    /// The highest peak memory of the task's `giverny orchestrator-session run` commands,
    /// MiB: the cgroup's `memory.peak` under a systemd
    /// scope, else the largest process's RSS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_mb: Option<u64>,
    /// CPU time of those commands, summed, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_s: Option<u64>,
    /// How many of them the memory cap killed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oom_kills: Option<u64>,
}

impl Record {
    /// Did the work finish (`Done`, `Review`)? A blocked or cancelled task
    /// stopped early and teaches nothing about estimates.
    fn finished(&self) -> bool {
        self.outcome
            .as_deref()
            .is_none_or(|o| o.starts_with("Done") || o.starts_with("Review"))
    }
}

/// One estimator's figures, each scored against what it estimated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    /// The dispatcher's guess (`plan`/`start --eta`), as given, against the
    /// whole working time.
    Guess,
    /// The worker's first re-estimate of the time left, as given, against
    /// the working time still to come when it was made.
    Reestimate,
}

impl Track {
    pub const ALL: [Track; 2] = [Track::Guess, Track::Reestimate];

    /// `(estimated, took)` in seconds, when `r` teaches this track anything.
    pub fn pair(self, r: &Record) -> Option<(u64, u64)> {
        if !r.finished() || r.work_s == 0 {
            return None;
        }
        let left = || r.work_s.checked_sub(r.reest_at_s?).filter(|l| *l > 0);
        let (est, took) = match self {
            Track::Guess => (r.estimate_s?, r.work_s),
            Track::Reestimate => (r.reest_s?, left()?),
        };
        (est > 0).then_some((est, took))
    }

    /// Took ÷ estimated.
    fn ratio(self, r: &Record) -> Option<f64> {
        self.pair(r).map(|(e, t)| t as f64 / e as f64)
    }

    /// What the track's estimates are called: `BUG guesses`.
    pub fn noun(self) -> &'static str {
        match self {
            Track::Guess => "guesses",
            Track::Reestimate => "re-estimates",
        }
    }

    /// The heading [`accuracy`] gives it.
    pub fn heading(self) -> &'static str {
        match self {
            Track::Guess => "dispatcher's guess, as given",
            Track::Reestimate => "worker's re-estimate of the time left, as given",
        }
    }
}

/// Where the history lives: `$GIVERNY_ORCHESTRATOR_SESSION_HISTORY`, else
/// `<feed dir>/history.jsonl`. `None` when the variable is set but empty.
pub fn path(feed_dir: &Path) -> Option<PathBuf> {
    match std::env::var_os(ENV).or_else(|| std::env::var_os(OLD_ENV)) {
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

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// The most recent ratios of `track` at the narrowest level with
/// [`MIN_SAMPLES`] (repo and kind, repo, all), with that level's words.
fn level_ratios(
    history: &[Record],
    track: Track,
    repo: Option<&str>,
    kind: Option<&str>,
) -> Option<(Vec<f64>, String)> {
    let levels: [(Option<&str>, Option<&str>); 3] = [(repo, kind), (repo, None), (None, None)];
    for (i, (r, k)) in levels.into_iter().enumerate() {
        // Skip a level that would only repeat the next one.
        if (i == 0 && (r.is_none() || k.is_none())) || (i == 1 && r.is_none()) {
            continue;
        }
        let ratios: Vec<f64> = history
            .iter()
            .rev()
            .filter(|h| r.is_none_or(|r| h.repo.as_deref() == Some(r)))
            .filter(|h| k.is_none_or(|k| h.kind.as_deref() == Some(k)))
            .filter_map(|h| track.ratio(h))
            .take(RECENT)
            .collect();
        if ratios.len() < MIN_SAMPLES {
            continue;
        }
        // `{}` is where the track's noun goes: `FEATURE {} in demo`.
        let words = match (r, k) {
            (Some(r), Some(k)) => format!("{k} {{}} in {r}"),
            (Some(r), None) => format!("{{}} in {r}"),
            _ => "{}".to_string(),
        };
        return Some((ratios, words));
    }
    None
}

/// How an estimator's own past estimates fared, for it to see before it
/// makes the next one: `your last 8 FEATURE guesses in giverny took ×0.21
/// of what you said (median)`. Told, never applied: the figure it gives next
/// is the figure the pane shows. `None` with too little history.
pub fn track_record(
    history: &[Record],
    track: Track,
    repo: Option<&str>,
    kind: Option<&str>,
) -> Option<String> {
    let (ratios, words) = level_ratios(history, track, repo, kind)?;
    let n = ratios.len();
    let r = median(ratios);
    Some(format!(
        "your last {n} {} took ×{r:.2} of what you said (median)",
        words.replace("{}", track.noun())
    ))
}

/// One group's figures on a track: how many, the median ratio (bias) and
/// the typical miss either way (the median of `max(r, 1/r)`: ×1.5 means
/// half the estimates were within half again of the truth).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    pub n: usize,
    pub bias: f64,
    pub off: f64,
}

/// [`Score`] of a set of ratios, `None` when empty.
pub fn score(ratios: &[f64]) -> Option<Score> {
    if ratios.is_empty() {
        return None;
    }
    Some(Score {
        n: ratios.len(),
        bias: median(ratios.to_vec()),
        off: median(ratios.iter().map(|r| r.max(1.0 / r)).collect()),
    })
}

/// `giverny orchestrator-session accuracy`: every track's error, older half
/// against recent half, for all tasks, each repo, and each repo's types with
/// [`MIN_SAMPLES`] or more. `repo` narrows it to one repo.
pub fn accuracy(history: &[Record], repo: Option<&str>) -> String {
    let history: Vec<&Record> = history
        .iter()
        .filter(|h| repo.is_none_or(|r| h.repo.as_deref() == Some(r)))
        .collect();
    let mut out = format!(
        "Estimate accuracy over {} landed task{}{}.\n\
         ratio = time worked ÷ time estimated (median): ×1.00 is right, under it the \
         estimates ran long.\n\
         off = the typical miss either way (median of the ratio or its inverse): ×1.00 \
         is perfect.\n\
         older / recent = the first and second half of each group, oldest first.\n",
        history.len(),
        if history.len() == 1 { "" } else { "s" },
        repo.map(|r| format!(" in {r}")).unwrap_or_default()
    );
    let mut repos: Vec<&str> = history.iter().filter_map(|h| h.repo.as_deref()).collect();
    repos.sort_unstable();
    repos.dedup();
    let fmt = |s: Option<Score>| match s {
        Some(s) => format!("{:>3}  ×{:<5.2} ×{:<5.2}", s.n, s.bias, s.off),
        None => format!("{:>3}  {:<6} {:<6}", 0, "—", "—"),
    };
    for track in Track::ALL {
        out.push_str(&format!(
            "\n{}\n  {:<24}{:<20}{:<20}\n",
            track.heading(),
            "",
            "older: n ratio off",
            "recent: n ratio off"
        ));
        let mut groups: Vec<(String, Vec<f64>)> = Vec::new();
        let ratios = |f: &dyn Fn(&Record) -> bool| -> Vec<f64> {
            history
                .iter()
                .filter(|h| f(h))
                .filter_map(|h| track.ratio(h))
                .collect()
        };
        let all = ratios(&|_| true);
        if all.is_empty() {
            out.push_str("  no tasks with this figure yet\n");
            continue;
        }
        if repo.is_none() {
            groups.push(("all".into(), all));
        }
        for r in &repos {
            groups.push(((*r).to_string(), ratios(&|h| h.repo.as_deref() == Some(r))));
            let mut kinds: Vec<&str> = history
                .iter()
                .filter(|h| h.repo.as_deref() == Some(r))
                .filter_map(|h| h.kind.as_deref())
                .collect();
            kinds.sort_unstable();
            kinds.dedup();
            for k in kinds {
                let v = ratios(&|h| h.repo.as_deref() == Some(r) && h.kind.as_deref() == Some(k));
                if v.len() >= MIN_SAMPLES {
                    groups.push((format!("{r} {k}"), v));
                }
            }
        }
        for (name, v) in groups.into_iter().filter(|(_, v)| !v.is_empty()) {
            let (old, new) = v.split_at(v.len() / 2);
            out.push_str(&format!(
                "  {:<24}{:<20}{:<20}\n",
                name,
                fmt(score(old)),
                fmt(score(new))
            ));
        }
    }
    out.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

/// How many tasks with a measured peak a level needs before
/// [`peak_hint`] speaks.
pub const MIN_PEAK_SAMPLES: usize = 3;

/// What the most recent tasks like this one peaked at under `giverny orchestrator-session
/// run`, by the same levels as [`track_record`] (repo and kind, repo, all):
/// `the last 4 BUG tasks in demo peaked at 1.8G (median), 2.6G at most`.
/// A task the cap killed counts at its peak, which is a floor.
pub fn peak_hint(history: &[Record], repo: Option<&str>, kind: Option<&str>) -> Option<String> {
    let levels: [(Option<&str>, Option<&str>); 3] = [(repo, kind), (repo, None), (None, None)];
    for (i, (r, k)) in levels.into_iter().enumerate() {
        if (i == 0 && (r.is_none() || k.is_none())) || (i == 1 && r.is_none()) {
            continue;
        }
        let peaks: Vec<u64> = history
            .iter()
            .rev()
            .filter(|h| r.is_none_or(|r| h.repo.as_deref() == Some(r)))
            .filter(|h| k.is_none_or(|k| h.kind.as_deref() == Some(k)))
            .filter_map(|h| h.peak_mb.filter(|p| *p > 0))
            .take(RECENT)
            .collect();
        if peaks.len() < MIN_PEAK_SAMPLES {
            continue;
        }
        let what = match (r, k) {
            (Some(r), Some(k)) => format!("{k} tasks in {r}"),
            (Some(r), None) => format!("tasks in {r}"),
            _ => "tasks".to_string(),
        };
        let max = peaks.iter().copied().max().unwrap_or(0);
        let med = median(peaks.iter().map(|p| *p as f64).collect()).round() as u64;
        let mem = |m: u64| giverny_core::limits::Mem(m).to_string();
        return Some(format!(
            "the last {} {what} peaked at {} (median), {} at most",
            peaks.len(),
            mem(med),
            mem(max)
        ));
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
    fn peaks_are_hinted_from_the_narrowest_level_with_three() {
        let peak = |repo: &str, kind: &str, mb: u64| Record {
            peak_mb: Some(mb),
            ..rec(repo, Some(kind), 10, 10)
        };
        let mut h = vec![
            peak("demo", "BUG", 1024),
            peak("demo", "BUG", 2662),
            peak("demo", "FEATURE", 6000),
        ];
        // Two BUGs is too few: the repo speaks, over all three.
        assert_eq!(
            peak_hint(&h, Some("demo"), Some("BUG")).as_deref(),
            Some("the last 3 tasks in demo peaked at 2.6G (median), 5.9G at most")
        );
        h.push(peak("demo", "BUG", 1843));
        assert_eq!(
            peak_hint(&h, Some("demo"), Some("BUG")).as_deref(),
            Some("the last 3 BUG tasks in demo peaked at 1.8G (median), 2.6G at most")
        );
        // Tasks with no measured peak say nothing; nor does too little.
        assert_eq!(peak_hint(&[rec("x", None, 1, 1)], None, None), None);
        assert_eq!(
            peak_hint(&h, Some("acme"), None)
                .as_deref()
                .map(|s| &s[..12]),
            Some("the last 4 t")
        );
    }

    #[test]
    fn kinds_and_repos() {
        assert_eq!(kind_of("BUG: pane flickers").as_deref(), Some("BUG"));
        assert_eq!(kind_of("FEATURE: x: y").as_deref(), Some("FEATURE"));
        assert_eq!(kind_of("Fix: lower case"), None);
        assert_eq!(kind_of("no colon"), None);
        assert_eq!(kind_of("A: one letter"), None);
        let nowhere = Path::new("/nonexistent/place/proj");
        assert_eq!(repo_of("demo#143", nowhere).as_deref(), Some("demo"));
        assert_eq!(repo_of("owner/acme#7", nowhere).as_deref(), Some("acme"));
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
    fn too_little_history_tells_nothing() {
        let h: Vec<Record> = (0..MIN_SAMPLES - 1)
            .map(|_| rec("g", Some("BUG"), 30, 10))
            .collect();
        assert_eq!(track_record(&h, Track::Guess, Some("g"), Some("BUG")), None);
        assert_eq!(track_record(&[], Track::Guess, None, None), None);
    }

    #[test]
    fn the_narrowest_level_with_enough_history_wins() {
        let mut h = Vec::new();
        // demo FEATUREs run at 0.4×, demo BUGs at 1×, acme at 2×.
        for _ in 0..5 {
            h.push(rec("demo", Some("FEATURE"), 50, 20));
            h.push(rec("demo", Some("BUG"), 20, 20));
            h.push(rec("acme", Some("BUG"), 10, 20));
        }
        let told = |repo, kind| track_record(&h, Track::Guess, repo, kind).unwrap();
        assert_eq!(
            told(Some("demo"), Some("FEATURE")),
            "your last 5 FEATURE guesses in demo took ×0.40 of what you said (median)"
        );
        // No RESEARCH history in demo: repo alone, median of 0.4 and 1.0.
        assert_eq!(
            told(Some("demo"), Some("RESEARCH")),
            "your last 10 guesses in demo took ×0.70 of what you said (median)"
        );
        // A repo with no history at all: everything (0.4, 1, 2 → 1).
        assert_eq!(
            told(Some("planets"), Some("BUG")),
            "your last 15 guesses took ×1.00 of what you said (median)"
        );
        // No repo known: straight to everything.
        assert_eq!(told(None, Some("BUG")), told(Some("planets"), None));
    }

    #[test]
    fn only_recent_finished_tasks_with_a_guess_count_and_nothing_is_held() {
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
        assert_eq!(
            track_record(&h, Track::Guess, Some("g"), None).unwrap(),
            "your last 20 guesses in g took ×0.50 of what you said (median)"
        );
        // A wild history is told as measured.
        let wild: Vec<Record> = (0..5).map(|_| rec("w", None, 10, 1000)).collect();
        assert_eq!(
            track_record(&wild, Track::Guess, Some("w"), None).unwrap(),
            "your last 5 guesses in w took ×100.00 of what you said (median)"
        );
    }

    #[test]
    fn old_lines_with_a_corrected_start_figure_still_score_the_guess() {
        // Before giverny#229 the pane counted down from a corrected `eta_s`;
        // the guess track reads `estimate_s`, the figure as given.
        let old: Record = serde_json::from_str(
            r#"{"key":"x","estimate_s":3000,"eta_s":1200,"wall_s":600,"work_s":600,"outcome":"Done"}"#,
        )
        .unwrap();
        assert_eq!(old.eta_s, Some(1200));
        assert_eq!(Track::Guess.pair(&old), Some((3000, 600)));
    }

    /// A task whose worker re-estimated `left_m` after `at_m` worked.
    fn reest(repo: &str, kind: &str, est_m: u64, work_m: u64, at_m: u64, left_m: u64) -> Record {
        Record {
            reest_s: Some(left_m * 60),
            reest_at_s: Some(at_m * 60),
            ..rec(repo, Some(kind), est_m, work_m)
        }
    }

    #[test]
    fn each_track_scores_its_own_estimate() {
        // Guessed 60m, worked 20m; the worker said 10m left at 5m in, and
        // 15m more were worked.
        let r = reest("g", "BUG", 60, 20, 5, 10);
        assert_eq!(Track::Guess.pair(&r), Some((3600, 1200)));
        assert_eq!(Track::Reestimate.pair(&r), Some((600, 900)));
        // Old records (no re-estimate fields) still load and score the guess.
        let old: Record = serde_json::from_str(
            r#"{"key":"x","estimate_s":600,"wall_s":300,"work_s":300,"outcome":"Done"}"#,
        )
        .unwrap();
        assert_eq!(Track::Guess.pair(&old), Some((600, 300)));
        assert_eq!(Track::Reestimate.pair(&old), None);
        // A re-estimate made after the work ended teaches nothing.
        assert_eq!(
            Track::Reestimate.pair(&reest("g", "BUG", 60, 20, 20, 5)),
            None
        );
    }

    #[test]
    fn re_estimates_are_told_from_their_own_history() {
        // Guesses run ×0.2; re-estimates run ×1.5 (workers say too little).
        let h: Vec<Record> = (0..6)
            .map(|_| reest("g", "FEATURE", 100, 20, 5, 10))
            .collect();
        assert_eq!(
            track_record(&h, Track::Guess, Some("g"), Some("FEATURE")).unwrap(),
            "your last 6 FEATURE guesses in g took ×0.20 of what you said (median)"
        );
        assert_eq!(
            track_record(&h, Track::Reestimate, Some("x"), None).unwrap(),
            "your last 6 re-estimates took ×1.50 of what you said (median)"
        );
        assert_eq!(track_record(&h[..4], Track::Guess, None, None), None);
    }

    #[test]
    fn accuracy_compares_older_and_recent_per_track() {
        let mut h = Vec::new();
        // Older guesses ×0.2, recent ×0.8: getting better.
        for _ in 0..5 {
            h.push(reest("g", "BUG", 50, 10, 2, 8));
        }
        for _ in 0..5 {
            h.push(reest("g", "BUG", 50, 40, 2, 38));
        }
        h.push(rec("other", None, 10, 10));
        let s = score(&[0.5, 2.0, 1.0]).unwrap();
        assert_eq!((s.n, s.bias, s.off), (3, 1.0, 2.0));
        let out = accuracy(&h, None);
        assert!(
            out.starts_with("Estimate accuracy over 11 landed tasks."),
            "{out}"
        );
        let guess = out
            .lines()
            .skip_while(|l| !l.starts_with("dispatcher's guess"))
            .nth(2)
            .unwrap();
        assert!(guess.trim_start().starts_with("all"), "{out}");
        assert!(guess.contains("×0.20") && guess.contains("×0.80"), "{out}");
        assert!(out.contains("  g BUG"), "{out}");
        assert!(out.contains("re-estimate of the time left"), "{out}");
        // One repo only.
        let only = accuracy(&h, Some("other"));
        assert!(only.contains("over 1 landed task in other"), "{only}");
        assert!(only.contains("no tasks with this figure yet"), "{only}");
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

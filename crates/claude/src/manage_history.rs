//! What `giverny manage` learns its estimates from.
//!
//! Every task that lands appends one line to `history.jsonl` beside the
//! feeds: its estimate (the guess as given), its
//! wall-clock time, and its *working* time — wall time minus the spans it was
//! paused, the spans its worker said it was waiting and the spans the
//! machine slept through ([`crate::sleeps`]). Append-only, one JSON
//! object per line, so concurrent manager sessions never lose each other's lines and a
//! line this version cannot read is skipped, not fatal.
//!
//! Nothing is corrected: every figure goes on the pane and into the history
//! as given. Each estimator is scored on its own **track** — the
//! dispatcher's guess against the whole working time, and the worker's first
//! re-estimate (made after reading the code) against the working time that
//! was still to come when it was made — and is *told* how its instinct runs
//! ([`track_record`]): the median of working time ÷ its *instinct* figure
//! over the most recent landed tasks of the same kind (the same repo and the
//! same type word in the title, `BUG:`, `FEATURE:` …, else the same repo,
//! else every task). Plain arithmetic over the file, nothing else.
//! [`accuracy`] reports every track's error, older against recent, so
//! whether estimates improve is read from the data.
//!
//! **The instinct, not the figure** (giverny#371). Each figure is kept with
//! the factor its giver had been shown for its track when giving it
//! (`told_x`, `reest_told_x`; none shown is ×1). The figure as given is the
//! instinct already scaled by that factor, so the instinct figure is
//! `given ÷ told_x` and the factor told is `median(work × told_x ÷ given)`.
//! Scoring the figures as given instead undid itself: once givers applied
//! the factor and landed on target, the records came in at ×1.0, the factor
//! drifted to ×1.0, the givers went back to their instinct and it swung
//! back. Learned from the instinct, the factor stays for as long as the
//! instinct is off, and follows it to ×1.0 only when the instinct itself
//! improves.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The history's file name inside the feed directory. Not `*.json`, so the
/// feed scan ([`crate::feed::find`]) never reads it as a feed.
pub const FILE: &str = "history.jsonl";

/// Overrides where the history lives (an empty value turns learning off).
pub const ENV: &str = "GIVERNY_MANAGE_HISTORY";

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
    /// The factor the dispatcher had been shown for its guess track when it
    /// gave `estimate_s` (giverny#371): ×1 when none was shown, and on
    /// lines written before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub told_x: Option<f64>,
    /// The worker's first re-estimate of the time left, as given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reest_s: Option<u64>,
    /// The factor the worker had been shown for its re-estimate track when
    /// it gave `reest_s` (giverny#371), as [`Record::told_x`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reest_told_x: Option<f64>,
    /// Working time already spent when the re-estimate was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reest_at_s: Option<u64>,
    pub wall_s: u64,
    #[serde(default)]
    pub paused_s: u64,
    #[serde(default)]
    pub wait_s: u64,
    /// What the machine slept through between start and end.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub slept_s: u64,
    pub work_s: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// The highest peak memory of the task's `giverny manage run` commands,
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

    /// The factor the giver had been shown when giving this track's figure
    /// on `r`: ×1 when none was (or the line predates giverny#371), and
    /// for anything that is not a usable factor.
    pub fn told_x(self, r: &Record) -> f64 {
        let x = match self {
            Track::Guess => r.told_x,
            Track::Reestimate => r.reest_told_x,
        };
        x.filter(|x| x.is_finite() && *x > 0.0).unwrap_or(1.0)
    }

    /// How the giver's *instinct* ran on `r`: took ÷ the instinct figure,
    /// which is the figure as given ÷ the factor shown. What the giver is
    /// told ([`track_record`]).
    pub fn ratio(self, r: &Record) -> Option<f64> {
        self.given_ratio(r).map(|g| g * self.told_x(r))
    }

    /// How the figure *as given* ran on `r`: took ÷ estimated. Near ×1.0
    /// when the factor told is being applied.
    pub fn given_ratio(self, r: &Record) -> Option<f64> {
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

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// Where the history lives: `$GIVERNY_MANAGE_HISTORY`, else
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
        // What the instinct is on: ` on FEATURE in demo`, ` in demo`, ``.
        let words = match (r, k) {
            (Some(r), Some(k)) => format!(" on {k} in {r}"),
            (Some(r), None) => format!(" in {r}"),
            _ => String::new(),
        };
        return Some((ratios, words));
    }
    None
}

/// How an estimator's instinct runs on tasks like this one (giverny#371):
/// the factor it is told, how many tasks it is the median of, and the level
/// it was found at (` on BUG in giverny`, ` in giverny` or empty), which is
/// also what tells one line from another. `None` with too little history.
#[derive(Debug, Clone, PartialEq)]
pub struct Instinct {
    pub x: f64,
    pub n: usize,
    pub level: String,
}

impl Instinct {
    /// The line its giver reads: `your instinct on BUG in giverny runs
    /// ×0.30 (last 20, median): scale your figure by it`.
    pub fn line(&self) -> String {
        format!(
            "your instinct{} runs ×{:.2} (last {}, median): scale your figure by it",
            self.level, self.x, self.n
        )
    }
}

/// The instinct factor of `track` for a task of this repo and kind: the
/// median of working time ÷ instinct figure over the most recent finished
/// tasks at the narrowest level with [`MIN_SAMPLES`].
pub fn instinct(
    history: &[Record],
    track: Track,
    repo: Option<&str>,
    kind: Option<&str>,
) -> Option<Instinct> {
    let (ratios, level) = level_ratios(history, track, repo, kind)?;
    Some(Instinct {
        n: ratios.len(),
        x: median(ratios),
        level,
    })
}

/// How an estimator's instinct runs, for it to see before it makes the
/// next figure: [`Instinct::line`]. Told, never applied: the figure it gives
/// next is the figure the pane shows. `None` with too little history.
pub fn track_record(
    history: &[Record],
    track: Track,
    repo: Option<&str>,
    kind: Option<&str>,
) -> Option<String> {
    instinct(history, track, repo, kind).map(|i| i.line())
}

/// One group's figures on a track: how many, the median instinct ratio
/// (the factor its givers are told, giverny#371), the median ratio of the
/// figures as given (bias: near ×1.0 when the factor is applied) and their
/// typical miss either way (the median of `max(r, 1/r)`: ×1.5 means half the
/// figures were within half again of the truth).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    pub n: usize,
    pub instinct: f64,
    pub bias: f64,
    pub off: f64,
}

/// [`Score`] of a set of `(instinct, as given)` ratios, `None` when empty.
pub fn score(ratios: &[(f64, f64)]) -> Option<Score> {
    if ratios.is_empty() {
        return None;
    }
    Some(Score {
        n: ratios.len(),
        instinct: median(ratios.iter().map(|r| r.0).collect()),
        bias: median(ratios.iter().map(|r| r.1).collect()),
        off: median(ratios.iter().map(|(_, r)| r.max(1.0 / r)).collect()),
    })
}

/// `giverny manage accuracy`: every track's error, older half
/// against recent half, for all tasks, each repo, and each repo's types with
/// [`MIN_SAMPLES`] or more. `repo` narrows it to one repo. Each group shows
/// both the instinct factor and how the figures as given landed.
pub fn accuracy(history: &[Record], repo: Option<&str>) -> String {
    let history: Vec<&Record> = history
        .iter()
        .filter(|h| repo.is_none_or(|r| h.repo.as_deref() == Some(r)))
        .collect();
    let mut out = format!(
        "Estimate accuracy over {} landed task{}{}.\n\
         instinct = time worked ÷ the figure before the factor shown (median): the \
         factor the line tells; ×1.00 is an instinct that is right.\n\
         given = time worked ÷ the figure as given (median): near ×1.00 when the line \
         is applied, drifting from it when it is not.\n\
         off = the typical miss of the figures as given, either way (median of the \
         ratio or its inverse): ×1.00 is perfect.\n\
         older / recent = the first and second half of each group, oldest first.\n",
        history.len(),
        if history.len() == 1 { "" } else { "s" },
        repo.map(|r| format!(" in {r}")).unwrap_or_default()
    );
    let mut repos: Vec<&str> = history.iter().filter_map(|h| h.repo.as_deref()).collect();
    repos.sort_unstable();
    repos.dedup();
    let fmt = |s: Option<Score>| match s {
        Some(s) => format!(
            "{:>3}  ×{:<7.2} ×{:<5.2} ×{:<5.2}",
            s.n, s.instinct, s.bias, s.off
        ),
        None => format!("{:>3}  {:<8} {:<6} {:<6}", 0, "—", "—", "—"),
    };
    for track in Track::ALL {
        // Each half's column names sit over its figures.
        let cols = "  n  instinct given  off";
        out.push_str(&format!(
            "\n{}\n  {:<24}{:<34}{:<34}\n  {:<24}{cols:<34}{cols:<34}\n",
            track.heading(),
            "",
            "older",
            "recent",
            "",
        ));
        let mut groups: Vec<(String, Vec<(f64, f64)>)> = Vec::new();
        let ratios = |f: &dyn Fn(&Record) -> bool| -> Vec<(f64, f64)> {
            history
                .iter()
                .filter(|h| f(h))
                .filter_map(|h| Some((track.ratio(h)?, track.given_ratio(h)?)))
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
                "  {:<24}{:<34}{:<34}\n",
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

/// The measured peaks of the most recent tasks like this one, by the same
/// levels as [`track_record`] (repo and kind, repo, all): the narrowest
/// with [`MIN_PEAK_SAMPLES`], newest first, with how to name the level
/// (`BUG tasks in demo`).
fn peak_sample(
    history: &[Record],
    repo: Option<&str>,
    kind: Option<&str>,
) -> Option<(String, Vec<u64>)> {
    sample(history, repo, kind, &|h| h.peak_mb.filter(|p| *p > 0))
}

/// One figure (`of`) of the most recent tasks like this one that have it,
/// by the levels [`peak_sample`] uses.
fn sample(
    history: &[Record],
    repo: Option<&str>,
    kind: Option<&str>,
    of: &dyn Fn(&Record) -> Option<u64>,
) -> Option<(String, Vec<u64>)> {
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
            .filter_map(of)
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
        return Some((what, peaks));
    }
    None
}

/// What the most recent tasks like this one peaked at under `giverny manage
/// run`, by the same levels as [`track_record`] (repo and kind, repo, all):
/// `the last 4 BUG tasks in demo peaked at 1.8G (median), 2.6G at most`.
/// A task the cap killed counts at its peak, which is a floor.
pub fn peak_hint(history: &[Record], repo: Option<&str>, kind: Option<&str>) -> Option<String> {
    let (what, peaks) = peak_sample(history, repo, kind)?;
    let max = peaks.iter().copied().max().unwrap_or(0);
    let med = median(peaks.iter().map(|p| *p as f64).collect()).round() as u64;
    let mem = |m: u64| giverny_core::limits::Mem(m).to_string();
    Some(format!(
        "the last {} {what} peaked at {} (median), {} at most",
        peaks.len(),
        mem(med),
        mem(max)
    ))
}

/// The high end of what tasks like this one peak at, MiB: the 90th
/// percentile (nearest rank) of the peaks [`peak_hint`] summarises. What the
/// ledger counts a task at when it admits memory (giverny#281). `None`
/// with too little history.
pub fn expected_peak_mb(history: &[Record], repo: Option<&str>, kind: Option<&str>) -> Option<u64> {
    let (_, mut peaks) = peak_sample(history, repo, kind)?;
    peaks.sort_unstable();
    let rank = (peaks.len() * 9).div_ceil(10).max(1);
    peaks.get(rank - 1).copied()
}

/// The cores a task used over the time it held its lease, in thousandths:
/// its `run` commands' CPU time over its wall time (start to landing).
/// `None` for a task with no CPU time recorded.
pub fn cpu_use_m(r: &Record) -> Option<u64> {
    let cpu = r.cpu_s?;
    (r.wall_s > 0).then(|| (cpu * 1000).div_ceil(r.wall_s))
}

/// The high end of the cores tasks like this one use over their life, in
/// thousandths: the 90th percentile (nearest rank) of [`cpu_use_m`] over
/// the same levels and as many tasks as [`expected_peak_mb`]. What the
/// ledger counts a lease at when it admits CPU (giverny#318). `None` with
/// too little history.
pub fn expected_cpu_m(history: &[Record], repo: Option<&str>, kind: Option<&str>) -> Option<u64> {
    let (_, mut uses) = sample(history, repo, kind, &cpu_use_m)?;
    uses.sort_unstable();
    let rank = (uses.len() * 9).div_ceil(10).max(1);
    uses.get(rank - 1).copied()
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
        // The ledger counts the high end: p90 by nearest rank, here the max of 3.
        assert_eq!(expected_peak_mb(&h, Some("demo"), Some("BUG")), Some(2662));
        let ten: Vec<Record> = (1..=10).map(|i| peak("demo", "BUG", i * 100)).collect();
        assert_eq!(expected_peak_mb(&ten, Some("demo"), Some("BUG")), Some(900));
        assert_eq!(expected_peak_mb(&ten[..2], Some("demo"), Some("BUG")), None);
        // Tasks with no measured peak say nothing; nor does too little.
        assert_eq!(peak_hint(&[rec("x", None, 1, 1)], None, None), None);
        assert_eq!(
            peak_hint(&h, Some("acme"), None)
                .as_deref()
                .map(|s| &s[..12]),
            Some("the last 4 t")
        );
    }

    /// giverny#318: a task's cores over its life are its runs' CPU time
    /// over its wall time; the ledger counts the p90 of its kind's.
    #[test]
    fn cpu_use_is_cpu_time_over_wall_time_and_counted_at_p90() {
        let used = |kind: &str, cpu_s: Option<u64>, wall_m: u64| Record {
            cpu_s,
            ..rec("demo", Some(kind), 10, wall_m)
        };
        // 30 min of CPU in a 60 min task: half a core.
        assert_eq!(cpu_use_m(&used("BUG", Some(1800), 60)), Some(500));
        assert_eq!(cpu_use_m(&used("BUG", None, 60)), None);
        assert_eq!(cpu_use_m(&used("BUG", Some(5), 0)), None);
        let ten: Vec<Record> = (1..=10).map(|i| used("BUG", Some(i * 60), 10)).collect();
        // 0.1 … 1.0 cores: p90 by nearest rank is 0.9.
        assert_eq!(expected_cpu_m(&ten, Some("demo"), Some("BUG")), Some(900));
        // Two is too few at a level, none says nothing; a task with no CPU
        // recorded is not a sample.
        assert_eq!(expected_cpu_m(&ten[..2], Some("demo"), Some("BUG")), None);
        let mut h = ten[..2].to_vec();
        h.push(used("BUG", None, 10));
        assert_eq!(expected_cpu_m(&h, Some("demo"), Some("BUG")), None);
        h.push(used("FEATURE", Some(1200), 10));
        assert_eq!(
            expected_cpu_m(&h, Some("demo"), Some("BUG")),
            Some(2000),
            "the repo speaks over all three"
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
            "your instinct on FEATURE in demo runs ×0.40 (last 5, median): scale your figure by it"
        );
        // No RESEARCH history in demo: repo alone, median of 0.4 and 1.0.
        assert_eq!(
            told(Some("demo"), Some("RESEARCH")),
            "your instinct in demo runs ×0.70 (last 10, median): scale your figure by it"
        );
        // A repo with no history at all: everything (0.4, 1, 2 → 1).
        assert_eq!(
            told(Some("planets"), Some("BUG")),
            "your instinct runs ×1.00 (last 15, median): scale your figure by it"
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
            "your instinct in g runs ×0.50 (last 20, median): scale your figure by it"
        );
        // A wild history is told as measured.
        let wild: Vec<Record> = (0..5).map(|_| rec("w", None, 10, 1000)).collect();
        assert_eq!(
            track_record(&wild, Track::Guess, Some("w"), None).unwrap(),
            "your instinct in w runs ×100.00 (last 5, median): scale your figure by it"
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
            "your instinct on FEATURE in g runs ×0.20 (last 6, median): scale your figure by it"
        );
        assert_eq!(
            track_record(&h, Track::Reestimate, Some("x"), None).unwrap(),
            "your instinct runs ×1.50 (last 6, median): scale your figure by it"
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
        let s = score(&[(0.5, 0.5), (2.0, 2.0), (1.0, 1.0)]).unwrap();
        assert_eq!((s.n, s.instinct, s.bias, s.off), (3, 1.0, 1.0, 2.0));
        let out = accuracy(&h, None);
        assert!(
            out.starts_with("Estimate accuracy over 11 landed tasks."),
            "{out}"
        );
        let guess = out
            .lines()
            .skip_while(|l| !l.starts_with("dispatcher's guess"))
            .nth(3)
            .unwrap();
        assert!(guess.trim_start().starts_with("all"), "{out}");
        assert!(guess.contains("×0.20") && guess.contains("×0.80"), "{out}");
        // Nothing was shown on these lines: the instinct is the figure.
        assert_eq!(guess.matches("×0.20").count(), 2, "{out}");
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
    /// One dispatcher's guess: `instinct_m` is what its gut says, `shown`
    /// the factor it had been told (applied: the figure is the gut scaled by
    /// it), and the work takes `runs` × the gut.
    fn applied(instinct_m: f64, shown: Option<f64>, runs: f64) -> Record {
        let x = shown.unwrap_or(1.0);
        Record {
            told_x: Some(x),
            estimate_s: Some((instinct_m * x * 60.0).round() as u64),
            ..rec("giverny", Some("BUG"), 0, 0)
        }
        .worked((instinct_m * runs * 60.0).round() as u64)
    }

    impl Record {
        fn worked(mut self, s: u64) -> Record {
            self.work_s = s;
            self.wall_s = s;
            self
        }
    }

    /// Thirty BUGs in a row, each given as the gut figure scaled by the
    /// factor `told` says before it, the work taking `runs(i)` × the gut;
    /// returns each task's told factor (`None`: too little history).
    fn simulate(
        runs: impl Fn(usize) -> f64,
        told: impl Fn(&[Record]) -> Option<f64>,
    ) -> (Vec<Record>, Vec<Option<f64>>) {
        let mut h = Vec::new();
        let mut shown = Vec::new();
        for i in 0..30 {
            let x = told(&h);
            shown.push(x);
            h.push(applied(60.0, x, runs(i)));
        }
        (h, shown)
    }

    /// The factor a giver is told, learned from the instinct (giverny#371).
    fn told_now(h: &[Record]) -> Option<f64> {
        instinct(h, Track::Guess, Some("giverny"), Some("BUG")).map(|i| i.x)
    }

    /// The factor as it was told before giverny#371: the median of working
    /// time ÷ the figure *as given*.
    fn told_before(h: &[Record]) -> Option<f64> {
        let (ratios, _) = level_ratios(h, Track::Guess, Some("giverny"), Some("BUG"))?;
        let given: Vec<f64> = h
            .iter()
            .rev()
            .filter_map(|r| Track::Guess.given_ratio(r))
            .take(ratios.len())
            .collect();
        Some(median(given))
    }

    /// giverny#371: an instinct that runs ×0.3, givers applying the factor
    /// shown and landing on target. Learned from the instinct, the factor
    /// stays ×0.30 for good; learned from the figures as given (the code
    /// before), it told ×1.0 once the corrected figures landed on target,
    /// and the givers went back to their instinct.
    #[test]
    fn the_factor_learned_from_the_instinct_does_not_undo_itself() {
        let (h, shown) = simulate(|_| 0.3, told_now);
        assert_eq!(&shown[..MIN_SAMPLES], &[None; MIN_SAMPLES][..]);
        for (i, x) in shown.iter().enumerate().skip(MIN_SAMPLES) {
            assert!((x.unwrap() - 0.3).abs() < 0.01, "task {i} told {x:?}");
        }
        // Those last figures were applied and landed on target.
        let last = h.last().unwrap();
        assert_eq!(Track::Guess.given_ratio(last), Some(1.0));
        assert!((Track::Guess.ratio(last).unwrap() - 0.3).abs() < 0.01);
        assert_eq!(
            track_record(&h, Track::Guess, Some("giverny"), Some("BUG")).as_deref(),
            Some(
                "your instinct on BUG in giverny runs ×0.30 (last 20, median): scale your figure by it"
            )
        );

        // The old reading of those same thirty: the twenty applied figures
        // landed on target, so it told ×1.00, and the next giver went back
        // to its gut.
        assert_eq!(told_before(&h), Some(1.0));
        // Run under the old reading from the start, the factor fades: each
        // applied figure lands nearer ×1.0 and pulls the median after it,
        // until it sits at √0.3 ≈ ×0.55, nowhere near the ×0.30 the gut runs.
        let (_, before) = simulate(|_| 0.3, told_before);
        assert_eq!(before[MIN_SAMPLES], Some(0.3));
        let last = before.last().unwrap().unwrap();
        assert!((last - 0.3_f64.sqrt()).abs() < 0.01, "{before:?}");
    }

    /// An instinct that improves takes the factor with it, toward ×1.0.
    #[test]
    fn an_instinct_that_improves_moves_the_factor_to_one() {
        let (h, shown) = simulate(|i| if i < 10 { 0.3 } else { 1.0 }, told_now);
        assert!((shown[9].unwrap() - 0.3).abs() < 0.01);
        assert!((told_now(&h).unwrap() - 1.0).abs() < 0.01, "{shown:?}");
        // On the way, while the window still holds the old gut.
        assert!(shown[15].unwrap() < shown[25].unwrap(), "{shown:?}");
    }

    /// Lines from before giverny#371 have no `told_x`: they count as ×1,
    /// which is what they were (mostly untold); a new line keeps both.
    #[test]
    fn old_lines_count_as_untold_and_new_ones_keep_the_factor() {
        let old: Record = serde_json::from_str(
            r#"{"key":"x","estimate_s":600,"reest_s":300,"reest_at_s":60,"wall_s":300,"work_s":300,"outcome":"Done"}"#,
        )
        .unwrap();
        assert_eq!((old.told_x, old.reest_told_x), (None, None));
        assert_eq!(Track::Guess.told_x(&old), 1.0);
        assert_eq!(Track::Guess.ratio(&old), Some(0.5));
        assert_eq!(
            Track::Reestimate.ratio(&old),
            Track::Reestimate.given_ratio(&old)
        );
        let new = Record {
            told_x: Some(0.5),
            reest_told_x: Some(2.0),
            ..old.clone()
        };
        let line = serde_json::to_string(&new).unwrap();
        assert!(line.contains(r#""told_x":0.5"#) && line.contains(r#""reest_told_x":2.0"#));
        // Said 600s while shown ×0.5: the gut said 1200s, and took 300s.
        assert_eq!(Track::Guess.ratio(&new), Some(0.25));
        // 300s said while shown ×2: the gut said 150s, and 240s were left.
        assert_eq!(Track::Reestimate.ratio(&new), Some(1.6));
        // A factor that is no factor counts as none.
        let bad = Record {
            told_x: Some(0.0),
            ..old
        };
        assert_eq!(Track::Guess.told_x(&bad), 1.0);
    }

    /// `accuracy` shows both: the instinct factor and the figures as given,
    /// which sit at ×1.00 when the line is applied.
    #[test]
    fn accuracy_shows_the_instinct_and_the_figures_as_given() {
        let (h, _) = simulate(|_| 0.3, told_now);
        let out = accuracy(&h, None);
        assert!(out.contains("instinct = time worked"), "{out}");
        let all = out
            .lines()
            .skip_while(|l| !l.starts_with("dispatcher's guess"))
            .nth(3)
            .unwrap();
        // Older: five untold (given ×0.30) and ten applied (×1.00), so ×1.00
        // in the median; recent: all applied. The instinct is ×0.30 in both.
        assert_eq!(all.matches("×0.30").count(), 2, "{out}");
        assert!(all.ends_with("×1.00  ×1.00"), "{all}");
    }
}

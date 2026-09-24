//! A Done row's Review line (giverny#60).
//!
//! When a task lands in Review, its issue body opens with one line —
//! `**Review:** <who> — <what> — <where>` — which is the thing a person has
//! to read about it. A Done row's overlay shows that line at its top. The
//! row's key names the issue (`giverny#60`, or `owner/repo#60`); the body is
//! one REST read through `gh`, made off the UI thread, and a row whose issue
//! has no such line (or no issue, or no `gh`) opens exactly as before.

use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

/// The GitHub issue a row's key names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    /// `None` for a bare `repo#n`: the owner is the `gh` user's.
    pub owner: Option<String>,
    pub repo: String,
    pub number: u64,
}

/// `repo#n` or `owner/repo#n`; anything else names no issue.
pub fn issue_of(key: &str) -> Option<IssueRef> {
    let (path, n) = key.trim().rsplit_once('#')?;
    let number: u64 = n.parse().ok()?;
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let (owner, repo) = match path.split_once('/') {
        Some((o, r)) if ok(o) && ok(r) => (Some(o.to_string()), r.to_string()),
        Some(_) => return None,
        None if ok(path) => (None, path.to_string()),
        None => return None,
    };
    Some(IssueRef {
        owner,
        repo,
        number,
    })
}

/// The Review line of an issue body, without its `**Review:**` marker: the
/// first line that starts with it, above the body's first `---` rule (below
/// that is the result, which may quote an old one).
pub fn review_line(body: &str) -> Option<String> {
    for line in body.lines() {
        let line = line.trim();
        if line == "---" {
            return None;
        }
        if let Some(rest) = line.strip_prefix("**Review:**") {
            let rest = rest.trim();
            return (!rest.is_empty()).then(|| rest.to_string());
        }
    }
    None
}

/// Where a fetched Review line lands; the overlay reads it every frame.
pub type Slot = Arc<Mutex<Option<String>>>;

/// Fetch `issue`'s Review line on a thread; it appears in the returned slot
/// (and a repaint is asked for) if there is one.
pub fn fetch(issue: IssueRef, ctx: eframe::egui::Context) -> Slot {
    let slot: Slot = Arc::new(Mutex::new(None));
    let out = slot.clone();
    std::thread::Builder::new()
        .name("giverny-review".into())
        .spawn(move || {
            if let Some(line) = read(&issue)
                && let Ok(mut s) = out.lock()
            {
                *s = Some(line);
                ctx.request_repaint();
            }
        })
        .ok();
    slot
}

/// The blocking read: `gh api repos/<owner>/<repo>/issues/<n>` (REST — its
/// budget is separate from GraphQL's, which boards spend).
fn read(issue: &IssueRef) -> Option<String> {
    let owner = match &issue.owner {
        Some(o) => o.clone(),
        None => gh_user()?,
    };
    let body = gh(&[
        "api",
        &format!("repos/{owner}/{}/issues/{}", issue.repo, issue.number),
        "-q",
        ".body",
    ])?;
    review_line(&body)
}

/// The `gh` user's login, asked once per run.
fn gh_user() -> Option<String> {
    static LOGIN: OnceLock<Option<String>> = OnceLock::new();
    LOGIN
        .get_or_init(|| {
            gh(&["api", "user", "-q", ".login"])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .clone()
}

fn gh(args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("gh");
    cmd.args(args).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes up in front of the app.
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_name_issues() {
        assert_eq!(
            issue_of("giverny#60"),
            Some(IssueRef {
                owner: None,
                repo: "giverny".into(),
                number: 60
            })
        );
        assert_eq!(
            issue_of("y0av/giverny#7"),
            Some(IssueRef {
                owner: Some("y0av".into()),
                repo: "giverny".into(),
                number: 7
            })
        );
        assert_eq!(issue_of("a93f0c"), None);
        assert_eq!(issue_of("giverny#"), None);
        assert_eq!(issue_of("#5"), None);
        assert_eq!(issue_of("bad repo#5"), None);
        assert_eq!(issue_of("a/b/c#5"), None);
    }

    #[test]
    fn finds_the_review_line() {
        let body = "**Review:** ita — the empty state — branch x\n\n**Agent:** Wren\n\
                    **Ask** — Ita:\n> hi\n";
        assert_eq!(
            review_line(body).as_deref(),
            Some("ita — the empty state — branch x")
        );
        // Below the signature line is still the top of the body.
        let body = "**Agent:** Wren\n**Review:**  ita — look\n";
        assert_eq!(review_line(body).as_deref(), Some("ita — look"));
    }

    #[test]
    fn no_review_line_or_only_in_the_result() {
        assert_eq!(review_line("**Ask** — none\n\ndetails"), None);
        assert_eq!(
            review_line("**Ask** — none\n\n---\n**Result**\n**Review:** old\n"),
            None
        );
        assert_eq!(review_line("**Review:**   \n"), None);
    }
}

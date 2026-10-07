//! `claude attach <id|name>`: a terminal showing a background job.
//!
//! A claude attached to a job (by hand, by a BACKGROUND click, or by a
//! restart's `claude --resume <conversation>`, which re-execs itself as
//! `attach` when the conversation is a job's) writes no `sessions/<pid>.json`.
//! The only place it says which job it shows is its command line — so that
//! is what is read, under a tab's shell (giverny#243).

use crate::jobs::Job;

/// The target of a `claude … attach <target>` command line, or `None` when
/// the process is not one.
pub fn target(argv: &[String]) -> Option<String> {
    // The program is the first word, or the script a runtime runs.
    let program = argv.iter().take(2).position(|a| is_claude(a))?;
    let rest = &argv[program + 1..];
    let at = rest.iter().position(|a| a == "attach")?;
    rest[at + 1..]
        .iter()
        .find(|a| !a.starts_with('-') && !a.is_empty())
        .cloned()
}

fn is_claude(arg: &str) -> bool {
    let name = arg.rsplit(['/', '\\']).next().unwrap_or(arg);
    name == "claude" || name == "claude.exe" || (name == "cli.js" && arg.contains("claude-code"))
}

/// The job `claude attach <target>` attaches to, as Claude Code reads the
/// argument: the job's short id, its conversation id (or the start of
/// either), or its name — part of it does. Of several, a running one, then
/// the most recently active.
pub fn resolve<'a>(target: &str, jobs: &'a [Job]) -> Option<&'a Job> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    let lower = target.to_lowercase();
    let tiers: [&dyn Fn(&Job) -> bool; 5] = [
        &|j| j.id == target,
        &|j| sids(j).any(|s| s == target),
        &|j| j.id.starts_with(target) || sids(j).any(|s| s.starts_with(target)),
        &|j| j.name == target,
        &|j| j.name.to_lowercase().contains(&lower),
    ];
    tiers.iter().find_map(|matches| {
        jobs.iter()
            .filter(|j| matches(j))
            .max_by_key(|j| (j.live, j.updated_at_ms))
    })
}

/// A job's conversation ids: its own, and the one it resumes.
fn sids(j: &Job) -> impl Iterator<Item = &str> {
    [j.session_id.as_deref(), j.resume_session_id.as_deref()]
        .into_iter()
        .flatten()
}

/// What `claude attach` under process `shell` targets, if one runs there.
///
/// Walks `shell`'s descendants only, and not into a claude: what a claude
/// runs (tools, MCP servers) is not the tab's. A few files per process, on
/// the session scan's thread.
#[cfg(target_os = "linux")]
pub fn under(shell: u32) -> Option<String> {
    let mut by_parent: Option<std::collections::HashMap<u32, Vec<u32>>> = None;
    let mut queue = vec![(shell, 0u8)];
    let mut seen = 0;
    while let Some((pid, depth)) = queue.pop() {
        seen += 1;
        if seen > 256 {
            return None;
        }
        if pid != shell {
            let argv = cmdline(pid);
            if argv.iter().take(2).any(|a| is_claude(a)) {
                // One stopped by job control (Ctrl+Z) shows nothing.
                if let Some(t) = target(&argv).filter(|_| !stopped(pid)) {
                    return Some(t);
                }
                continue;
            }
        }
        if depth >= 6 {
            continue;
        }
        let kids = children(pid).unwrap_or_else(|| {
            by_parent
                .get_or_insert_with(parents)
                .get(&pid)
                .cloned()
                .unwrap_or_default()
        });
        queue.extend(kids.into_iter().map(|k| (k, depth + 1)));
    }
    None
}

/// Elsewhere there is no `/proc` to read; the agents pane is WSL-only.
#[cfg(not(target_os = "linux"))]
pub fn under(_shell: u32) -> Option<String> {
    None
}

/// Is `pid` stopped (`T`, or `t` under a tracer)?
#[cfg(target_os = "linux")]
fn stopped(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|s| s == "T" || s == "t")
    })
}

#[cfg(target_os = "linux")]
fn cmdline(pid: u32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|raw| {
            raw.split(|&b| b == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// `pid`'s children, from every thread's `children` file; `None` on a
/// kernel without them.
#[cfg(target_os = "linux")]
fn children(pid: u32) -> Option<Vec<u32>> {
    let tasks = std::fs::read_dir(format!("/proc/{pid}/task")).ok()?;
    let mut out = Vec::new();
    for task in tasks.flatten() {
        let text = std::fs::read_to_string(task.path().join("children")).ok()?;
        out.extend(
            text.split_whitespace()
                .filter_map(|p| p.parse::<u32>().ok()),
        );
    }
    Some(out)
}

/// Every process's children, from each one's parent: the slow way, for a
/// kernel with no `children` files.
#[cfg(target_os = "linux")]
fn parents() -> std::collections::HashMap<u32, Vec<u32>> {
    let mut out: std::collections::HashMap<u32, Vec<u32>> = Default::default();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in dir.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let ppid = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1)?.parse::<u32>().ok());
        if let Some(ppid) = ppid {
            out.entry(ppid).or_default().push(pid);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::JobState;

    fn argv(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn reads_the_attach_target() {
        assert_eq!(
            target(&argv("claude attach 34c55b2c")).as_deref(),
            Some("34c55b2c")
        );
        assert_eq!(
            target(&argv(
                "/home/x/.nvm/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe attach 34c55b2c"
            ))
            .as_deref(),
            Some("34c55b2c"),
            "the re-exec a resume becomes"
        );
        assert_eq!(
            target(&argv("node /x/claude-code/cli.js attach --verbose Open")).as_deref(),
            Some("Open")
        );
        assert_eq!(target(&argv("claude --resume 34c55b2c")), None);
        assert_eq!(target(&argv("claude attach")), None);
        assert_eq!(target(&argv("vim attach 34c55b2c")), None);
        assert_eq!(target(&argv("claude -p attach")), None);
    }

    fn job(id: &str, name: &str, sid: &str, live: bool, at: u64) -> Job {
        Job {
            id: id.into(),
            name: name.into(),
            state: JobState::Working,
            detail: None,
            tasks: 0,
            queued: 0,
            cwd: None,
            session_id: Some(sid.into()),
            resume_session_id: None,
            updated_at_ms: at,
            config_dir: "/c".into(),
            live,
            forked_from: None,
            untouched: false,
            pinned: false,
        }
    }

    #[test]
    fn resolves_an_id_a_conversation_or_a_name() {
        let jobs = [
            job("34c55b2c", "Open bugs in panel", "aaaa-1111", true, 5),
            job("970bf052", "Open bugs in panel (2)", "bbbb-2222", true, 9),
            job("6e7e56e0", "count rust lines", "cccc-3333", false, 1),
        ];
        let id = |t: &str| resolve(t, &jobs).map(|j| j.id.as_str());
        assert_eq!(id("34c55b2c"), Some("34c55b2c"));
        assert_eq!(id("bbbb-2222"), Some("970bf052"), "the conversation");
        assert_eq!(id("6e7e"), Some("6e7e56e0"), "the start of an id");
        assert_eq!(id("cccc"), Some("6e7e56e0"), "the start of a conversation");
        assert_eq!(id("Open bugs in panel"), Some("34c55b2c"), "the whole name");
        assert_eq!(id("rust"), Some("6e7e56e0"), "part of the name");
        assert_eq!(id("open bugs"), Some("970bf052"), "of two, the later");
        assert_eq!(id("nothing"), None);
        assert_eq!(id(""), None);
    }

    /// A `claude attach` under a shell is found from the shell's pid.
    #[cfg(target_os = "linux")]
    #[test]
    fn finds_an_attach_under_a_shell() {
        // A stand-in named `claude`, run as `sh <dir>/claude attach …`
        // under a shell that stays its parent, as a tab's shell does.
        let dir = std::env::temp_dir().join(format!("giverny-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("claude");
        std::fs::write(&fake, "sleep 5\n").unwrap();
        let mut shell = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("sh {} attach 6e7e56e0; true", fake.display()))
            .spawn()
            .unwrap();
        let mut found = None;
        for _ in 0..50 {
            found = under(shell.id());
            if found.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = shell.kill();
        let _ = shell.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found.as_deref(), Some("6e7e56e0"));
        assert_eq!(under(std::process::id()), None, "none under the test");
    }
}

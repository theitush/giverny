//! Shell history per tab (#213).
//!
//! Each tab's shell keeps its own history file, named by the tab's stable
//! [`TabId`] and kept beside its restore snapshot, so a restored tab's ↑
//! gives that tab's own last commands rather than whatever any shell on the
//! machine ran last.
//!
//! Done only through the environment Giverny hands the shells it spawns: no
//! rc file is written, and a shell started anywhere else is untouched. That
//! is also the limit of it — an rc file that sets `HISTFILE` itself wins, and
//! the tab simply has the user's usual history, as before.
//!
//! - **bash**: `HISTFILE`, plus `history -a` on every prompt (appended to any
//!   inherited `PROMPT_COMMAND`), so a killed tab keeps what it ran. With
//!   `also_shared`, each new entry is appended to `~/.bash_history` too.
//!   The prompt hook checks `HISTFILE` is still the tab's, so it writes
//!   nothing when an rc has pointed it elsewhere. An rc that *replaces*
//!   `PROMPT_COMMAND` drops the hook: the file is then written on exit only.
//! - **zsh**: `HISTFILE` (and `SAVEHIST`/`HISTSIZE` where nothing set them,
//!   since zsh saves nothing by default). Written when the shell exits:
//!   per-command writes are a `setopt`, which no environment variable can
//!   reach. `also_shared` does not apply.
//! - **fish**: `fish_history` names a session of its own; fish writes every
//!   command as it runs. `also_shared` does not apply.
//! - Anything else (sh, PowerShell, cmd, a WSL shell): left alone.

use std::path::{Path, PathBuf};

use crate::state::Paths;
use crate::tabs::TabId;

/// The shells whose history can be steered from the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    Zsh,
    Fish,
}

impl ShellKind {
    /// Which shell a program path runs, by its file name; `None` for one this
    /// does not know how to steer.
    pub fn of(program: &str) -> Option<ShellKind> {
        let name = Path::new(program)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(program);
        let name = name.strip_suffix(".exe").unwrap_or(name);
        match name {
            "bash" => Some(ShellKind::Bash),
            "zsh" => Some(ShellKind::Zsh),
            "fish" => Some(ShellKind::Fish),
            _ => None,
        }
    }
}

/// Where one tab's history lives: `state/history/<tab>`, beside its snapshot.
pub fn history_file(paths: &Paths, tab: TabId) -> PathBuf {
    paths
        .base()
        .join("state")
        .join("history")
        .join(tab.0.to_string())
}

/// fish keeps history by session name, in its own data directory.
fn fish_session(tab: TabId) -> String {
    format!("giverny_{}", tab.0)
}

/// The file fish writes for a tab's session.
fn fish_file(tab: TabId) -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))?;
    Some(
        data.join("fish")
            .join(format!("{}_history", fish_session(tab))),
    )
}

/// The prompt hook bash runs: write this tab's new entries now, not at exit.
/// Guarded on `HISTFILE` so an rc that moved it elsewhere gets nothing extra.
const BASH_HOOK: &str = r#"[ "$HISTFILE" = "$GIVERNY_HISTFILE" ] && history -a"#;

/// The same, also appending the new entries — exactly as bash wrote them,
/// timestamps included — to the shell's usual history file.
const BASH_HOOK_SHARED: &str = r#"if [ "$HISTFILE" = "$GIVERNY_HISTFILE" ]; then __giverny_hn=$(wc -l 2>/dev/null <"$HISTFILE"); history -a; tail -n "+$((__giverny_hn+1))" "$HISTFILE" >>"$GIVERNY_HISTFILE_SHARED" 2>/dev/null; fi"#;

/// Environment that gives a tab's shell its own history.
///
/// `inherited` looks up a variable in the environment the shell would
/// otherwise get — an inherited `PROMPT_COMMAND` is kept, run first.
pub fn env(
    kind: ShellKind,
    file: &Path,
    tab: TabId,
    also_shared: bool,
    home: Option<&Path>,
    inherited: impl Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let file = file.display().to_string();
    match kind {
        ShellKind::Bash => {
            let shared = also_shared.then(|| home.map(|h| h.join(".bash_history")));
            let hook = match &shared {
                Some(Some(_)) => BASH_HOOK_SHARED,
                _ => BASH_HOOK,
            };
            let prompt_command = match inherited("PROMPT_COMMAND").filter(|p| !p.trim().is_empty())
            {
                // A newline, not `;`: the inherited one may end in either.
                Some(prev) => format!("{prev}\n{hook}"),
                None => hook.to_string(),
            };
            let mut env = vec![
                ("HISTFILE".to_string(), file.clone()),
                ("GIVERNY_HISTFILE".to_string(), file),
                ("PROMPT_COMMAND".to_string(), prompt_command),
            ];
            if let Some(Some(shared)) = shared {
                env.push((
                    "GIVERNY_HISTFILE_SHARED".to_string(),
                    shared.display().to_string(),
                ));
            }
            env
        }
        ShellKind::Zsh => {
            let mut env = vec![("HISTFILE".to_string(), file)];
            for var in ["HISTSIZE", "SAVEHIST"] {
                if inherited(var).is_none() {
                    env.push((var.to_string(), "10000".to_string()));
                }
            }
            env
        }
        ShellKind::Fish => vec![("fish_history".to_string(), fish_session(tab))],
    }
}

/// A tab closed for good takes its history with it.
pub fn remove(paths: &Paths, tab: TabId) {
    let _ = std::fs::remove_file(history_file(paths, tab));
    if let Some(fish) = fish_file(tab) {
        let _ = std::fs::remove_file(fish);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn shells_are_known_by_file_name() {
        assert_eq!(ShellKind::of("/bin/bash"), Some(ShellKind::Bash));
        assert_eq!(ShellKind::of("/usr/bin/zsh"), Some(ShellKind::Zsh));
        assert_eq!(ShellKind::of("/usr/local/bin/fish"), Some(ShellKind::Fish));
        assert_eq!(ShellKind::of("bash"), Some(ShellKind::Bash));
        assert_eq!(ShellKind::of("/bin/sh"), None);
        assert_eq!(ShellKind::of("pwsh.exe"), None);
        assert_eq!(ShellKind::of("wsl.exe"), None);
    }

    #[test]
    fn each_tab_has_its_own_file_beside_the_snapshots() {
        let paths = Paths::at("/cfg/giverny");
        assert_eq!(
            history_file(&paths, TabId(7)),
            PathBuf::from("/cfg/giverny/state/history/7")
        );
        assert_ne!(
            history_file(&paths, TabId(7)),
            history_file(&paths, TabId(8))
        );
    }

    /// The hooks, run by a real interactive bash.
    #[cfg(unix)]
    #[test]
    fn bash_writes_per_command_and_shares_only_when_asked() {
        if !Path::new("/bin/bash").exists() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("giverny-hist-bash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |also_shared: bool, file: &Path| {
            let env = env(
                ShellKind::Bash,
                file,
                TabId(1),
                also_shared,
                Some(&dir),
                |_| None,
            );
            let mut cmd = std::process::Command::new("/bin/bash");
            cmd.args(["--norc", "--noprofile", "-i"])
                .env_clear()
                .env("HOME", &dir)
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .envs(env)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            let mut child = cmd.spawn().unwrap();
            {
                use std::io::Write;
                let stdin = child.stdin.as_mut().unwrap();
                // Killed, not exited: what is in the file got there per command.
                stdin
                    .write_all(b"echo one\necho two\nkill -9 $$\n")
                    .unwrap();
            }
            let _ = child.wait();
        };
        let shared = dir.join(".bash_history");
        std::fs::write(&shared, "older\n").unwrap();

        let a = dir.join("a");
        run(false, &a);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "echo one\necho two\n");
        assert_eq!(std::fs::read_to_string(&shared).unwrap(), "older\n");

        let b = dir.join("b");
        run(true, &b);
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "echo one\necho two\n");
        assert_eq!(
            std::fs::read_to_string(&shared).unwrap(),
            "older\necho one\necho two\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bash_gets_a_file_and_a_prompt_hook() {
        let f = Path::new("/s/history/3");
        let env = env(
            ShellKind::Bash,
            f,
            TabId(3),
            false,
            Some(Path::new("/h")),
            |_| None,
        );
        assert_eq!(get(&env, "HISTFILE"), Some("/s/history/3"));
        assert_eq!(get(&env, "GIVERNY_HISTFILE"), Some("/s/history/3"));
        assert_eq!(get(&env, "PROMPT_COMMAND"), Some(BASH_HOOK));
        assert_eq!(get(&env, "GIVERNY_HISTFILE_SHARED"), None, "off by default");
    }

    #[test]
    fn bash_shared_appends_to_the_usual_file() {
        let f = Path::new("/s/history/3");
        let env = env(
            ShellKind::Bash,
            f,
            TabId(3),
            true,
            Some(Path::new("/h")),
            |_| None,
        );
        assert_eq!(get(&env, "PROMPT_COMMAND"), Some(BASH_HOOK_SHARED));
        assert_eq!(
            get(&env, "GIVERNY_HISTFILE_SHARED"),
            Some("/h/.bash_history")
        );
        // Append only: the usual file is never truncated or rewritten.
        assert!(BASH_HOOK_SHARED.contains(r#">>"$GIVERNY_HISTFILE_SHARED""#));
        assert!(!BASH_HOOK_SHARED.contains(r#" >"$GIVERNY_HISTFILE_SHARED""#));
    }

    #[test]
    fn an_inherited_prompt_command_is_kept_and_runs_first() {
        let f = Path::new("/s/history/3");
        let env = env(ShellKind::Bash, f, TabId(3), false, None, |k| {
            (k == "PROMPT_COMMAND").then(|| "echo hi;".to_string())
        });
        assert_eq!(
            get(&env, "PROMPT_COMMAND"),
            Some(format!("echo hi;\n{BASH_HOOK}").as_str())
        );
    }

    #[test]
    fn zsh_gets_a_file_and_sizes_only_where_unset() {
        let f = Path::new("/s/history/3");
        let env = env(ShellKind::Zsh, f, TabId(3), true, None, |k| {
            (k == "SAVEHIST").then(|| "500".to_string())
        });
        assert_eq!(get(&env, "HISTFILE"), Some("/s/history/3"));
        assert_eq!(get(&env, "HISTSIZE"), Some("10000"));
        assert_eq!(get(&env, "SAVEHIST"), None, "the user's own value stands");
        assert_eq!(get(&env, "PROMPT_COMMAND"), None);
    }

    #[test]
    fn fish_gets_a_session_name_it_accepts() {
        let env = env(
            ShellKind::Fish,
            Path::new("/x"),
            TabId(42),
            false,
            None,
            |_| None,
        );
        assert_eq!(env, vec![("fish_history".into(), "giverny_42".into())]);
        assert!(
            fish_session(TabId(42))
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        );
        assert_eq!(get(&env, "HISTFILE"), None);
    }

    #[test]
    fn remove_takes_the_file() {
        let dir = std::env::temp_dir().join(format!("giverny-hist-{}", std::process::id()));
        let paths = Paths::at(&dir);
        let f = history_file(&paths, TabId(5));
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "ls\n").unwrap();
        remove(&paths, TabId(5));
        assert!(!f.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

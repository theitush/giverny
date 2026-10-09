//! The plugin's `PreToolUse` Bash guard: a command that would take work
//! out of its tab's resource cap is refused (giverny#262).
//!
//! Every claude in a Giverny tab runs in a capped scope ([`crate::tab_cap`])
//! and everything it starts inherits that cap — unless the command asks
//! systemd or the kernel to put it somewhere else. `systemd-run --user`
//! makes a scope of its own outside every cap; `systemctl --user
//! set-property` lifts a cap; a write into `/sys/fs/cgroup` moves a process
//! or changes a limit (the user owns their manager's cgroup tree);
//! `cgexec`, `taskset`, `chrt`, a negative `nice`/`renice` and a
//! realtime `ionice` reach for more than they were given. Those are
//! refused, with the way to get more said: `giverny-manage claim` and
//! `giverny-manage run`. A command that goes through `giverny manage run`
//! passes — what it runs is checked the same way, since a scope started
//! from inside a run's scope would leave it as well.
//!
//! **Cheap.** It runs before every Bash call: one pass over the command's
//! text, no file read, no process started. It reads the shell the way a
//! person would (quotes, `;`/`&&`/`|`, `$(…)` and backticks, `sh -c '…'`,
//! `env`/`sudo`/`timeout`/`xargs`/`nice` in front), not as bash does: a
//! command built at run time (`$(echo c3lz… | base64 -d)`), one a script
//! file holds, or one a program starts on its own is not seen. It is a
//! guard against forgetting, not against a determined escape.

use crate::tab_cap::SLICE;

/// Why `cmd` is refused, or `None` when it may run.
pub fn verdict(cmd: &str) -> Option<String> {
    check_text(cmd, 0).map(|what| refusal(&what))
}

/// The message Claude is shown when a command is refused.
pub fn refusal(what: &str) -> String {
    format!(
        "Giverny refused this command: {what} would take work out of this tab's \
         resource cap. Every claude in a Giverny tab runs capped (in {SLICE}), and \
         all it starts with it. For more, ask for it: `giverny-manage claim <task> \
         --cpu <n> --ram <size>`, then run the heavy command as `giverny-manage run \
         <task> -- <command>`, which gives it a capped scope of its own inside the \
         machine's budget."
    )
}

/// How deep `sh -c '…'` inside `sh -c '…'` is followed.
const MAX_DEPTH: usize = 8;

fn check_text(cmd: &str, depth: usize) -> Option<String> {
    if depth > MAX_DEPTH {
        return None;
    }
    let parsed = split(cmd);
    for nested in &parsed.nested {
        if let Some(w) = check_text(nested, depth + 1) {
            return Some(w);
        }
    }
    for seg in &parsed.segments {
        if let Some(t) = seg.redirects.iter().find(|t| t.contains(CGROUP_FS)) {
            return Some(format!("writing to `{t}`"));
        }
        if let Some(w) = check_words(&seg.words, depth) {
            return Some(w);
        }
    }
    None
}

const CGROUP_FS: &str = "/sys/fs/cgroup";

/// One simple command: its words, and where its output is redirected.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Segment {
    words: Vec<String>,
    redirects: Vec<String>,
}

#[derive(Debug, Default)]
struct Parsed {
    segments: Vec<Segment>,
    /// The text of each `$(…)` or backtick substitution met inside double
    /// quotes, checked as commands of their own.
    nested: Vec<String>,
}

/// The command's simple commands, split the way a shell would at `;`,
/// `&`, `|`, newlines, parentheses and backticks, with quotes removed.
fn split(cmd: &str) -> Parsed {
    let mut out = Parsed::default();
    let mut seg = Segment::default();
    let mut word = String::new();
    // A word is open (even an empty one, as `''` is).
    let mut open = false;
    // The next word is a redirect's target: `Some(true)` for output.
    let mut redirect: Option<bool> = None;
    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;

    let end_word =
        |word: &mut String, open: &mut bool, seg: &mut Segment, redirect: &mut Option<bool>| {
            if *open {
                let w = std::mem::take(word);
                match redirect.take() {
                    Some(true) => seg.redirects.push(w),
                    Some(false) => {}
                    None => seg.words.push(w),
                }
                *open = false;
            }
        };
    let end_seg = |seg: &mut Segment, out: &mut Parsed| {
        let s = std::mem::take(seg);
        if !s.words.is_empty() || !s.redirects.is_empty() {
            out.segments.push(s);
        }
    };

    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' => end_word(&mut word, &mut open, &mut seg, &mut redirect),
            ';' | '\n' | '&' | '|' | '(' | ')' | '`' => {
                end_word(&mut word, &mut open, &mut seg, &mut redirect);
                redirect = None;
                end_seg(&mut seg, &mut out);
            }
            '$' if chars.get(i + 1) == Some(&'(') => {
                end_word(&mut word, &mut open, &mut seg, &mut redirect);
                redirect = None;
                end_seg(&mut seg, &mut out);
                i += 1;
            }
            '#' if !open => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '>' | '<' => {
                // `2>`, `&>`: the number is the stream, not a word.
                if open && word.chars().all(|c| c.is_ascii_digit()) {
                    word.clear();
                    open = false;
                }
                end_word(&mut word, &mut open, &mut seg, &mut redirect);
                let output = c == '>';
                while matches!(chars.get(i + 1), Some('>' | '&' | '|')) {
                    i += 1;
                }
                // `<<EOF`, `<<<word`: nothing written.
                while c == '<' && matches!(chars.get(i + 1), Some('<' | '-')) {
                    i += 1;
                }
                redirect = Some(output);
            }
            '\\' => {
                if let Some(&n) = chars.get(i + 1) {
                    if n != '\n' {
                        word.push(n);
                        open = true;
                    }
                    i += 1;
                }
            }
            '\'' => {
                open = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    word.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                open = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    match chars[i] {
                        '\\' if matches!(chars.get(i + 1), Some('"' | '\\' | '$' | '`')) => {
                            word.push(chars[i + 1]);
                            i += 1;
                        }
                        '$' if chars.get(i + 1) == Some(&'(') => {
                            let (inner, next) = until_close(&chars, i + 2, '(', ')');
                            out.nested.push(inner);
                            i = next;
                        }
                        '`' => {
                            let (inner, next) = until_close(&chars, i + 1, '\0', '`');
                            out.nested.push(inner);
                            i = next;
                        }
                        ch => word.push(ch),
                    }
                    i += 1;
                }
            }
            ch => {
                word.push(ch);
                open = true;
            }
        }
        i += 1;
    }
    end_word(&mut word, &mut open, &mut seg, &mut redirect);
    end_seg(&mut seg, &mut out);
    out
}

/// The text from `from` up to the `close` that ends it (nested `open`s
/// counted), and the index of that `close`.
fn until_close(chars: &[char], from: usize, open: char, close: char) -> (String, usize) {
    let mut depth = 0usize;
    let mut i = from;
    let mut s = String::new();
    while i < chars.len() {
        let c = chars[i];
        if c == close && depth == 0 {
            return (s, i);
        }
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
        }
        s.push(c);
        i += 1;
    }
    (s, i)
}

/// Programs that only read what they are given: a path under
/// `/sys/fs/cgroup` in their arguments is a look, not a change.
const READERS: &[&str] = &[
    "cat",
    "head",
    "tail",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ls",
    "find",
    "stat",
    "wc",
    "less",
    "more",
    "file",
    "readlink",
    "realpath",
    "test",
    "[",
    "echo",
    "printf",
    "du",
    "diff",
    "cut",
    "sort",
    "uniq",
    "column",
    "xxd",
    "od",
    "systemd-cgls",
    "systemd-cgtop",
    "basename",
    "dirname",
    "tree",
    "jq",
];

/// Words that only lead into the command after them.
const LEADERS: &[&str] = &[
    "exec", "command", "builtin", "time", "nohup", "setsid", "then", "do", "else", "elif", "if",
    "while", "until", "!", "{", "}", "unbuffer", "chronic",
];

fn base(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(k, _)| {
        !k.is_empty()
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !k.starts_with(|c: char| c.is_ascii_digit())
    })
}

/// A negative niceness among `nice`'s options: `-n -5`, `-n-5`,
/// `--adjustment=-5`, `--5`.
fn negative_nice(args: &[String]) -> bool {
    let neg = |v: &str| v.trim().parse::<i64>().is_ok_and(|n| n < 0);
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-n" || a == "--adjustment" {
            if args.get(i + 1).is_some_and(|v| neg(v)) {
                return true;
            }
            i += 2;
            continue;
        }
        if let Some(v) = a
            .strip_prefix("--adjustment=")
            .or_else(|| a.strip_prefix("-n"))
            && neg(v)
        {
            return true;
        }
        // `nice --5 cmd` is an adjustment of -5.
        if a.starts_with("--") && a[2..].parse::<u64>().is_ok() {
            return true;
        }
        i += 1;
    }
    false
}

/// Where the command after `nice`'s options starts.
fn after_nice(args: &[String]) -> usize {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-n" || a == "--adjustment" {
            i += 2;
        } else if a.starts_with('-') {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// `renice`'s new priority, when it is negative: `-n -5`, `-5`, `--priority -5`.
fn negative_renice(args: &[String]) -> bool {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if matches!(a, "-n" | "--priority" | "--relative") {
            return args
                .get(i + 1)
                .is_some_and(|v| v.parse::<i64>().is_ok_and(|n| n < 0));
        }
        if let Ok(n) = a.parse::<i64>() {
            return n < 0;
        }
        i += 1;
    }
    false
}

/// `ionice -c 1` (realtime).
fn realtime_ionice(args: &[String]) -> bool {
    let rt = |v: &str| matches!(v.trim(), "1" | "realtime");
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-c" || a == "--class" {
            if args.get(i + 1).is_some_and(|v| rt(v)) {
                return true;
            }
            i += 2;
            continue;
        }
        if let Some(v) = a.strip_prefix("-c").or_else(|| a.strip_prefix("--class="))
            && rt(v)
        {
            return true;
        }
        i += 1;
    }
    false
}

/// Skip `prog`'s options (those in `with_value` take the next word) and
/// return where its command starts.
fn after_options(args: &[String], with_value: &[&str]) -> usize {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            return i + 1;
        }
        if !a.starts_with('-') || a == "-" {
            break;
        }
        i += if with_value.contains(&a) { 2 } else { 1 };
    }
    i
}

fn check_words(words: &[String], depth: usize) -> Option<String> {
    let mut i = 0;
    while i < words.len() && is_assignment(&words[i]) {
        i += 1;
    }
    let words = &words[i..];
    let first = words.first()?;
    let prog = base(first);
    let args = &words[1..];
    let rest = |from: usize| check_words(args.get(from..).unwrap_or_default(), depth);

    if LEADERS.contains(&prog) {
        return rest(0);
    }
    match prog {
        "sudo" | "doas" => return rest(after_options(args, &["-u", "-g", "-C", "-p", "-D"])),
        "env" => {
            let mut j = 0;
            while j < args.len() {
                let a = args[j].as_str();
                if a == "-S" || a == "--split-string" {
                    let inner = args.get(j + 1).cloned().unwrap_or_default();
                    let tail = args.get(j + 2..).unwrap_or_default().join(" ");
                    return check_text(&format!("{inner} {tail}"), depth + 1);
                }
                if matches!(a, "-u" | "--unset" | "-C" | "--chdir") {
                    j += 2;
                } else if a.starts_with('-') || is_assignment(a) {
                    j += 1;
                } else {
                    break;
                }
            }
            return rest(j);
        }
        "timeout" => {
            let j = after_options(args, &["-s", "--signal", "-k", "--kill-after"]);
            return rest(j + 1); // the duration
        }
        "xargs" => {
            return rest(after_options(
                args,
                &["-I", "-n", "-P", "-L", "-s", "-d", "-E", "-a"],
            ));
        }
        "stdbuf" | "watch" => return rest(after_options(args, &["-n", "-i", "-o", "-e"])),
        "flock" => {
            let j = after_options(args, &["-w", "--timeout", "-E", "--conflict-exit-code"]);
            // `flock FILE -c 'cmd'` or `flock FILE cmd …`.
            if args
                .get(j + 1)
                .is_some_and(|a| a == "-c" || a == "--command")
            {
                return check_text(args.get(j + 2).map_or("", String::as_str), depth + 1);
            }
            return rest(j + 1);
        }
        "nice" => {
            if negative_nice(args) {
                return Some("`nice` with a negative adjustment".into());
            }
            return rest(after_nice(args));
        }
        "renice" => {
            if negative_renice(args) {
                return Some("`renice` to a higher priority".into());
            }
            return None;
        }
        "ionice" => {
            if realtime_ionice(args) {
                return Some("`ionice` realtime".into());
            }
            if args.iter().any(|a| a == "-p" || a == "-P" || a == "-u") {
                return None;
            }
            return rest(after_options(args, &["-c", "-n", "--class", "--classdata"]));
        }
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish" => {
            let mut j = 0;
            let mut script = false;
            while j < args.len() && args[j].starts_with('-') && args[j] != "-" {
                if !args[j].starts_with("--") && args[j][1..].contains('c') {
                    script = true;
                }
                j += 1;
            }
            return if script {
                check_text(args.get(j).map_or("", String::as_str), depth + 1)
            } else {
                None // a script file: not seen
            };
        }
        "eval" => return check_text(&args.join(" "), depth + 1),
        "giverny-manage" => return through_run(args, depth),
        "giverny" if args.first().is_some_and(|a| a == "manage") => {
            return through_run(&args[1..], depth);
        }
        _ => {}
    }

    match prog {
        "systemd-run" => {
            if args
                .iter()
                .all(|a| matches!(a.as_str(), "--help" | "-h" | "--version"))
            {
                return None;
            }
            return Some("`systemd-run` (a scope or service of its own)".into());
        }
        "systemctl" => {
            const LIFTS: &[&str] = &[
                "set-property",
                "revert",
                "edit",
                "start",
                "restart",
                "try-restart",
                "reload-or-restart",
                "try-reload-or-restart",
                "isolate",
                "link",
                "daemon-reexec",
            ];
            let verb = args.iter().find(|a| !a.starts_with('-'));
            if let Some(v) = verb
                && LIFTS.contains(&v.as_str())
            {
                return Some(format!("`systemctl {v}`"));
            }
            if verb.is_some_and(|v| v == "enable" || v == "reenable")
                && args.iter().any(|a| a == "--now")
            {
                return Some("`systemctl enable --now`".into());
            }
        }
        "cgexec" | "cgclassify" | "cgcreate" | "cgset" | "cgdelete" => {
            return Some(format!("`{prog}`"));
        }
        "taskset" => {
            // `taskset -p PID` only reads the affinity.
            let reads = args.len() == 2 && args[0] == "-p" && args[1].parse::<u32>().is_ok();
            if !reads {
                return Some("`taskset` (CPU affinity)".into());
            }
        }
        "chrt" | "numactl" => return Some(format!("`{prog}` (scheduling)")),
        "busctl" | "dbus-send" | "gdbus" => {
            const CALLS: &[&str] = &[
                "StartTransientUnit",
                "AttachProcessesToUnit",
                "SetUnitProperties",
                "StartUnit",
                "RestartUnit",
            ];
            if let Some(c) = CALLS.iter().find(|c| args.iter().any(|a| a.contains(*c))) {
                return Some(format!("a systemd `{c}` call"));
            }
        }
        _ => {}
    }

    if let Some(p) = args.iter().find(|a| a.contains(CGROUP_FS)) {
        let reads = READERS.contains(&prog)
            || (prog == "sed"
                && !args
                    .iter()
                    .any(|a| a.starts_with("-i") || a == "--in-place"));
        if !reads {
            return Some(format!("`{prog}` on `{p}`"));
        }
    }
    None
}

/// `giverny manage <sub> …`: a `run` passes, and what it runs is checked.
fn through_run(args: &[String], depth: usize) -> Option<String> {
    if args.first().is_none_or(|a| a != "run") {
        return None;
    }
    let dash = args.iter().position(|a| a == "--")?;
    check_words(&args[dash + 1..], depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(cmd: &str) -> bool {
        verdict(cmd).is_some()
    }

    #[test]
    fn a_scope_of_its_own_is_refused() {
        for cmd in [
            "systemd-run --user --scope -p MemoryMax=10G -- cargo build --release",
            "systemd-run --user --scope cargo build",
            "/usr/bin/systemd-run --user --unit x sleep 1",
            "cd /x && systemd-run --user --scope make",
            "FOO=1 systemd-run --user --scope make",
            "env CARGO_TARGET_DIR=/t systemd-run --user --scope cargo test",
            "sudo systemd-run --scope make",
            "nohup systemd-run --user --scope make &",
            "timeout 600 systemd-run --user --scope make",
            "echo go; systemd-run --user --scope make",
            "true && systemd-run --user --scope make",
            "x=$(systemd-run --user --scope make)",
            "echo \"$(systemd-run --user --scope make)\"",
            "echo `systemd-run --user --scope make`",
            "bash -c 'systemd-run --user --scope make'",
            "sh -lc \"cd /x && systemd-run --user --scope make\"",
            "eval systemd-run --user --scope make",
            "(systemd-run --user --scope make)",
            "ls | xargs -n1 systemd-run --user --scope",
        ] {
            assert!(refused(cmd), "{cmd}");
        }
    }

    #[test]
    fn lifting_a_cap_or_reaching_for_more_is_refused() {
        for cmd in [
            "systemctl --user set-property giverny-claude.slice CPUQuota=",
            "systemctl --user revert giverny-claude.slice",
            "systemctl --user start my-build.service",
            "systemctl --user enable --now my-build.service",
            "cgexec -g cpu:/ make",
            "taskset -c 0-13 make",
            "taskset -p 0xff 1234",
            "chrt -f 10 make",
            "nice -n -5 make",
            "nice --adjustment=-5 make",
            "nice --5 make",
            "renice -n -5 -p 1234",
            "renice -5 1234",
            "ionice -c 1 make",
            "echo 1234 > /sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/cgroup.procs",
            "echo max >> /sys/fs/cgroup/x/cpu.max",
            "echo 1234 | tee /sys/fs/cgroup/x/cgroup.procs",
            "sed -i s/3/9/ /sys/fs/cgroup/x/cpu.max",
            "busctl --user call org.freedesktop.systemd1 /org/freedesktop/systemd1 \
             org.freedesktop.systemd1.Manager StartTransientUnit ssa(sv)a(sa(sv)) x.scope fail 0 0",
            "giverny-manage run t -- systemd-run --user --scope make",
        ] {
            assert!(refused(cmd), "{cmd}");
        }
    }

    #[test]
    fn ordinary_and_capped_commands_pass() {
        for cmd in [
            "cargo build --release",
            "giverny-manage run 262-cgroup -- /home/ita/giverny/.claude/bin/cargo-shared test -p giverny-claude",
            "giverny manage run t -- cargo build",
            "giverny-manage claim t --cpu 6 --ram 8G",
            "git commit -m \"systemd-run is refused now\"",
            "grep -rn systemd-run crates/",
            "rg 'systemctl --user set-property' docs",
            "systemctl --user status giverny-claude.slice",
            "systemctl --user show giverny-claude.slice -p CPUQuotaPerSecUSec",
            "systemd-run --version",
            "cat /sys/fs/cgroup/user.slice/user-1000.slice/cpu.max",
            "cat /proc/self/cgroup; ls /sys/fs/cgroup/ > /tmp/x",
            "nice -n 19 cargo build",
            "nice cargo build",
            "renice -n 10 -p 1234",
            "ionice -c 3 cargo build",
            "taskset -p 1234",
            "echo 'systemd-run' # systemd-run",
            "cat <<'EOF'\nhello\nEOF",
            "",
        ] {
            assert!(!refused(cmd), "{cmd}: {:?}", verdict(cmd));
        }
    }

    #[test]
    fn the_refusal_says_what_and_where_to_go() {
        let r = verdict("systemd-run --user --scope make").unwrap();
        assert!(r.contains("`systemd-run`"), "{r}");
        assert!(r.contains("giverny-manage claim"), "{r}");
        assert!(r.contains("giverny-manage run"), "{r}");
    }

    #[test]
    fn the_shell_is_split_where_a_shell_splits_it() {
        let p = split("A=1 cat 'a b' \"c $(d e)\" > /out 2>&1 | wc -l; x\\ y");
        assert_eq!(p.segments.len(), 3);
        assert_eq!(p.segments[0].words, ["A=1", "cat", "a b", "c "]);
        assert_eq!(p.segments[0].redirects, ["/out", "1"]);
        assert_eq!(p.segments[1].words, ["wc", "-l"]);
        assert_eq!(p.segments[2].words, ["x y"]);
        assert_eq!(p.nested, ["d e"]);
    }
}

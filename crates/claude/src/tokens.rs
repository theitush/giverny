//! Token counts for the status line: `session <n>  ·  total: <n>`.
//!
//! The count is the one Claude Code's agents view prints beside a subagent —
//! `latestInputTokens`: input + cache creation + cache read of the **last**
//! API response in a transcript, assigned, never summed. A finished reply is
//! already inside the next request's input, so adding output on top would
//! count it twice. This is the same definition, rule for rule, as coo's
//! `orchestrate-status` (`tokens_of`, `usage_numbers`, `dispatcher_tokens`,
//! `fmt_tokens`; coo#103, coo#118), so the numbers did not move when the
//! segment moved here (giverny#22).
//!
//! Cheap by construction: the session's own count comes off the status line's
//! stdin when Claude Code hands it (`context_window.total_input_tokens`), and
//! a transcript is read **from its tail** — the last usage is near the end, so
//! a chunk is read backwards and doubled only while no usage has turned up.
//! Nothing is cached; a render reads a few tens of KB per subagent.

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// A `usage` object is a few hundred bytes; this is slack.
const USAGE_MAX: usize = 20_000;

/// First tail chunk read off a transcript; doubled until a usage is found.
const TAIL_START: u64 = 64 * 1024;

const SYNTHETIC: &[u8] = b"\"<synthetic>\"";

/// `(input, cache_creation, cache_read)` of one `usage` object.
///
/// Claude Code measures a turn that ran several API iterations by the last
/// real entry of `usage.iterations` (skipping `advisor_message` and
/// `compaction`), falling back to the top level on anything unexpected.
pub fn usage_numbers(u: &Value) -> (u64, u64, u64) {
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let base = (
        num(u, "input_tokens") as u64,
        num(u, "cache_creation_input_tokens") as u64,
        num(u, "cache_read_input_tokens") as u64,
    );
    let Some(its) = u.get("iterations").and_then(Value::as_array) else {
        return base;
    };
    if base.0 + base.1 + base.2 == 0 {
        return base;
    }
    let last = its.iter().rfind(|it| {
        it.is_object()
            && !matches!(
                it.get("type").and_then(Value::as_str),
                Some("advisor_message" | "compaction")
            )
    });
    let Some(last) = last else {
        return base;
    };
    if !matches!(
        last.get("type").and_then(Value::as_str),
        Some("message" | "fallback_message")
    ) {
        return base;
    }
    let mut got = [0u64; 3];
    for (slot, k) in got.iter_mut().zip([
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]) {
        match last.get(k).and_then(Value::as_f64) {
            Some(v) if v >= 0.0 => *slot = v as u64,
            _ => return base,
        }
    }
    // The output field must be a number too, as coo checks all four.
    if !last
        .get("output_tokens")
        .and_then(Value::as_f64)
        .is_some_and(|v| v >= 0.0)
    {
        return base;
    }
    if got.iter().sum::<u64>() == 0 {
        return base;
    }
    (got[0], got[1], got[2])
}

/// The context one transcript line sets, if it sets one: an assistant line
/// with a non-synthetic, non-zero `usage`.
fn line_tokens(line: &[u8]) -> Option<u64> {
    let i = find(line, b"\"usage\":")?;
    if find(&line[..i], b"\"assistant\"").is_none() || find(line, SYNTHETIC).is_some() {
        return None;
    }
    let k = i + line[i..].iter().position(|&c| c == b'{')?;
    let mut depth = 0i32;
    let end = line.len().min(k + USAGE_MAX);
    for j in k..end {
        match line[j] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    let u: Value = serde_json::from_slice(&line[k..=j]).ok()?;
                    if !u.is_object() {
                        return None;
                    }
                    let (a, b, c) = usage_numbers(&u);
                    return (a + b + c > 0).then_some(a + b + c);
                }
            }
            _ => {}
        }
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The last context a run of complete lines sets, scanning from the end.
fn last_in(buf: &[u8]) -> Option<u64> {
    buf.split(|&c| c == b'\n').rev().find_map(line_tokens)
}

/// What a transcript says it is carrying now, or `None`: the context of the
/// last API response in it. Never fails — an unreadable file is `None`.
pub fn tokens_of(path: &Path) -> Option<u64> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut want = TAIL_START;
    loop {
        let start = len.saturating_sub(want);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = Vec::with_capacity((len - start) as usize);
        (&mut file).take(len - start).read_to_end(&mut buf).ok()?;
        let whole = start == 0;
        let lines: &[u8] = if whole {
            &buf
        } else {
            // The first line is cut; it is read whole on the next, wider pass.
            match buf.iter().position(|&c| c == b'\n') {
                Some(i) => &buf[i + 1..],
                None => &[],
            }
        };
        if let Some(n) = last_in(lines) {
            return Some(n);
        }
        if whole {
            return None;
        }
        want = want.saturating_mul(2);
    }
}

/// The session's own count: `context_window.total_input_tokens` off the status
/// line's stdin when it is there (the same number, a turn fresher), else its
/// transcript.
pub fn session_tokens(payload: &Value, transcript: Option<&Path>) -> Option<u64> {
    let live = payload
        .get("context_window")
        .and_then(|c| c.get("total_input_tokens"))
        .and_then(Value::as_f64);
    if let Some(v) = live
        && v > 0.0
    {
        return Some(v as u64);
    }
    transcript.and_then(tokens_of)
}

/// Every `subagents/` dir of this session: beside its transcript, and under
/// any project dir of the config dir that holds the session id.
pub fn session_subagent_dirs(
    transcript: Option<&Path>,
    config_dir: Option<&Path>,
    session_id: Option<&str>,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if p.is_dir() && !out.contains(&p) {
            out.push(p);
        }
    };
    if let Some(t) = transcript {
        push(t.with_extension("").join("subagents"));
    }
    if let (Some(cfg), Some(sid)) = (config_dir, session_id.filter(|s| !s.is_empty()))
        && let Ok(entries) = std::fs::read_dir(cfg.join("projects"))
    {
        for e in entries.flatten() {
            push(e.path().join(sid).join("subagents"));
        }
    }
    out
}

/// Every subagent transcript in these dirs, each file counted once however
/// many dirs link to it.
pub fn subagent_transcripts(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        for id in crate::subagents::list_agent_ids(dir) {
            let p = crate::subagents::agent_transcript(dir, &id);
            let key = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
            if seen.insert(key) {
                out.push(p);
            }
        }
    }
    out
}

/// `(session, total)`: the session's count, and that plus every subagent's.
/// `None` for both when nothing could be counted.
pub fn session_and_total(
    session: Option<u64>,
    subagents: &[PathBuf],
) -> (Option<u64>, Option<u64>) {
    let workers: Vec<u64> = subagents.iter().filter_map(|p| tokens_of(p)).collect();
    let total = (session.is_some() || !workers.is_empty())
        .then(|| session.unwrap_or(0) + workers.iter().sum::<u64>());
    (session, total)
}

/// Claude Code's compact count — `842`, `13.5k`, `124.8k`, `1.2M` — with a
/// capital `M` so a total never reads as minutes.
pub fn fmt_tokens(n: u64) -> String {
    for (floor, div, suffix) in [
        (999_950_000u64, 1_000_000_000f64, "B"),
        (999_950, 1_000_000.0, "M"),
        (1000, 1000.0, "k"),
    ] {
        if n >= floor {
            let v = format!("{:.1}", n as f64 / div);
            let v = v.strip_suffix(".0").unwrap_or(&v);
            return format!("{v}{suffix}");
        }
    }
    n.to_string()
}

/// The status-line segments: `session <n>` and `total: <n>`, either left out
/// when it has no number.
pub fn segments(session: Option<u64>, total: Option<u64>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(s) = session {
        out.push(format!("session {}", fmt_tokens(s)));
    }
    if let Some(t) = total {
        out.push(format!("total: {}", fmt_tokens(t)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("giverny-tokens-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn asst(usage: Value) -> String {
        json!({"type": "assistant", "message": {"role": "assistant", "model": "claude-opus-5-5",
            "content": [{"type": "text", "text": "hi"}], "usage": usage}})
        .to_string()
    }

    #[test]
    fn the_last_response_is_assigned_not_summed_and_output_is_left_out() {
        let d = tmpdir("last");
        let p = d.join("s.jsonl");
        let lines = [
            json!({"type": "user", "message": {"content": "go"}}).to_string(),
            asst(
                json!({"input_tokens": 10, "cache_creation_input_tokens": 100,
                        "cache_read_input_tokens": 1000, "output_tokens": 5000}),
            ),
            asst(
                json!({"input_tokens": 2, "cache_creation_input_tokens": 300,
                        "cache_read_input_tokens": 2000, "output_tokens": 9}),
            ),
            // A trailing tool result, and a zero usage that says nothing.
            json!({"type": "user", "message": {"content": [{"type": "tool_result"}]}}).to_string(),
            asst(json!({"input_tokens": 0, "output_tokens": 0})),
        ];
        std::fs::write(&p, lines.join("\n") + "\n").unwrap();
        assert_eq!(tokens_of(&p), Some(2302));
    }

    #[test]
    fn synthetic_and_non_assistant_usages_are_skipped() {
        let d = tmpdir("synth");
        let p = d.join("s.jsonl");
        let synth =
            json!({"type": "assistant", "message": {"role": "assistant", "model": "<synthetic>",
            "usage": {"input_tokens": 99999}}})
            .to_string();
        // `"usage":` before any `"assistant"` on the line: not an assistant usage.
        let other = r#"{"usage":{"input_tokens":7777},"type":"assistant"}"#.to_string();
        let lines = [asst(json!({"input_tokens": 40})), synth, other];
        std::fs::write(&p, lines.join("\n")).unwrap();
        assert_eq!(tokens_of(&p), Some(40));
    }

    #[test]
    fn the_last_real_iteration_wins_over_the_top_level() {
        let u = json!({"input_tokens": 1, "cache_read_input_tokens": 1000,
            "iterations": [
                {"type": "message", "input_tokens": 5, "cache_creation_input_tokens": 0,
                 "cache_read_input_tokens": 50, "output_tokens": 3},
                {"type": "compaction", "input_tokens": 9000}]});
        assert_eq!(usage_numbers(&u), (5, 0, 50));
        // An unexpected last entry: the top level wins.
        let u = json!({"input_tokens": 1, "iterations": [{"type": "weird", "input_tokens": 5}]});
        assert_eq!(usage_numbers(&u), (1, 0, 0));
        // A missing field on the iteration: the top level wins.
        let u = json!({"input_tokens": 1, "iterations": [{"type": "message", "input_tokens": 5}]});
        assert_eq!(usage_numbers(&u), (1, 0, 0));
    }

    #[test]
    fn a_usage_far_from_the_tail_is_still_found() {
        let d = tmpdir("far");
        let p = d.join("s.jsonl");
        let big = "x".repeat(300_000);
        let mut body = asst(json!({"input_tokens": 1234})) + "\n";
        for _ in 0..3 {
            body += &json!({"type": "user", "message": {"content": big}}).to_string();
            body += "\n";
        }
        std::fs::write(&p, body).unwrap();
        assert_eq!(tokens_of(&p), Some(1234));
        assert_eq!(tokens_of(&d.join("missing.jsonl")), None);
    }

    #[test]
    fn stdin_context_is_the_session_count_when_present() {
        let d = tmpdir("stdin");
        let p = d.join("s.jsonl");
        std::fs::write(&p, asst(json!({"input_tokens": 70}))).unwrap();
        let live = json!({"context_window": {"total_input_tokens": 500}});
        assert_eq!(session_tokens(&live, Some(&p)), Some(500));
        assert_eq!(session_tokens(&json!({}), Some(&p)), Some(70));
        let zero = json!({"context_window": {"total_input_tokens": 0}});
        assert_eq!(session_tokens(&zero, Some(&p)), Some(70));
        assert_eq!(session_tokens(&json!({}), None), None);
    }

    #[test]
    fn total_adds_every_subagent_once() {
        let cfg = tmpdir("total");
        let proj = cfg.join("projects/-home-x");
        let sub = proj.join("sid1/subagents");
        std::fs::create_dir_all(&sub).unwrap();
        let t = proj.join("sid1.jsonl");
        std::fs::write(&t, asst(json!({"input_tokens": 1000}))).unwrap();
        std::fs::write(
            sub.join("agent-a.jsonl"),
            asst(json!({"input_tokens": 200})),
        )
        .unwrap();
        std::fs::write(
            sub.join("agent-b.jsonl"),
            asst(json!({"cache_read_input_tokens": 30})),
        )
        .unwrap();
        std::fs::write(sub.join("agent-b.meta.json"), "{}").unwrap();
        // The same worker linked into a second project dir is counted once.
        #[cfg(unix)]
        {
            let other = cfg.join("projects/-home-y/sid1/subagents");
            std::fs::create_dir_all(&other).unwrap();
            std::os::unix::fs::symlink(sub.join("agent-a.jsonl"), other.join("agent-a.jsonl"))
                .unwrap();
        }
        let dirs = session_subagent_dirs(Some(&t), Some(&cfg), Some("sid1"));
        let files = subagent_transcripts(&dirs);
        assert_eq!(files.len(), 2);
        let (s, total) = session_and_total(session_tokens(&json!({}), Some(&t)), &files);
        assert_eq!((s, total), (Some(1000), Some(1230)));
        assert_eq!(
            segments(s, total),
            vec!["session 1k".to_string(), "total: 1.2k".to_string()]
        );
        // No session and no subagents: nothing to say.
        assert_eq!(session_and_total(None, &[]), (None, None));
        assert!(segments(None, None).is_empty());
    }

    #[test]
    fn counts_format_like_claude_code() {
        assert_eq!(fmt_tokens(842), "842");
        assert_eq!(fmt_tokens(1000), "1k");
        assert_eq!(fmt_tokens(13_450), "13.4k");
        assert_eq!(fmt_tokens(124_832), "124.8k");
        assert_eq!(fmt_tokens(999_949), "999.9k");
        assert_eq!(fmt_tokens(999_950), "1M");
        assert_eq!(fmt_tokens(1_234_567), "1.2M");
        assert_eq!(fmt_tokens(2_500_000_000), "2.5B");
    }
}

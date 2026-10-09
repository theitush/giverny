//! "Manage this" → the `giverny:manage` skill, every time (giverny#269).
//!
//! A skill runs only when the model picks it from its description, and
//! "manage" reads as a plain verb; in a repo that carries its own
//! orchestrating skill the two compete for the same ask. So the plugin's
//! `UserPromptSubmit` hook reads each prompt and, for one that asks to
//! manage, adds context telling the model to invoke the skill first.
//!
//! The match is narrow on purpose: "manage" as an imperative at the start
//! of a clause ("manage that task", "… and manage this plz", "can you
//! manage these?", "/manage"), with something to manage after it. A
//! question about managing ("how do I manage X", "why does he manage it"),
//! another subject ("make sure they manage the resources"), the other
//! forms ("managing", "manager") and quoted text never fire it.

use serde_json::json;

/// What the model is told when a prompt asks to manage.
pub const CONTEXT: &str = "The user asked you to manage this work. Before anything else, invoke \
the `giverny:manage` skill with the Skill tool and run the work as it says: you are the \
dispatcher, and subagents do the tasks. That is what \"manage\" means here; another skill or \
instruction for orchestrating or dispatching work (a project's `/orchestrate`, for one) is not it.";

/// Words that may stand between a clause's start and "manage".
const LEAD: &[&str] = &[
    "and", "then", "so", "now", "ok", "okay", "k", "pls", "plz", "please", "just", "also", "yes",
    "yeah", "yh", "yep", "sure", "cool", "great", "nice", "alright", "right", "go", "ahead",
    "lets", "let's", "let’s", "hey", "hi", "oh", "well", "fine", "good",
];

/// Words right after "manage" that name what to manage.
const OBJECT: &[&str] = &[
    "this",
    "that",
    "these",
    "those",
    "it",
    "them",
    "all",
    "everything",
    "both",
    "each",
    "task",
    "tasks",
    "issue",
    "issues",
];

/// After "manage the", one of these within three words makes it work to manage.
const WORK: &[&str] = &[
    "task", "tasks", "issue", "issues", "work", "job", "jobs", "rest", "queue", "backlog", "board",
    "project", "list", "fix", "fixes", "bug", "bugs", "feature", "features", "pr", "prs", "ticket",
    "tickets", "todo", "todos", "whole", "lot", "plan", "review",
];

/// Words that may close a bare "manage" ("ok manage plz").
const POLITE: &[&str] = &["pls", "plz", "please", "now", "thanks", "thx", "ty"];

#[derive(Debug, PartialEq)]
enum Tok {
    Word(String),
    /// A clause's edge: sentence punctuation, a comma, a line break.
    Edge,
}

/// Whether `prompt` asks to manage the work.
pub fn asks_to_manage(prompt: &str) -> bool {
    let p = prompt.trim();
    if let Some(rest) = p.strip_prefix("/manage") {
        return rest.is_empty() || rest.starts_with(char::is_whitespace);
    }
    if p.starts_with('/') && p.split_whitespace().next() == Some("/giverny:manage") {
        return false; // the skill runs already
    }
    let toks = tokens(&unquoted(p));
    toks.iter().enumerate().any(|(i, t)| {
        matches!(t, Tok::Word(w) if w == "manage") && led(&toks[..i]) && followed(&toks[i + 1..])
    })
}

/// The `UserPromptSubmit` reply for `prompt`: the context, or nothing.
pub fn reply(prompt: &str) -> Option<String> {
    asks_to_manage(prompt).then(|| {
        json!({
            "hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": CONTEXT
            }
        })
        .to_string()
    })
}

/// A `UserPromptSubmit` payload's prompt; `None` for any other payload.
pub fn prompt_of(input: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Submit {
        hook_event_name: Option<String>,
        prompt: Option<String>,
    }
    if !input.contains("\"UserPromptSubmit\"") {
        return None;
    }
    let s: Submit = serde_json::from_str(input).ok()?;
    if s.hook_event_name.as_deref() != Some("UserPromptSubmit") {
        return None;
    }
    Some(s.prompt.unwrap_or_default())
}

/// `prompt` without what it quotes: `"…"`, `“…”`, `` `…` `` spans, `>`
/// lines and pasted content. A quote left open is kept as text.
fn unquoted(prompt: &str) -> String {
    let mut lines = Vec::new();
    let mut pasted = false;
    for line in prompt.lines() {
        let l = line.trim_start();
        if l.starts_with("<pasted_content") {
            pasted = !l.contains("</pasted_content>");
            continue;
        }
        if pasted {
            pasted = !l.starts_with("</pasted_content>");
            continue;
        }
        if !l.starts_with('>') {
            lines.push(line);
        }
    }
    let text = lines.join("\n");
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let close = match chars[i] {
            '"' => Some('"'),
            '`' => Some('`'),
            '“' => Some('”'),
            _ => None,
        };
        if let Some(c) = close
            && let Some(end) = chars[i + 1..].iter().position(|&x| x == c)
        {
            out.push_str(" \u{2026} "); // a stand-in word: no edge, no lead
            i += end + 2;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn tokens(text: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut Vec<Tok>| {
        if !word.is_empty() {
            out.push(Tok::Word(std::mem::take(word).to_lowercase()));
        }
    };
    for c in text.chars() {
        if c.is_alphanumeric() || matches!(c, '\'' | '’' | '#' | '-' | '_') {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            if matches!(c, '.' | '!' | '?' | ';' | ':' | ',' | '\n' | '(' | ')') {
                out.push(Tok::Edge);
            } else if c == '…' {
                out.push(Tok::Word(c.to_string()));
            }
        }
    }
    flush(&mut word, &mut out);
    out
}

fn word(t: &Tok) -> Option<&str> {
    match t {
        Tok::Word(w) => Some(w),
        Tok::Edge => None,
    }
}

/// Whether what comes before "manage" makes it an imperative: a clause's
/// start, past lead words and a "can you" / "could you" …, or "want you to".
fn led(before: &[Tok]) -> bool {
    let mut n = before.len();
    let at = |k: usize| word(&before[k]);
    if n >= 3
        && at(n - 1) == Some("to")
        && matches!(at(n - 2), Some("you" | "u"))
        && matches!(at(n - 3), Some("want" | "need" | "like"))
    {
        return true;
    }
    loop {
        if n == 0 || before[n - 1] == Tok::Edge {
            return true;
        }
        let w = at(n - 1).unwrap_or_default();
        if LEAD.contains(&w) {
            n -= 1;
        } else if matches!(w, "you" | "u")
            && n >= 2
            && matches!(
                at(n - 2),
                Some("can" | "could" | "would" | "will" | "pls" | "plz")
            )
        {
            n -= 2;
        } else {
            return false;
        }
    }
}

/// Whether what comes after "manage" is work to manage.
fn followed(after: &[Tok]) -> bool {
    let Some(first) = after.first() else {
        return true; // "… and manage"
    };
    let Some(w) = word(first) else {
        return true; // "ok manage."
    };
    if OBJECT.contains(&w) || is_ref(w) {
        return true;
    }
    if w == "the" {
        return after[1..]
            .iter()
            .take(3)
            .map_while(word)
            .any(|w| WORK.contains(&w) || is_ref(w));
    }
    POLITE.contains(&w) && after.get(1).is_none_or(|t| word(t).is_none())
}

/// A task reference: `#12`, `giverny#12`, `12`.
fn is_ref(w: &str) -> bool {
    let n = w.rsplit('#').next().unwrap_or(w);
    !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prompts from Ita's transcripts that ask to manage (giverny#269).
    const FIRE: &[&str] = &[
        "mm.. the dewpoint stuff is not in prod yet.. so lets just update whats live which is \
         the other metrics. so yes update one mm not cell the bigger one and test that it looks \
         fine . manage that task and leave it for me in review and then we'll continue if it \
         all works out",
        "take a look at the ofer research plana nd tell me what you think of it. the idea is to \
         make moeny on the simulator. review what they put in the plan and suggest alternatives \
         or diff things to do or whtaever. and manage this plz",
        "so first of all, the orchestrate skill and plugin/feature/panel etc etc should all be \
         named manage. so the pane is the management panel, the skill is /manage , etc. so i \
         want no mention of orchestrate as a thing that exists in this repo. \nmanage this task \
         plz ^_^",
        "manage that task",
        "Manage these",
        "manage it",
        "ok manage #269 and #268",
        "manage giverny#12",
        "pls manage the rest of the queue",
        "can you manage these three issues?",
        "i want you to manage this",
        "fix the tooltip, then manage the review",
        "/manage",
        "/manage the backlog",
        "great, just manage.",
        "ok manage plz",
    ];

    /// Prompts that only talk about managing, from the same transcripts and
    /// a few more.
    const QUIET: &[&str] = &[
        "orchestrators should be aware of other orchestrators on the machine and specifically \
         make sure they manage the resources properly and not overload my RAM or CPU",
        "why does ofer manage it and our engine cant?",
        "so now i just tell agents to manage? do i need to restart or something or itll just \
         work?",
        "does saying manage supposed to make agents use the manage skill? coz im not sure they \
         are getting it..",
        "how does the orchestrator deal with devops? so like managing the resources it has",
        "can you create a dashboard of the data? the audience is the desk managers",
        "how do I manage this?",
        "how would you manage these?",
        "should I manage it myself",
        "don't manage this one, just fix it",
        "please manage the memory better in this function",
        "manage memory carefully here",
        "when I say \"manage that task\" it should use the skill",
        "the `manage this` phrase",
        "> manage this\nwhat does this quote mean?",
        "/giverny:manage the backlog",
        "/manager",
        "Run exactly this with the Bash tool: giverny-manage show",
        "the manage skill",
        "managed this already",
        "",
    ];

    #[test]
    fn real_asks_to_manage_fire_it() {
        for p in FIRE {
            assert!(asks_to_manage(p), "should fire: {p:?}");
        }
    }

    #[test]
    fn talk_about_managing_does_not() {
        for p in QUIET {
            assert!(!asks_to_manage(p), "should stay quiet: {p:?}");
        }
    }

    #[test]
    fn pasted_content_is_not_the_ask() {
        let p = "<pasted_content id=\"1\">\nmanage this\n</pasted_content>\nwhat is this?";
        assert!(!asks_to_manage(p));
        assert!(asks_to_manage(&format!("{p}\nmanage it")));
    }

    #[test]
    fn the_hook_reads_only_a_prompt_and_replies_with_context() {
        let input = json!({"session_id": "s1", "hook_event_name": "UserPromptSubmit",
                           "prompt": "manage that task", "cwd": "/x"})
        .to_string();
        let prompt = prompt_of(&input).unwrap();
        let out = reply(&prompt).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
        assert!(
            v["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .contains("`giverny:manage`")
        );
        assert_eq!(reply("how do I manage this?"), None);
        // Another event that only mentions it: not a prompt.
        let post = json!({"hook_event_name": "PostToolUse", "tool_name": "Read",
                          "tool_response": {"content": "\"UserPromptSubmit\""}})
        .to_string();
        assert_eq!(prompt_of(&post), None);
    }
}

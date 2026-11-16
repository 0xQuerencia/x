//! Deterministic phrases for window commands. Anything not matched here falls through to
//! the other planners; nothing is guessed.

use super::apps::{self, Entry};
use super::{Op, Request, Target, Ws};

const THIS: [&str; 10] = [
    "this",
    "app",
    "window",
    "application",
    "this app",
    "this application",
    "this window",
    "it",
    "current window",
    "the current window",
];

fn target(words: &str, entries: &[Entry]) -> Option<Target> {
    let words = words.trim();
    if THIS.contains(&words.trim_start_matches("the ").trim()) {
        return Some(Target::default());
    }
    let named = apps::normalize(words);
    if apps::known(&named, entries) {
        return Some(Target {
            app: named,
            pick: None,
        });
    }
    // "firefox 2": the second Firefox window.
    let (name, n) = named.rsplit_once(' ')?;
    let pick = n.parse::<u8>().ok().filter(|n| *n >= 1)?;
    apps::known(name, entries).then(|| Target {
        app: name.trim().to_string(),
        pick: Some(pick),
    })
}

fn workspace(words: &str) -> Option<Ws> {
    let words = words.trim().trim_start_matches("the ").trim();
    match words {
        "next workspace" | "next" => Some(Ws::Next),
        "previous workspace" | "previous" | "last workspace" => Some(Ws::Prev),
        _ => {
            let n = words.strip_prefix("workspace ").unwrap_or(words).trim();
            n.parse::<u8>()
                .ok()
                .filter(|n| (1..=10).contains(n))
                .map(Ws::N)
        }
    }
}

/// A question about the person's shortcuts, as the words to look up (empty lists them).
fn keys_question(text: &str) -> Option<String> {
    const LIST: [&str; 8] = [
        "what are my keybinds",
        "what are my shortcuts",
        "what are my keyboard shortcuts",
        "list my keybinds",
        "list my shortcuts",
        "show my keybinds",
        "show my shortcuts",
        "show my keyboard shortcuts",
    ];
    if LIST.contains(&text) {
        return Some(String::new());
    }
    for lead in [
        "what is the shortcut for",
        "what's the shortcut for",
        "what is the keybind for",
        "what's the keybind for",
        "what is the shortcut to",
        "what key",
        "which key",
        "which shortcut",
        "what shortcut",
    ] {
        if let Some(rest) = text
            .strip_prefix(lead)
            .map(str::trim)
            .filter(|r| !r.is_empty())
        {
            return Some(rest.to_string());
        }
    }
    // "what does super plus q do": only when the words really are a key combination.
    let rest = text.strip_prefix("what does ")?;
    let rest = rest
        .strip_suffix(" do")
        .unwrap_or(rest)
        .replace(" plus ", "+");
    super::keys::canonical(&rest)
        .filter(|c| c.contains('+'))
        .map(|_| rest)
}

fn prefixed<'a>(text: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    prefixes
        .iter()
        .find_map(|p| text.strip_prefix(p)?.strip_prefix(' '))
}

fn req(op: Op, target: Target, ws: Option<Ws>) -> Option<Request> {
    Some(Request { op, target, ws })
}

pub fn parse(text: &str) -> Option<Request> {
    parse_with(text, &apps::load())
}

/// [`parse`] against a given set of installed apps.
pub fn parse_with(text: &str, entries: &[Entry]) -> Option<Request> {
    let text = super::speech::fix(text);
    let text = text.as_str();
    match text {
        "undo" | "undo that" | "undo it" => return req(Op::Undo, Target::default(), None),
        "show my open windows"
        | "show open windows"
        | "list windows"
        | "list my windows"
        | "what windows are open" => return req(Op::List, Target::default(), None),
        "next workspace"
        | "go to the next workspace"
        | "switch to the next workspace"
        | "go to next workspace" => return req(Op::Workspace, Target::default(), Some(Ws::Next)),
        "previous workspace"
        | "go to the previous workspace"
        | "switch to the previous workspace"
        | "go to previous workspace" => {
            return req(Op::Workspace, Target::default(), Some(Ws::Prev));
        }
        "focus the previous window"
        | "go to the previous window"
        | "bring the last window back"
        | "go back to the last window"
        | "switch to the previous window"
        | "previous window"
        | "last window" => return req(Op::Back, Target::default(), None),
        _ => {}
    }
    if let Some(query) = keys_question(text) {
        return req(
            Op::Keys,
            Target {
                app: query,
                pick: None,
            },
            None,
        );
    }
    if let Some(rest) = prefixed(text, &["move", "send", "put"]) {
        let (who, to) = rest.split_once(" to ").or_else(|| rest.split_once(" on "))?;
        return req(Op::Move, target(who, entries)?, Some(workspace(to)?));
    }
    if let Some(rest) = prefixed(
        text,
        &["switch to", "go to", "focus", "show", "bring up"],
    ) {
        if let Some(ws) = workspace(rest).filter(|_| rest.starts_with("workspace")) {
            return req(Op::Workspace, Target::default(), Some(ws));
        }
        if rest.starts_with("workspace") {
            return None; // a bad workspace number is never an app called "workspace"
        }
        return req(Op::Focus, target(rest, entries)?, None);
    }
    // Natural wording for bringing a specific app to the front. The target
    // still has to match a real installed app or a known window role.
    if let Some(rest) = prefixed(text, &["bring"]) {
        let who = rest
            .strip_suffix(" to the front")
            .or_else(|| rest.strip_suffix(" to front"))
            .or_else(|| rest.strip_suffix(" forward"))?;
        return req(Op::Focus, target(who, entries)?, None);
    }
    if let Some(rest) = prefixed(text, &["tuck"])
        && let Some(who) = rest.strip_suffix(" away")
    {
        return req(Op::Minimize, target(who, entries)?, None);
    }
    for (verb, op) in [
        ("minimize", Op::Minimize),
        ("hide", Op::Minimize),
        ("maximize", Op::Maximize),
        ("fullscreen", Op::Maximize),
        ("close", Op::Close),
        ("quit", Op::Close),
        ("exit", Op::Close),
        ("shut", Op::Close),
        ("shut down", Op::Close),
    ] {
        if let Some(rest) = prefixed(text, &[verb]) {
            return req(op, target(rest, entries)?, None);
        }
    }
    None
}

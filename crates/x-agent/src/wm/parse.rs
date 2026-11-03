//! Deterministic phrases for window commands. Anything not matched here falls through to
//! the other planners; nothing is guessed.

use super::{Op, Request, Target, Ws};
use crate::tools::resolve_app;

const THIS: [&str; 5] = [
    "this",
    "this window",
    "it",
    "current window",
    "the current window",
];

fn target(words: &str) -> Option<Target> {
    let words = words
        .trim()
        .trim_start_matches("the ")
        .trim_start_matches("my ")
        .trim();
    if THIS.contains(&words) {
        return Some(Target::default());
    }
    if let Some(app) = resolve_app(words) {
        return Some(Target {
            app: app.into(),
            pick: None,
        });
    }
    // "firefox 2": the second Firefox window.
    let (name, n) = words.rsplit_once(' ')?;
    let pick = n.parse::<u8>().ok().filter(|n| *n >= 1)?;
    Some(Target {
        app: resolve_app(name.trim())?.into(),
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

fn prefixed<'a>(text: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    prefixes
        .iter()
        .find_map(|p| text.strip_prefix(p)?.strip_prefix(' '))
}

fn req(op: Op, target: Target, ws: Option<Ws>) -> Option<Request> {
    Some(Request { op, target, ws })
}

pub fn parse(text: &str) -> Option<Request> {
    let text = text
        .trim()
        .trim_end_matches(['.', '!', '?', ' '])
        .to_ascii_lowercase();
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
    if let Some(rest) = prefixed(text, &["move", "send"]) {
        let (who, to) = rest.split_once(" to ")?;
        return req(Op::Move, target(who)?, Some(workspace(to)?));
    }
    if let Some(rest) = prefixed(text, &["switch to", "go to", "focus"]) {
        if let Some(ws) = workspace(rest).filter(|_| rest.starts_with("workspace")) {
            return req(Op::Workspace, Target::default(), Some(ws));
        }
        return req(Op::Focus, target(rest)?, None);
    }
    for (verb, op) in [
        ("minimize", Op::Minimize),
        ("maximize", Op::Maximize),
        ("close", Op::Close),
    ] {
        if let Some(rest) = prefixed(text, &[verb]) {
            return req(op, target(rest)?, None);
        }
    }
    None
}

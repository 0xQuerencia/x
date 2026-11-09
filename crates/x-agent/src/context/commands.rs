//! Contextual commands: "search this on GitHub", "open this link", "copy this". Parsing is
//! deterministic and strict: a command only matches when a reference word ("this",
//! "the selected text") sits in a recognised phrase. Resolution turns the reference into
//! real text from a [`Context`], or says exactly why it cannot. No model is involved.

use super::{Context, ContextError};
use crate::tools::{self, Action};

/// Words that point at the current selection. Longest first so "the selected error"
/// is not mistaken for "the selected".
const REFS: &[&str] = &[
    "the selected error",
    "the selected text",
    "the highlighted text",
    "the selection",
    "selected text",
    "highlighted text",
    "this error",
    "that error",
    "this text",
    "that text",
    "this link",
    "that link",
    "the link",
    "this url",
    "this",
    "that",
    "it",
];

/// Spoken site names and the search table key they mean.
const SITES: &[(&str, &str)] = &[
    ("google", "google"),
    ("github", "github"),
    ("git hub", "github"),
    ("stack overflow", "stackoverflow"),
    ("stackoverflow", "stackoverflow"),
    ("youtube", "youtube"),
    ("wikipedia", "wikipedia"),
    ("reddit", "reddit"),
];

const MAX_QUERY: usize = 300;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Search {
        site: &'static str,
    },
    /// A site X cannot search, named so the person hears it instead of a wrong search.
    UnknownSite(String),
    OpenLink,
    Copy,
    Explain,
}

impl Command {
    /// Whether this command needs the selection read at all.
    pub fn needs_selection(&self) -> bool {
        !matches!(self, Self::UnknownSite(_))
    }
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Removes the one reference phrase, returning what is left. `None` when there is no
/// reference or more than one (ambiguous).
fn strip_reference(text: &str) -> Option<String> {
    let padded = format!(" {} ", words(text).join(" "));
    let mut hits = REFS
        .iter()
        .filter_map(|r| padded.find(&format!(" {r} ")).map(|at| (at, r)));
    let (at, phrase) = hits.next()?;
    let shorter_overlap =
        hits.any(|(other, r)| !(other >= at && other + r.len() <= at + phrase.len()));
    if shorter_overlap {
        return None;
    }
    let rest = format!("{} {}", &padded[..at], &padded[at + phrase.len() + 1..]);
    Some(rest.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn site_of(tail: &str) -> Option<Result<&'static str, String>> {
    let tail = tail.trim();
    if tail.is_empty() {
        return Some(Ok("google"));
    }
    let name = ["on ", "in ", "with ", "using ", "at ", "up on ", "for on "]
        .iter()
        .find_map(|p| tail.strip_prefix(p))
        .unwrap_or(tail);
    let name = name.strip_suffix(" for").unwrap_or(name).trim();
    SITES
        .iter()
        .find(|(spoken, _)| *spoken == name)
        .map(|(_, key)| Ok(*key))
        .or_else(|| {
            let plain = name.split(' ').count() <= 2 && !name.is_empty();
            plain.then(|| Err(name.to_string()))
        })
}

/// Recognises a contextual command in spoken or typed text (wake word already removed).
pub fn parse(text: &str) -> Option<Command> {
    let rest = strip_reference(text)?;
    for verb in ["search for", "search", "look up", "lookup", "find"] {
        if let Some(tail) = rest.strip_prefix(verb) {
            if tail.is_empty() || tail.starts_with(' ') {
                return Some(match site_of(tail)? {
                    Ok(site) => Command::Search { site },
                    Err(name) => Command::UnknownSite(name),
                });
            }
        }
    }
    if rest == "google" {
        return Some(Command::Search { site: "google" });
    }
    match rest.as_str() {
        "open" | "go to" | "visit" => Some(Command::OpenLink),
        "copy" => Some(Command::Copy),
        "explain" | "what does mean" | "what is" => Some(Command::Explain),
        _ => None,
    }
}

pub struct Resolved {
    pub actions: Vec<Action>,
    /// Things worth telling the person, such as "the selection was trimmed".
    pub notes: Vec<String>,
}

fn flatten(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Turns a parsed command plus a snapshot into validated actions, or the reason it cannot.
pub fn resolve(cmd: &Command, ctx: &Context) -> Result<Resolved, ContextError> {
    let mut notes = Vec::new();
    let invalid = |why: String| ContextError::Unsupported(why);
    let action = match cmd {
        Command::UnknownSite(name) => {
            return Err(ContextError::Unsupported(format!(
                "X cannot search {name:?} yet. Nothing was done."
            )));
        }
        Command::Search { site } => {
            let query = flatten(ctx.selected_text()?);
            let query = if query.chars().count() > MAX_QUERY {
                notes.push(format!(
                    "The selection was long; searching its first {MAX_QUERY} characters"
                ));
                query.chars().take(MAX_QUERY).collect()
            } else {
                query
            };
            Action::Search {
                target: (*site).into(),
                query,
                browser: String::new(),
            }
        }
        Command::OpenLink => {
            let text = flatten(ctx.selected_text()?);
            if text.contains(' ') {
                return Err(invalid("The selection is not a single link".into()));
            }
            if text.starts_with("http://") {
                return Err(invalid("X only opens HTTPS links".into()));
            }
            let url = if text.starts_with("https://") {
                text
            } else {
                format!("https://{text}")
            };
            Action::Url {
                target: url,
                browser: String::new(),
            }
        }
        Command::Copy => Action::Copy {
            text: ctx.selected_text()?.to_string(),
        },
        Command::Explain => {
            return Err(invalid("Explaining a selection needs a model".into()));
        }
    };
    let action = action.validate().map_err(invalid)?;
    if ctx.selection_truncated() {
        notes.push("The selection was too long and was cut".into());
    }
    Ok(Resolved {
        actions: vec![action],
        notes,
    })
}

/// Like `tools::plan_rules` for contextual speech: wake word stripped, then parsed.
pub fn parse_utterance(utterance: &str) -> Option<Command> {
    parse(&tools::strip_wake_word(utterance))
}

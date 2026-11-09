//! Spoken or typed edit commands. Parsing is strict and deterministic: a phrase either
//! matches one of these shapes or it is not an edit command and falls through to the other
//! planners. Nothing here calls a model.

/// What an edit command refers to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
    /// "this", "the selected text": the current selection, and only that.
    Selection,
    /// "that", "it": the current selection if there is one, else what X last inserted.
    Recent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Transform {
    Shorter,
    FixGrammar,
    MoreCasual,
    Bullets,
    Rewrite,
    Translate(String),
    Explain,
}

impl Transform {
    /// What the model is told to do. Fixed text: nothing the person dictated is spliced in
    /// except a validated language name.
    pub fn instruction(&self) -> String {
        match self {
            Self::Shorter => "Make the text shorter while keeping its meaning and tone.".into(),
            Self::FixGrammar => {
                "Fix grammar, spelling and punctuation. Change nothing else.".into()
            }
            Self::MoreCasual => "Rewrite the text in a more casual, conversational tone.".into(),
            Self::Bullets => "Turn the text into a short list of bullet points, one per line, each starting with \"- \".".into(),
            Self::Rewrite => "Rewrite the text so it reads more clearly and smoothly.".into(),
            Self::Translate(lang) => format!("Translate the text into {lang}."),
            Self::Explain => "Explain what the text means in plain language, briefly. If it is an error message, say the likely cause and the first thing to check.".into(),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Shorter => "Make shorter".into(),
            Self::FixGrammar => "Fix grammar".into(),
            Self::MoreCasual => "Make more casual".into(),
            Self::Bullets => "Turn into bullet points".into(),
            Self::Rewrite => "Rewrite".into(),
            Self::Translate(lang) => format!("Translate into {lang}"),
            Self::Explain => "Explain".into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    // Changes that need no model and apply at once once the target is verified.
    ScratchThat,
    RemoveLastSentence,
    DeleteLastWord,
    NewParagraph,
    SelectLastSentence,
    UndoThat,
    /// Deterministic spacing and punctuation cleanup, shown as a preview.
    Cleanup(Scope),
    /// A rewrite shown as a preview. Bullets try the deterministic path first.
    Transform(Transform, Scope),
    // Answers to a preview that is waiting.
    Accept,
    Retry,
    Cancel,
    SendToModel,
}

impl Command {
    /// Whether this may call a model, which decides whether it runs off the control thread.
    pub fn may_use_model(&self) -> bool {
        matches!(self, Self::Transform(..) | Self::Retry | Self::SendToModel)
    }
}

fn words(text: &str) -> String {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Splits a leading reference ("that", "this", "the selected paragraph"…) off `rest`.
fn reference(rest: &str) -> Option<(Scope, &str)> {
    const SELECTION: [&str; 7] = [
        "the selected paragraph",
        "the selected text",
        "the selection",
        "selected text",
        "this paragraph",
        "this text",
        "this",
    ];
    const RECENT: [&str; 4] = ["that text", "that", "it", "what i just said"];
    SELECTION
        .iter()
        .find_map(|r| word_prefix(rest, r).map(|t| (Scope::Selection, t)))
        .or_else(|| {
            RECENT
                .iter()
                .find_map(|r| word_prefix(rest, r).map(|t| (Scope::Recent, t)))
        })
}

fn word_prefix<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(prefix)?;
    (rest.is_empty() || rest.starts_with(' ')).then(|| rest.trim_start())
}

fn language(name: &str) -> Option<String> {
    let name = name.trim().trim_start_matches("the ").trim();
    let ok = !name.is_empty()
        && name.chars().count() <= 24
        && name.split(' ').count() <= 2
        && name.chars().all(|c| c.is_ascii_alphabetic() || c == ' ');
    ok.then(|| {
        name.split(' ')
            .map(|w| {
                let mut c = w.chars();
                c.next().map_or(String::new(), |f| {
                    f.to_uppercase().collect::<String>() + c.as_str()
                })
            })
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// Recognises an edit command in spoken or typed text (wake word already removed).
pub fn parse(text: &str) -> Option<Command> {
    use Command::*;
    let said = words(text);
    let said = said.as_str();
    match said {
        "scratch that" | "scratch it" => return Some(ScratchThat),
        "remove the last sentence" | "delete the last sentence" | "remove last sentence" => {
            return Some(RemoveLastSentence);
        }
        "delete the last word" | "remove the last word" | "delete last word" => {
            return Some(DeleteLastWord);
        }
        "add a new paragraph" | "new paragraph" | "start a new paragraph" => {
            return Some(NewParagraph);
        }
        "select the last sentence" => return Some(SelectLastSentence),
        "undo that" | "undo it" | "undo the last edit" => return Some(UndoThat),
        "replace the selected text"
        | "replace it"
        | "replace"
        | "insert the corrected version"
        | "insert it"
        | "apply it"
        | "apply the edit"
        | "accept the edit"
        | "accept" => {
            return Some(Accept);
        }
        "try again" | "retry" | "do it again" => return Some(Retry),
        "cancel the edit" | "keep the original" | "discard the edit" | "never mind" => {
            return Some(Cancel);
        }
        "send it" | "send it to the model" | "go ahead and send it" => return Some(SendToModel),
        _ => {}
    }
    if let Some(rest) = word_prefix(said, "make") {
        let (scope, tail) = reference(rest)?;
        return match tail {
            "shorter" | "more concise" | "shorter please" => {
                Some(Transform(self::Transform::Shorter, scope))
            }
            "more casual" | "casual" | "less formal" => {
                Some(Transform(self::Transform::MoreCasual, scope))
            }
            _ => None,
        };
    }
    if let Some(rest) = word_prefix(said, "fix") {
        let rest = word_prefix(rest, "the").unwrap_or(rest);
        if let Some(tail) = word_prefix(rest, "grammar") {
            return Some(Transform(
                self::Transform::FixGrammar,
                scope_after(tail, "in")?,
            ));
        }
        if let Some(tail) =
            word_prefix(rest, "punctuation").or_else(|| word_prefix(rest, "spacing"))
        {
            return Some(Cleanup(scope_after(tail, "in")?));
        }
    }
    if let Some(rest) = word_prefix(said, "clean up") {
        let rest = word_prefix(rest, "the").unwrap_or(rest);
        let tail = word_prefix(rest, "punctuation").or_else(|| word_prefix(rest, "spacing"))?;
        return Some(Cleanup(scope_after(tail, "in")?));
    }
    if let Some(rest) = word_prefix(said, "turn") {
        let (scope, tail) = reference(rest)?;
        let tail = word_prefix(tail, "into")?;
        return matches!(
            tail,
            "bullet points" | "bullets" | "a bulleted list" | "a list"
        )
        .then_some(Transform(self::Transform::Bullets, scope));
    }
    if let Some(rest) = word_prefix(said, "rewrite") {
        let (scope, tail) = reference(rest)?;
        return tail
            .is_empty()
            .then_some(Transform(self::Transform::Rewrite, scope));
    }
    if let Some(rest) = word_prefix(said, "translate") {
        let (scope, tail) = reference(rest)?;
        let lang = word_prefix(tail, "into").or_else(|| word_prefix(tail, "to"))?;
        return Some(Transform(
            self::Transform::Translate(language(lang)?),
            scope,
        ));
    }
    if let Some(rest) = word_prefix(said, "explain") {
        let (scope, tail) = reference(rest)?;
        return matches!(tail, "" | "error" | "the error")
            .then_some(Transform(self::Transform::Explain, scope));
    }
    None
}

/// "that", "in this", or nothing at all (which means the usual: selection, else recent).
fn scope_after(tail: &str, joiner: &str) -> Option<Scope> {
    if tail.is_empty() {
        return Some(Scope::Recent);
    }
    let tail = word_prefix(tail, joiner).unwrap_or(tail);
    match reference(tail)? {
        (scope, "") => Some(scope),
        _ => None,
    }
}

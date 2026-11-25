//! Local command boundaries. Quoted content remains literal data, including words
//! that otherwise begin a desktop action. No quote or escape is shell syntax.

#[derive(Default)]
struct Quotes {
    end: Option<char>,
    escaped: bool,
}

fn closing(ch: char) -> Option<char> {
    match ch {
        '"' | '\'' => Some(ch),
        '“' => Some('”'),
        '‘' => Some('’'),
        _ => None,
    }
}

impl Quotes {
    fn observe(&mut self, ch: char, previous: Option<char>, next: Option<char>) {
        let apostrophe = matches!(ch, '\'' | '’')
            && previous.is_some_and(char::is_alphanumeric)
            && next.is_some_and(char::is_alphanumeric);
        if self.escaped {
            self.escaped = false;
        } else if self.end.is_some() && ch == '\\' {
            self.escaped = true;
        } else if self.end == Some(ch) && !apostrophe {
            self.end = None;
        } else if self.end.is_none() && !apostrophe {
            self.end = closing(ch);
        }
    }
}

/// Normalize command spacing without changing spaces inside a quoted payload.
pub(super) fn normalize(text: &str) -> String {
    let mut quotes = Quotes::default();
    let mut result = String::new();
    let mut space = false;
    let mut previous = None;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_whitespace() && quotes.end.is_none() {
            space = !result.is_empty();
        } else {
            if space {
                result.push(' ');
                space = false;
            }
            result.push(ch);
        }
        quotes.observe(ch, previous, chars.peek().copied());
        previous = Some(ch);
    }
    result
}

/// Remove exactly one complete enclosing quote pair; never trim the payload.
/// Text after the closing quote belongs to a later command, not this literal.
pub(super) fn literal(text: &str) -> Option<&str> {
    let text = text.trim();
    let first = text.chars().next()?;
    if closing(first).is_none() {
        return Some(text);
    }
    let mut quotes = Quotes::default();
    let mut previous = None;
    let mut chars = text.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
        quotes.observe(ch, previous, chars.peek().map(|(_, ch)| *ch));
        if at > 0 && quotes.end.is_none() {
            return (at + ch.len_utf8() == text.len()).then_some(&text[first.len_utf8()..at]);
        }
        previous = Some(ch);
    }
    None
}

pub(super) fn split_steps(text: &str) -> Option<Vec<String>> {
    let lower = text.to_ascii_lowercase();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut at = 0;
    let mut quotes = Quotes::default();
    let mut previous = None;
    while at < text.len() {
        let ch = text[at..].chars().next()?;
        quotes.observe(ch, previous, text[at + ch.len_utf8()..].chars().next());
        if quotes.end.is_none()
            && let Some(joiner) = [" and then ", " then ", " and "].iter().find(|joiner| {
                lower[at..].strip_prefix(**joiner).is_some_and(|after| {
                    [
                        "open ",
                        "launch ",
                        "start ",
                        "search ",
                        "look up ",
                        "type ",
                        "write ",
                        "switch to ",
                        "focus ",
                        "move ",
                        "maximize ",
                        "minimize ",
                        "close ",
                        "list ",
                        "mute ",
                        "unmute ",
                        "volume ",
                        "brightness ",
                        "turn ",
                        "take a screenshot",
                        "take screenshot",
                        "screenshot",
                    ]
                    .iter()
                    .any(|verb| after.starts_with(verb))
                })
            })
        {
            parts.push(text[start..at].trim().to_string());
            start = at + joiner.len();
            at = start;
            previous = Some(' ');
            continue;
        }
        at += ch.len_utf8();
        previous = Some(ch);
    }
    if quotes.end.is_some() {
        return None;
    }
    parts.push(text[start..].trim().to_string());
    Some(parts)
}

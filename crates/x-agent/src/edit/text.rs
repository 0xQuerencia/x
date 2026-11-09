//! Deterministic text edits: ranges for "the last sentence" and "the last word", bullet
//! lists and punctuation cleanup. All offsets count characters (what accessibility uses),
//! never bytes, and every function is pure.

pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// The characters `a..b` of `text`.
pub fn slice(text: &str, a: usize, b: usize) -> String {
    text.chars().skip(a).take(b.saturating_sub(a)).collect()
}

fn is_end(c: char) -> bool {
    matches!(c, '.' | '!' | '?')
}

/// Character range of the last sentence (no surrounding whitespace). A sentence ends at
/// `.`, `!` or `?` followed by whitespace, or at a line break.
pub fn last_sentence(text: &str) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut end = chars.len();
    while end > 0 && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    // The last sentence's own terminator does not count as a boundary.
    let mut i = end - 1;
    while i > 0 && is_end(chars[i]) {
        i -= 1;
    }
    let mut start = 0;
    while i > 0 {
        let boundary = chars[i].is_whitespace()
            && (chars[i] == '\n' || is_end(chars[i - 1]) || chars[i - 1] == '\n');
        if boundary {
            start = i + 1;
            break;
        }
        i -= 1;
    }
    while start < end && chars[start].is_whitespace() {
        start += 1;
    }
    (start < end).then_some((start, end))
}

/// The range to delete for "remove the last sentence": the sentence plus the space that
/// separated it from the one before, so no dangling whitespace is left behind.
pub fn last_sentence_removal(text: &str) -> Option<(usize, usize)> {
    let (start, end) = last_sentence(text)?;
    let chars: Vec<char> = text.chars().collect();
    let mut from = start;
    while from > 0 && matches!(chars[from - 1], ' ' | '\t') {
        from -= 1;
    }
    // Trailing whitespace after the sentence stays where it is.
    Some((from, end))
}

/// The last word, as the trailing run of non-whitespace (punctuation stays with it).
pub fn last_word(text: &str) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut end = chars.len();
    while end > 0 && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    (start < end).then_some((start, end))
}

pub fn last_word_removal(text: &str) -> Option<(usize, usize)> {
    let (start, end) = last_word(text)?;
    let chars: Vec<char> = text.chars().collect();
    let mut from = start;
    while from > 0 && matches!(chars[from - 1], ' ' | '\t') {
        from -= 1;
    }
    Some((from, end))
}

fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text.trim();
    while let Some((a, b)) = last_sentence(rest) {
        out.push(slice(rest, a, b));
        rest = rest[..rest.char_indices().nth(a).map_or(0, |(i, _)| i)].trim_end();
    }
    out.reverse();
    out
}

const BULLETS: [&str; 3] = ["- ", "• ", "* "];

/// A bullet list from text that is already clearly structured: several lines, or several
/// sentences. `None` when it is a single thought (that needs a model) or already bullets.
pub fn bullets(text: &str) -> Option<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let items: Vec<String> = if lines.len() > 1 {
        lines.iter().map(|l| (*l).to_string()).collect()
    } else {
        let found = sentences(text);
        if found.len() < 2 {
            return None;
        }
        found
    };
    if items
        .iter()
        .all(|i| BULLETS.iter().any(|b| i.starts_with(b)))
    {
        return None;
    }
    Some(
        items
            .iter()
            .map(|i| format!("- {}", i.trim_end_matches(['.', ';', ','])))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Spacing and capitalisation only: collapse runs of spaces, no space before `, . ; : ! ?`,
/// no trailing spaces, a capital after a sentence end. `None` when nothing would change.
pub fn cleanup(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut capitalise = true;
    let mut after_end = false; // just saw . ! or ?, so a following space starts a sentence
    let mut pending_space = false;
    for c in text.chars() {
        match c {
            ' ' | '\t' => {
                pending_space = true;
                if after_end {
                    capitalise = true;
                }
            }
            '\n' => {
                pending_space = false;
                after_end = false;
                out.push('\n');
                capitalise = true;
            }
            ',' | '.' | ';' | ':' | '!' | '?' => {
                pending_space = false;
                out.push(c);
                after_end = is_end(c);
            }
            _ => {
                if pending_space && !out.is_empty() && !out.ends_with('\n') {
                    out.push(' ');
                }
                pending_space = false;
                after_end = false;
                if capitalise && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                    capitalise = false;
                } else {
                    out.push(c);
                    if !matches!(c, '"' | '\'' | '(') {
                        capitalise = false;
                    }
                }
            }
        }
    }
    (out != text).then_some(out)
}

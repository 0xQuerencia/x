//! A word-level diff for the edit preview, so the person sees what changed rather than
//! re-reading two paragraphs. Skipped (`None`) for very long text, where it would be noise.

use serde::Serialize;

const MAX_TOKENS: usize = 500;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Same,
    Removed,
    Added,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Seg {
    pub kind: Kind,
    pub text: String,
}

/// Words with the whitespace that follows them, so joining the tokens restores the text.
fn tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            in_space = true;
        } else if in_space || out.is_empty() {
            out.push(String::new());
            in_space = false;
        }
        if out.is_empty() {
            out.push(String::new());
        }
        if let Some(last) = out.last_mut() {
            last.push(c);
        }
    }
    out
}

pub fn words(original: &str, proposed: &str) -> Option<Vec<Seg>> {
    let (a, b) = (tokens(original), tokens(proposed));
    if a.len() > MAX_TOKENS || b.len() > MAX_TOKENS {
        return None;
    }
    let norm = |t: &String| t.trim().to_string();
    let mut lcs = vec![vec![0u16; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if norm(&a[i]) == norm(&b[j]) {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut segs: Vec<Seg> = Vec::new();
    let mut add = |kind: Kind, text: &str| match segs.last_mut() {
        Some(last) if last.kind == kind => last.text.push_str(text),
        _ => segs.push(Seg {
            kind,
            text: text.to_string(),
        }),
    };
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && norm(&a[i]) == norm(&b[j]) {
            add(Kind::Same, &b[j]);
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || lcs[i][j + 1] >= lcs[i + 1][j]) {
            add(Kind::Added, &b[j]);
            j += 1;
        } else {
            add(Kind::Removed, &a[i]);
            i += 1;
        }
    }
    Some(segs)
}

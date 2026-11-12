//! A small, explicitly requested local snippet catalog for keyboard users.
//! This does not change Hex's spoken text replacements or personal commands.
//! Reading a snippet is local; it is not typed into another app until the
//! person explicitly copies it through the existing clipboard adapter.

use std::fs;
use std::path::Path;

use serde::Deserialize;

const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_SNIPPETS: usize = 100;
const MAX_SNIPPET_CHARS: usize = 8_000;
const MAX_RESULTS: usize = 6;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    snippets: Vec<Snippet>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snippet {
    name: String,
    text: String,
    #[serde(default)]
    keywords: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hit {
    pub name: String,
    pub text: String,
}

fn valid(item: &Snippet) -> bool {
    let name = item.name.trim();
    !name.is_empty()
        && name.chars().count() <= 64
        && !name.chars().any(char::is_control)
        && !item.text.is_empty()
        && item.text.chars().count() <= MAX_SNIPPET_CHARS
        && !item.text.contains('\0')
        && item.keywords.len() <= 12
        && item.keywords.iter().all(|tag| {
            !tag.trim().is_empty()
                && tag.chars().count() <= 40
                && !tag.chars().any(char::is_control)
        })
}

fn read_catalog(path: &Path) -> Vec<Snippet> {
    // Never follow a link out of the user's local configuration directory.
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return vec![];
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_CONFIG_BYTES
    {
        return vec![];
    }
    let Ok(bytes) = fs::read(path) else {
        return vec![];
    };
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return vec![];
    }
    let Ok(catalog) = serde_json::from_slice::<Catalog>(&bytes) else {
        return vec![];
    };
    if catalog.snippets.len() > MAX_SNIPPETS {
        return vec![];
    }
    catalog.snippets.into_iter().filter(valid).collect()
}

fn matches(snippet: &Snippet, terms: &str) -> bool {
    let haystack = format!("{} {}", snippet.name, snippet.keywords.join(" ")).to_lowercase();
    terms
        .to_lowercase()
        .split_whitespace()
        .all(|term| haystack.contains(term))
}

fn search_file(path: &Path, query: &str) -> Vec<Hit> {
    let needle = query.trim();
    if needle.chars().count() > 100 {
        return vec![];
    }
    let mut hits: Vec<_> = read_catalog(path)
        .into_iter()
        .filter(|item| matches(item, needle))
        .map(|item| Hit {
            name: item.name,
            text: item.text,
        })
        .collect();
    hits.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    hits.truncate(MAX_RESULTS);
    hits
}

/// Type 'snip signature' (or 'snippet signature') in Quick to find matching
/// entries in ~/.config/hex/x-snippets.json. No background file reads.
pub fn search(query: &str) -> Vec<Hit> {
    let raw = query.trim();
    let needle = raw
        .strip_prefix("snip ")
        .or_else(|| raw.strip_prefix("snippet "));
    let Some(needle) = needle else {
        return vec![];
    };
    let Some(home) = dirs::home_dir() else {
        return vec![];
    };
    search_file(&home.join(".config/hex/x-snippets.json"), needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_and_searches_only_explicit_named_snippets() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("x-snippets.json");
        fs::write(
            &file,
            r#"{"snippets":[
                {"name":"Sign off","text":"Best,\nSomeone","keywords":["signature","email"]},
                {"name":"Issue template","text":"Steps to reproduce:\n","keywords":["bug"]}
            ]}"#,
        )
        .unwrap();
        let signature = search_file(&file, "sig");
        assert_eq!(signature.len(), 1);
        assert_eq!(signature[0].name, "Sign off");
        assert_eq!(signature[0].text, "Best,\nSomeone");
        assert_eq!(search_file(&file, "missing"), Vec::<Hit>::new());
    }

    #[test]
    fn invalid_and_oversized_text_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("x-snippets.json");
        fs::write(&file, r#"{"snippets":[{"name":"A","text":"abc","unknown":true}]}"#).unwrap();
        assert!(search_file(&file, "").is_empty());
        fs::write(&file, format!(r#"{{"snippets":[{{"name":"A","text":"{}"}}]}}"#, "x".repeat(8001))).unwrap();
        assert!(search_file(&file, "").is_empty());
        fs::write(&file, vec![b'x'; MAX_CONFIG_BYTES as usize + 1]).unwrap();
        assert!(search_file(&file, "").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_catalog() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("original.json");
        fs::write(&file, r#"{"snippets":[{"name":"secret","text":"private"}]}"#).unwrap();
        let link = tmp.path().join("shortcut.json");
        symlink(&file, &link).unwrap();
        assert!(search_file(&link, "secret").is_empty());
    }
}

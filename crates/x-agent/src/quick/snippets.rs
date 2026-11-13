//! User-owned, local-only snippet library. This is not a system-wide keyboard
//! interceptor: explicit X Quick selection copies a snippet after the user asks.
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

const MAX_ITEMS: usize = 100;
const MAX_BODY: usize = 4_096;
const MAX_FILE: u64 = 512 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snippet {
    pub trigger: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FileFormat {
    version: u8,
    snippets: Vec<Snippet>,
}

pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn user() -> Result<Self, String> {
        Ok(Self {
            path: dirs::config_dir()
                .ok_or("The user configuration folder is unavailable")?
                .join("hex")
                .join("x-snippets.json"),
        })
    }

    #[cfg(test)]
    fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn list(&self) -> Result<Vec<Snippet>, String> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("Cannot inspect snippet file: {error}")),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_FILE {
            return Err("Snippet storage is not a safe, bounded regular file".into());
        }
        let mut bytes = Vec::new();
        fs::File::open(&self.path)
            .and_then(|file| file.take(MAX_FILE + 1).read_to_end(&mut bytes))
            .map_err(|error| format!("Cannot read snippets: {error}"))?;
        if bytes.len() as u64 > MAX_FILE {
            return Err("Snippet storage exceeds its size limit".into());
        }
        let file: FileFormat = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Snippet storage contains invalid JSON: {error}"))?;
        if file.version != 1 || file.snippets.len() > MAX_ITEMS {
            return Err("Snippet storage has an unsupported version or too many entries".into());
        }
        let mut seen = std::collections::HashSet::new();
        for item in &file.snippets {
            validate(&item.trigger, &item.text)?;
            if !seen.insert(item.trigger.to_ascii_lowercase()) {
                return Err("Snippet storage has duplicate triggers".into());
            }
        }
        Ok(file.snippets)
    }

    pub fn upsert(&self, trigger: &str, text: &str) -> Result<(), String> {
        let trigger = trigger.trim();
        validate(trigger, text)?;
        let mut items = self.list()?;
        if let Some(existing) = items
            .iter_mut()
            .find(|item| item.trigger.eq_ignore_ascii_case(trigger))
        {
            existing.trigger = trigger.to_string();
            existing.text = text.to_string();
        } else {
            if items.len() >= MAX_ITEMS {
                return Err("Snippet library has reached its 100-item limit".into());
            }
            items.push(Snippet {
                trigger: trigger.to_string(),
                text: text.to_string(),
            });
        }
        self.write(&items)
    }

    pub fn delete(&self, trigger: &str) -> Result<bool, String> {
        let mut items = self.list()?;
        let before = items.len();
        items.retain(|item| !item.trigger.eq_ignore_ascii_case(trigger.trim()));
        if items.len() == before {
            return Ok(false);
        }
        self.write(&items)?;
        Ok(true)
    }

    fn write(&self, snippets: &[Snippet]) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or("No snippet configuration directory")?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("Cannot create snippet directory: {error}"))?;
        let dir = fs::symlink_metadata(parent)
            .map_err(|error| format!("Cannot inspect snippet directory: {error}"))?;
        if !dir.is_dir() || dir.file_type().is_symlink() {
            return Err("Snippet configuration directory must not be a symlink".into());
        }
        let data = serde_json::to_vec_pretty(&FileFormat {
            version: 1,
            snippets: snippets.to_vec(),
        })
        .map_err(|error| format!("Cannot encode snippets: {error}"))?;
        if data.len() as u64 > MAX_FILE {
            return Err("Snippet storage exceeds its size limit".into());
        }

        let tmp = parent.join(format!(
            ".x-snippets-{}-{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .map_err(|error| format!("Cannot create temporary snippet file: {error}"))?;
        let result = (|| {
            file.write_all(&data)
                .and_then(|_| file.sync_all())
                .map_err(|error| format!("Cannot save snippets: {error}"))?;
            // A failed replacement leaves the previous library intact.
            fs::rename(&tmp, &self.path)
                .map_err(|error| format!("Cannot replace snippet file: {error}"))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

/// Allow only predictable keys, never arbitrary key sequences or shortcut syntax.
fn validate(trigger: &str, text: &str) -> Result<(), String> {
    let valid = (1..=32).contains(&trigger.len())
        && trigger
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !valid {
        return Err("Snippet trigger must be 1–32 ASCII letters, numbers, - or _".into());
    }
    if text.is_empty()
        || text.chars().count() > MAX_BODY
        || text.contains('\0')
        || text
            .chars()
            .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
    {
        return Err("Snippet text must be 1–4096 characters without control characters".into());
    }
    Ok(())
}

/// Only explicit `snip <query>` or `snippet <query>` searches library text.
pub fn matches(query: &str) -> Result<Vec<Snippet>, String> {
    let raw = query.trim_start();
    let search = raw
        .strip_prefix("snip ")
        .or_else(|| raw.strip_prefix("snippet "))
        .or_else(|| (raw == "snippets").then_some(""));
    let Some(search) = search else {
        return Ok(Vec::new());
    };
    let needle = search.trim().to_lowercase();
    let mut matches: Vec<_> = Store::user()?
        .list()?
        .into_iter()
        .filter(|snippet| snippet.trigger.to_lowercase().contains(&needle))
        .collect();
    matches.sort_by(|a, b| a.trigger.cmp(&b.trigger));
    matches.truncate(6);
    Ok(matches)
}

/// Resolve only explicitly named placeholders. Every source value is inserted
/// once; placeholders inside a copied snapshot are never recursively expanded.
fn render_fields(
    text: &str,
    utc_date: &str,
    utc_time: &str,
    clipboard: Option<&str>,
) -> Result<String, String> {
    let mut output = text
        .replace("{{utc_date}}", utc_date)
        .replace("{{utc_time}}", utc_time);
    if output.contains("{{clipboard_snapshot}}") {
        let saved = clipboard.ok_or("There is no explicitly saved clipboard snapshot")?;
        output = output.replace("{{clipboard_snapshot}}", saved);
    }
    if output.chars().count() > crate::clipboard::MAX_COPY {
        return Err("Expanded snippet exceeds the clipboard's 8000-character limit".into());
    }
    Ok(output)
}

/// Render dynamic fields only when the user chooses to copy or insert a snippet.
/// This never reads the live system clipboard or sends snippet text to a model.
pub fn render_live(text: &str) -> Result<String, String> {
    if !text.contains("{{") {
        return Ok(text.to_string());
    }
    let date = crate::quick::datetime::now("date today")
        .ok_or("UTC clock unavailable")?
        .1;
    let time = crate::quick::datetime::now("time utc")
        .ok_or("UTC clock unavailable")?
        .1;
    let clock = time
        .split_whitespace()
        .last()
        .ok_or("UTC time unavailable")?;
    let clip = if text.contains("{{clipboard_snapshot}}") {
        Some(
            crate::quick::clipboard_history::Store::user()?
                .search("")?
                .first()
                .map(|item| item.text.clone())
                .ok_or("No saved clipboard snapshot found")?,
        )
    } else {
        None
    };
    render_fields(text, &date, clock, clip.as_deref())
}

/// Resolve a saved snippet by its unique trigger and expand dynamic fields.
pub fn expand(trigger: &str) -> Result<String, String> {
    let entries = Store::user()?.list()?;
    let text = entries
        .iter()
        .find(|item| item.trigger.eq_ignore_ascii_case(trigger))
        .ok_or("No saved snippet has that trigger")?;
    render_live(&text.text)
}

/// Insert saved text only when the user deliberately says "insert snippet NAME"
/// or "expand snippet NAME". Ordinary prose is never matched against triggers.
/// The transcript is not passed to a model or saved by this lookup.
fn spoken_name(phrase: &str) -> Option<&str> {
    let trimmed = phrase.trim();
    let lowered = trimmed.to_ascii_lowercase();
    let prefix = ["insert snippet ", "expand snippet "]
        .into_iter()
        .find(|prefix| lowered.starts_with(prefix))?;
    let name = trimmed[prefix.len()..]
        .trim()
        .trim_end_matches(['.', '!', '?']);
    let name = name.trim();
    if (2..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        Some(name)
    } else {
        None
    }
}

fn spoken_from(entries: &[Snippet], phrase: &str) -> Option<String> {
    let name = spoken_name(phrase)?;
    entries
        .iter()
        .find(|snippet| snippet.trigger.eq_ignore_ascii_case(name))
        .map(|snippet| snippet.text.clone())
}

/// Return None for all non-snippet speech without opening any configuration
/// files. A missing saved name is not hallucinated into an inserted value.
pub fn spoken(phrase: &str) -> Result<Option<String>, String> {
    if spoken_name(phrase).is_none() {
        return Ok(None);
    }
    let entries = Store::user()?.list()?;
    spoken_from(&entries, phrase)
        .map(|text| render_live(&text))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_fields_are_bounded_and_never_recursive() {
        let text = "Today {{utc_date}} at {{utc_time}}: {{clipboard_snapshot}}";
        let output = render_fields(text, "2026-11-13", "18:30", Some("{{utc_date}}")).unwrap();
        assert_eq!(output, "Today 2026-11-13 at 18:30: {{utc_date}}");
        assert!(render_fields(text, "2026-11-13", "18:30", None).is_err());
        assert_eq!(
            render_fields("{{unknown}}", "a", "b", None).unwrap(),
            "{{unknown}}"
        );
        assert!(
            render_fields("{{clipboard_snapshot}}", "a", "b", Some(&"x".repeat(9000))).is_err()
        );
    }

    #[test]
    fn spoken_snippets_are_explicit_and_case_insensitive() {
        let entries = vec![Snippet {
            trigger: "signature".into(),
            text: "Regards,\nX".into(),
        }];
        assert_eq!(
            spoken_from(&entries, "Insert snippet SIGNATURE."),
            Some("Regards,\nX".into())
        );
        assert_eq!(
            spoken_from(&entries, "expand snippet signature"),
            Some("Regards,\nX".into())
        );
        assert_eq!(spoken_from(&entries, "signature"), None);
        assert_eq!(spoken_from(&entries, "insert snippet missing"), None);
        assert_eq!(
            spoken_from(&entries, "insert snippet signature and do stuff"),
            None
        );
    }

    #[test]
    fn saves_updates_and_removes_without_duplication() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::at(tmp.path().join("x-snippets.json"));
        assert!(store.list().unwrap().is_empty());
        store.upsert("email", "Hello!").unwrap();
        store.upsert("EMAIL", "New email reply\nRegards").unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(store.list().unwrap()[0].text, "New email reply\nRegards");
        assert!(store.delete("email").unwrap());
        assert!(!store.delete("email").unwrap());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn rejects_malicious_input_and_does_not_overwrite_corrupt_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("x-snippets.json");
        let store = Store::at(path.clone());
        for trigger in [
            "",
            "not allowed",
            "too-long-a-trigger-name-that-is-absolutely-invalid",
        ] {
            assert!(store.upsert(trigger, "Hello").is_err());
        }
        assert!(store.upsert("valid", "").is_err());
        assert!(store.upsert("valid", "hi\0no").is_err());
        fs::write(&path, "this is not JSON").unwrap();
        assert!(store.upsert("new", "Should not replace").is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "this is not JSON");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_store() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        fs::write(&outside, "sensitive").unwrap();
        let path = tmp.path().join("snippets.json");
        symlink(&outside, &path).unwrap();
        let store = Store::at(path);
        assert!(store.upsert("test", "text").is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "sensitive");
    }
}

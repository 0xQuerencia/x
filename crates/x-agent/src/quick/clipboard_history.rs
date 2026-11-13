//! Explicit, opt-in text clipboard snapshots for X Quick.
//! This never monitors the clipboard or runs while X is idle. A person clicks
//! "Save current clipboard" to capture one text item on this device.
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::desktop::capture;
use crate::tools::which;

const MAX_ITEMS: usize = 100;
const MAX_TEXT_BYTES: usize = 16 * 1024;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const RETENTION_SECS: u64 = 24 * 60 * 60;
static NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub timestamp: u64,
    pub text: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    entries: Vec<Entry>,
}

pub struct Store {
    path: PathBuf,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn valid(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_TEXT_BYTES
        && !text.contains('\0')
        && !text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
}

impl Store {
    pub fn user() -> Result<Self, String> {
        Ok(Self {
            path: dirs::config_dir()
                .ok_or("No user configuration folder available")?
                .join("hex")
                .join("x-clipboard-snapshots.json"),
        })
    }

    #[cfg(test)]
    fn at(path: PathBuf) -> Self {
        Self { path }
    }

    fn load(&self) -> Result<Vec<Entry>, String> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(m) => m,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(format!("Cannot inspect clipboard snapshots: {error}")),
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_FILE_BYTES
        {
            return Err("Clipboard snapshot store is not a safe, bounded regular file".into());
        }
        let mut data = Vec::new();
        fs::File::open(&self.path)
            .and_then(|f| f.take(MAX_FILE_BYTES + 1).read_to_end(&mut data))
            .map_err(|error| format!("Cannot read clipboard snapshots: {error}"))?;
        if data.len() as u64 > MAX_FILE_BYTES {
            return Err("Clipboard snapshot store is oversized".into());
        }
        let doc: Document = serde_json::from_slice(&data)
            .map_err(|error| format!("Clipboard snapshot store is malformed: {error}"))?;
        if doc.version != 1 || doc.entries.len() > MAX_ITEMS {
            return Err("Unsupported clipboard snapshot store".into());
        }
        if !doc.entries.iter().all(|entry| valid(&entry.text)) {
            return Err("Invalid clipboard snapshot contents".into());
        }
        Ok(doc.entries)
    }

    pub fn search(&self, filter: &str) -> Result<Vec<Entry>, String> {
        let now = now();
        let needle = filter.trim().to_lowercase();
        Ok(self
            .load()?
            .into_iter()
            .filter(|e| e.timestamp <= now && now - e.timestamp < RETENTION_SECS)
            .filter(|e| e.text.to_lowercase().contains(&needle))
            .take(6)
            .collect())
    }

    pub fn save(&self, text: &str) -> Result<(), String> {
        if !valid(text) {
            return Err("Clipboard text must contain 1–16384 safe UTF-8 bytes".into());
        }
        let now = now();
        let mut entries: Vec<_> = self
            .load()?
            .into_iter()
            .filter(|e| e.timestamp <= now && now - e.timestamp < RETENTION_SECS)
            .filter(|e| e.text != text)
            .collect();
        entries.insert(
            0,
            Entry {
                timestamp: now,
                text: text.into(),
            },
        );
        entries.truncate(MAX_ITEMS);
        self.write(&entries)
    }

    pub fn clear(&self) -> Result<(), String> {
        // Remove the store, not just its visible rows.
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(m) => m,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("Cannot inspect clipboard snapshot store: {error}")),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("Refusing to clear a nonregular or symlinked store".into());
        }
        fs::remove_file(&self.path)
            .map_err(|error| format!("Cannot erase clipboard snapshots: {error}"))
    }

    fn write(&self, entries: &[Entry]) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or("No clipboard configuration folder")?;
        fs::create_dir_all(parent).map_err(|error| format!("Cannot create folder: {error}"))?;
        let meta = fs::symlink_metadata(parent)
            .map_err(|error| format!("Cannot inspect configuration folder: {error}"))?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("Refusing to write to a symlinked configuration folder".into());
        }
        let bytes = serde_json::to_vec(&Document {
            version: 1,
            entries: entries.to_vec(),
        })
        .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("Clipboard snapshot store is oversized".into());
        }
        let tmp = parent.join(format!(
            ".x-clipboard-{}-{}.tmp",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
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
            .map_err(|e| format!("Cannot save clipboard data: {e}"))?;
        let saved = (|| {
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| format!("Could not save clipboard data: {e}"))?;
            fs::rename(&tmp, &self.path).map_err(|e| format!("Cannot replace clipboard data: {e}"))
        })();
        if saved.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        saved
    }
}

/// Reads only UTF-8 text after an explicit UI click. This is not a clipboard
/// monitor and never captures images, passwords from password managers or files
/// automatically. The person must check what is copied before capturing it.
pub fn capture_now() -> Result<String, String> {
    let paste = which("wl-paste").ok_or("Install wl-clipboard to capture text on Wayland")?;
    let (ok, data) = capture(
        Command::new(paste).args(["--no-newline", "--type", "text/plain"]),
        Duration::from_secs(2),
    )?;
    if !ok || data.len() > MAX_TEXT_BYTES {
        return Err("Clipboard has no supported text or is over 16 KiB".into());
    }
    let text = String::from_utf8(data).map_err(|_| "Clipboard text is not UTF-8")?;
    Store::user()?.save(&text)?;
    Ok(format!(
        "Saved {} clipboard characters locally; expires after 24 hours",
        text.chars().count()
    ))
}

/// Called only after the user types "clip " or "clipboard ".
pub fn matches(query: &str) -> Result<Vec<Entry>, String> {
    let filter = query
        .strip_prefix("clip ")
        .or_else(|| query.strip_prefix("clipboard "))
        .or_else(|| (query == "clips" || query == "clipboard").then_some(""));
    match filter {
        Some(filter) => Store::user()?.search(filter),
        None => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_only_explicit_text_with_dedupe_and_search() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("clipboard.json"));
        store.save("one").unwrap();
        store.save("two").unwrap();
        store.save("one").unwrap();
        assert_eq!(
            store
                .search("")
                .unwrap()
                .iter()
                .map(|e| e.text.as_str())
                .collect::<Vec<_>>(),
            vec!["one", "two"]
        );
        assert_eq!(store.search("two").unwrap()[0].text, "two");
        store.clear().unwrap();
        assert!(store.search("").unwrap().is_empty());
    }

    #[test]
    fn refuses_invalid_and_corrupted_data_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clipboard.json");
        let store = Store::at(path.clone());
        assert!(store.save("hello\0world").is_err());
        assert!(store.save(&"A".repeat(MAX_TEXT_BYTES + 1)).is_err());
        fs::write(&path, "broken").unwrap();
        assert!(store.save("safe").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "broken");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_store_is_refused() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("other.json");
        fs::write(&target, "secret").unwrap();
        let path = dir.path().join("clipboard.json");
        symlink(&target, &path).unwrap();
        let store = Store::at(path);
        assert!(store.save("example").is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "secret");
    }
}

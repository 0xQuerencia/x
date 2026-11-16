//! Private, opt-in speech-correction history for explicit Actions voice mode.
//!
//! The list is never read by a network planner and never affects dictation.
//! Only a person-promoted *exact* full-utterance correction is applied.

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 40;
const MAX_SIZE: u64 = 32 * 1024;
const MAX_CHARS: usize = 120;
const PAIR_SECONDS: u64 = 20;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Entry {
    heard: String,
    meant: String,
    #[serde(default)]
    promoted: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Pending {
    heard: String,
    at: u64,
}

#[derive(Default, Deserialize, Serialize)]
struct Store {
    enabled: bool,
    #[serde(default)]
    pending: Option<Pending>,
    #[serde(default)]
    corrections: Vec<Entry>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn path() -> Result<PathBuf, String> {
    crate::app_paths::support_dir()
        .map(|dir| dir.join("x-heard.json"))
        .map_err(|_| "X support directory is unavailable".into())
}

fn read(path: &std::path::Path) -> Result<Store, String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Store::default()),
        Err(_) => return Err("Cannot inspect private correction history".into()),
    };
    if !meta.file_type().is_file() || meta.len() > MAX_SIZE {
        return Err("Private correction history is not a bounded regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("Correction history permissions are not private".into());
        }
    }
    let data = std::fs::read(path).map_err(|_| "Cannot read correction history")?;
    serde_json::from_slice(&data).map_err(|_| "Cannot decode correction history".into())
}

fn write(path: &std::path::Path, store: &Store) -> Result<(), String> {
    let content = serde_json::to_vec(store).map_err(|_| "Cannot encode correction history")?;
    if content.len() as u64 > MAX_SIZE {
        return Err("Correction history reached its size limit".into());
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            let _ = read(path)?; // refuse symlinks, insecure permissions, and corrupt stores
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("Cannot inspect private correction history".into()),
    }

    // Write to a private, exclusive temporary file in the same directory.
    // Rename only after a complete sync so a crash cannot leave a truncated log.
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    for _ in 0..16 {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temp = path.with_file_name(format!(
            ".x-heard-{}-{}-{sequence}.tmp",
            std::process::id(),
            now()
        ));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = match opts.open(&temp) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err("Cannot open private correction history".into()),
        };
        let saved = file.write_all(&content).and_then(|_| file.sync_all());
        drop(file);
        if saved.is_err() {
            let _ = std::fs::remove_file(&temp);
            return Err("Cannot save correction history".into());
        }
        let replaced = std::fs::rename(&temp, path);
        if replaced.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        return replaced.map_err(|_| "Cannot replace correction history".into());
    }
    Err("Cannot allocate private correction history file".into())
}

fn suitable(text: &str) -> Option<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.len() > 500
        || text.chars().count() > MAX_CHARS
        || text.chars().count() < 5
        || text.chars().any(char::is_control)
        || text.contains('@')
        || text.contains("://")
        || text.to_ascii_lowercase().contains("password")
        || text.to_ascii_lowercase().contains("token")
    {
        return None;
    }
    Some(text)
}

impl Store {
    fn miss(&mut self, heard: &str, at: u64) {
        if self.enabled {
            self.pending = suitable(heard).map(|heard| Pending { heard, at });
        }
    }

    fn succeeded(&mut self, meant: &str, at: u64) {
        if !self.enabled {
            return;
        }
        let Some(pending) = self.pending.take() else {
            return;
        };
        let Some(meant) = suitable(meant) else {
            return;
        };
        if at.saturating_sub(pending.at) > PAIR_SECONDS
            || pending.heard.eq_ignore_ascii_case(&meant)
        {
            return;
        }
        if self.corrections.iter().any(|row| {
            row.heard.eq_ignore_ascii_case(&pending.heard) && row.meant.eq_ignore_ascii_case(&meant)
        }) {
            return;
        }
        self.corrections.insert(
            0,
            Entry {
                heard: pending.heard,
                meant,
                promoted: false,
            },
        );
        self.corrections.truncate(MAX_ENTRIES);
    }

    fn apply(&self, heard: &str) -> String {
        if self.enabled {
            if let Some(row) = self
                .corrections
                .iter()
                .find(|row| row.promoted && row.heard.eq_ignore_ascii_case(heard.trim()))
            {
                return row.meant.clone();
            }
        }
        heard.to_string()
    }
}

pub(super) fn apply(heard: &str) -> String {
    path()
        .and_then(|p| read(&p))
        .map_or_else(|_| heard.into(), |store| store.apply(heard))
}

pub(super) fn record_miss(heard: &str) {
    if let Ok(p) = path()
        && let Ok(mut store) = read(&p)
        && store.enabled
    {
        store.miss(heard, now());
        let _ = write(&p, &store);
    }
}

pub(super) fn record_success(meant: &str) {
    if let Ok(p) = path()
        && let Ok(mut store) = read(&p)
        && store.enabled
    {
        store.succeeded(meant, now());
        let _ = write(&p, &store);
    }
}

pub(crate) fn command(command: &str, index: Option<usize>) -> Result<String, String> {
    let p = path()?;
    let mut store = read(&p)?;
    match command {
        "on" => {
            store.enabled = true;
            write(&p, &store)?;
            Ok("Local correction history enabled for explicit Actions speech only".into())
        }
        "off" => {
            store.enabled = false;
            store.pending = None;
            write(&p, &store)?;
            Ok("Local correction history disabled".into())
        }
        "list" => {
            if store.corrections.is_empty() {
                return Ok("No local corrections recorded".into());
            }
            Ok(store
                .corrections
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    format!(
                        "{}. {:?} -> {:?}{}",
                        i + 1,
                        row.heard,
                        row.meant,
                        if row.promoted { " [promoted]" } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "promote" => {
            let n = index.ok_or("Specify the correction number")?;
            let entry = store
                .corrections
                .get_mut(n.saturating_sub(1))
                .filter(|_| n > 0)
                .ok_or("No correction with that number")?;
            entry.promoted = true;
            write(&p, &store)?;
            Ok("Correction promoted; only an exact voice match will apply it".into())
        }
        "clear" => {
            store.pending = None;
            store.corrections.clear();
            write(&p, &store)?;
            Ok("Local correction history cleared".into())
        }
        _ => Err("Choose heard on, off, list, promote N, or clear".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_opt_in_learns_short_pairs_within_twenty_seconds() {
        let mut store = Store::default();
        store.miss("opne fire fox", 100);
        store.succeeded("open firefox", 105);
        assert!(store.corrections.is_empty());
        store.enabled = true;
        store.miss("opne fire fox", 100);
        store.succeeded("open firefox", 121);
        assert!(store.corrections.is_empty());
        store.miss("opne fire fox", 200);
        store.succeeded("open firefox", 219);
        assert_eq!(store.corrections.len(), 1);
        assert_eq!(store.apply("opne fire fox"), "opne fire fox");
        store.corrections[0].promoted = true;
        assert_eq!(store.apply("OPNE FIRE FOX"), "open firefox");
        assert_eq!(store.apply("please opne fire fox"), "please opne fire fox");
        store.enabled = false;
        assert_eq!(store.apply("opne fire fox"), "opne fire fox");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinks_and_world_readable_logs() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let tmp =
            std::env::temp_dir().join(format!("x-heard-test-{}-{}", std::process::id(), now()));
        std::fs::create_dir_all(&tmp).unwrap();
        let target = tmp.join("target.json");
        let link = tmp.join("link.json");
        symlink(&target, &link).unwrap();
        assert!(read(&link).is_err());
        assert!(write(&link, &Store::default()).is_err());
        let file = tmp.join("private.json");
        write(&file, &Store::default()).unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o077,
            0
        );
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read(&file).is_err());
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_file(&file).unwrap();
        std::fs::remove_dir(&tmp).unwrap();
    }

    #[test]
    fn replaces_existing_history_without_losing_new_content() {
        let dir = std::env::temp_dir().join(format!(
            "x-heard-replace-test-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("corrections.json");
        write(&file, &Store::default()).unwrap();
        assert!(!read(&file).unwrap().enabled);
        let enabled = Store {
            enabled: true,
            ..Store::default()
        };
        write(&file, &enabled).unwrap();
        assert!(read(&file).unwrap().enabled);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_file(file).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn sensitive_transcripts_are_not_retained() {
        for heard in [
            "my password is secret",
            "open https://secret.example",
            "email a@b.com",
        ] {
            assert!(suitable(heard).is_none());
        }
        let mut store = Store {
            enabled: true,
            ..Store::default()
        };
        for i in 0..90 {
            store.miss(&format!("open fake app number {i}"), i * 40);
            store.succeeded(&format!("open real app number {i}"), i * 40 + 2);
        }
        assert_eq!(store.corrections.len(), MAX_ENTRIES);
    }
}

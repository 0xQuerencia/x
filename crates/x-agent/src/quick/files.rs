//! Explicit, bounded local filename search for X Quick.
//! No content indexing, model calls, background watch, or symlink traversal.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MAX_WALK: usize = 6_000;
const MAX_DEPTH: usize = 3;
const MAX_HITS: usize = 6;
const DEADLINE: Duration = Duration::from_millis(70);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileHit {
    pub name: String,
    pub path: String,
}

fn roots(home: &Path) -> Vec<PathBuf> {
    ["Desktop", "Documents", "Downloads", "Projects", "code", "src"]
        .iter()
        .map(|folder| home.join(folder))
        .filter(|root| fs::symlink_metadata(root).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink()))
        .collect()
}

/// Search only filenames, never file contents. Common build/cache directories
/// and hidden folders are excluded, and a traversal deadline bounds UI work.
fn scan(roots: &[PathBuf], text: &str) -> Vec<FileHit> {
    let needle = text.trim().to_lowercase();
    if needle.chars().count() < 2 || needle.chars().count() > 100 {
        return Vec::new();
    }
    let start = Instant::now();
    let mut queue: VecDeque<_> = roots.iter().cloned().map(|p| (p, 0usize)).collect();
    let mut inspected = 0usize;
    let mut hits = Vec::new();
    while let Some((dir, depth)) = queue.pop_front() {
        if inspected >= MAX_WALK || start.elapsed() > DEADLINE || hits.len() >= 80 {
            break;
        }
        let Ok(read) = fs::read_dir(dir) else {
            continue;
        };
        for entry in read.flatten() {
            if inspected >= MAX_WALK || start.elapsed() > DEADLINE {
                break;
            }
            inspected += 1;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if depth < MAX_DEPTH
                    && !matches!(name.as_str(), "node_modules" | "target" | "build" | "dist" | "venv" | "__pycache__")
                {
                    queue.push_back((entry.path(), depth + 1));
                }
            } else if file_type.is_file() && name.to_lowercase().contains(&needle) {
                hits.push(FileHit {
                    name,
                    path: entry.path().to_string_lossy().into_owned(),
                });
                if hits.len() >= 80 {
                    break;
                }
            }
        }
    }
    hits.sort_by(|a, b| {
        let a_prefix = !a.name.to_lowercase().starts_with(&needle);
        let b_prefix = !b.name.to_lowercase().starts_with(&needle);
        a_prefix
            .cmp(&b_prefix)
            .then_with(|| a.name.len().cmp(&b.name.len()))
            .then_with(|| a.path.cmp(&b.path))
    });
    hits.truncate(MAX_HITS);
    hits
}

/// Triggering is explicit: "file report" or "f report" searches on this machine.
pub fn search(query: &str) -> Vec<FileHit> {
    let text = query.trim();
    let prefix = text.strip_prefix("file ").or_else(|| text.strip_prefix("f "));
    let Some(needle) = prefix else {
        return Vec::new();
    };
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    scan(&roots(&home), needle)
}

fn refused_extension(path: &Path) -> bool {
    let suffix = path.extension().and_then(|s| s.to_str()).unwrap_or_default();
    ["desktop", "sh", "exe", "bat", "cmd", "com", "msi", "ps1", "appimage", "jar", "run"]
        .iter()
        .any(|blocked| suffix.eq_ignore_ascii_case(blocked))
}

/// Open only a regular nonexecutable file in one of X Quick's scoped user roots.
/// The path is re-canonicalized before use; no shell or text interpolation occurs.
pub fn open(path: &str) -> Result<String, String> {
    if path.len() > 4096 || path.contains('\0') {
        return Err("Invalid file path".into());
    }
    let home = dirs::home_dir().ok_or("No home directory found")?;
    let allowed = roots(&home);
    let selected = Path::new(path);
    let metadata = fs::symlink_metadata(selected).map_err(|e| format!("File disappeared: {e}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("This action opens ordinary local files only".into());
    }
    let canonical = selected.canonicalize().map_err(|e| format!("File is unavailable: {e}"))?;
    if !allowed.iter().filter_map(|p| p.canonicalize().ok()).any(|root| canonical.starts_with(root)) {
        return Err("The file is outside the configured Quick search folders".into());
    }
    if refused_extension(&canonical) {
        return Err("X refuses to launch an executable file through search".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 != 0 {
            return Err("X refuses to launch an executable file through search".into());
        }
    }
    #[cfg(target_os = "windows")]
    let opener = "explorer.exe";
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let opener = "xdg-open";

    let mut child = Command::new(opener)
        .arg(&canonical)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not open this file with your desktop: {e}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok("Sent to the system file opener; X cannot verify that it opened".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_scans_given_roots_and_skips_hidden_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Documents");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("project-report.pdf"), "data").unwrap();
        fs::write(root.join(".project-secret"), "secret").unwrap();
        fs::write(tmp.path().join("another-project-report.pdf"), "other").unwrap();
        let results = scan(&[root], "project");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "project-report.pdf");
    }

    #[test]
    fn refuses_executable_extension_and_blank_queries() {
        for name in ["run.sh", "app.desktop", "helper.exe", "installer.msi", "test.jar"] {
            assert!(refused_extension(Path::new(name)), "{name}");
        }
        assert!(scan(&[], "").is_empty());
        assert!(scan(&[], "a").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_symlinked_files() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Documents");
        fs::create_dir(&root).unwrap();
        fs::write(tmp.path().join("private-report.txt"), "private").unwrap();
        symlink(tmp.path().join("private-report.txt"), root.join("linked-report.txt")).unwrap();
        assert!(scan(&[root], "report").is_empty());
    }
}

//! Explicit, bounded local filename search for X Quick.
//! No content indexing, model calls, background watch, or symlink traversal.

use std::collections::VecDeque;
use std::fs;
use std::io::Read;
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
    [
        "Desktop",
        "Documents",
        "Downloads",
        "Projects",
        "code",
        "src",
    ]
    .iter()
    .map(|folder| home.join(folder))
    .filter(|root| {
        fs::symlink_metadata(root).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
    })
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
                    && !matches!(
                        name.as_str(),
                        "node_modules" | "target" | "build" | "dist" | "venv" | "__pycache__"
                    )
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
    let prefix = text
        .strip_prefix("file ")
        .or_else(|| text.strip_prefix("f "))
        .or_else(|| text.strip_prefix("read "))
        .or_else(|| text.strip_prefix("view "));
    let Some(needle) = prefix else {
        return Vec::new();
    };
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    scan(&roots(&home), needle)
}

fn refused_extension(path: &Path) -> bool {
    let suffix = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    [
        "desktop", "sh", "exe", "bat", "cmd", "com", "msi", "ps1", "appimage", "jar", "run",
    ]
    .iter()
    .any(|blocked| suffix.eq_ignore_ascii_case(blocked))
}

/// Verify a search result against currently approved roots, not just the
/// filename captured earlier. Never trust a stale Quick result path.
fn scoped_regular_file(path: &str) -> Result<PathBuf, String> {
    if path.len() > 4096 || path.contains('\0') {
        return Err("Invalid file path".into());
    }
    let home = dirs::home_dir().ok_or("No home directory found")?;
    let allowed = roots(&home);
    let selected = Path::new(path);
    let metadata = fs::symlink_metadata(selected).map_err(|e| format!("File disappeared: {e}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("This action accepts ordinary local files only".into());
    }
    let canonical = selected
        .canonicalize()
        .map_err(|e| format!("File is unavailable: {e}"))?;
    if !allowed
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .any(|root| canonical.starts_with(root))
    {
        return Err("The file is outside the configured Quick search folders".into());
    }
    if refused_extension(&canonical) {
        return Err("X refuses executable files through Quick".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 != 0 {
            return Err("X refuses executable files through Quick".into());
        }
    }
    Ok(canonical)
}

/// Explicitly preview a small ordinary UTF-8 text file. Never send content to
/// the model, clipboard, history store, terminal, or an external application.
/// Unsupported extensions and oversized files are refused before reading.
pub fn preview(path: &str) -> Result<String, String> {
    let canonical = scoped_regular_file(path)?;
    read_preview(&canonical)
}

fn read_preview(canonical: &Path) -> Result<String, String> {
    const LIMIT: u64 = 8 * 1024;
    let extension = canonical.extension().and_then(|ext| ext.to_str()).unwrap_or_default();
    if ![
        "txt", "md", "rs", "toml", "json", "yaml", "yml", "csv", "log",
        "py", "js", "jsx", "ts", "tsx", "html", "css", "xml", "go",
        "lua", "java", "c", "cpp", "h", "hpp", "sql",
    ]
    .iter()
    .any(|allowed| extension.eq_ignore_ascii_case(allowed))
    {
        return Err("Only known text-file extensions can be previewed".into());
    }
    let metadata = fs::metadata(&canonical).map_err(|e| format!("Cannot inspect file: {e}"))?;
    if metadata.len() > LIMIT {
        return Err("File is larger than the 8 KiB preview limit; open it in an editor".into());
    }
    let mut file = fs::File::open(&canonical).map_err(|e| format!("Cannot read file: {e}"))?;
    let mut data = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut data)
        .map_err(|e| format!("Cannot read file: {e}"))?;
    if data.len() as u64 > LIMIT {
        return Err("File changed or exceeds the 8 KiB preview limit".into());
    }
    let text = String::from_utf8(data).map_err(|_| "File is not UTF-8 text")?;
    if text.contains('\0') {
        return Err("Binary-looking file cannot be previewed".into());
    }
    Ok(text.chars().take(2_000).collect())
}

/// Open only a regular nonexecutable file in one of X Quick's scoped user roots.
/// Opening is explicit, with no shell or text interpolation.
pub fn open(path: &str) -> Result<String, String> {
    let canonical = scoped_regular_file(path)?;
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
    fn text_previews_are_bounded_and_do_not_decode_binaries() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("notes.md");
        fs::write(&file, "A short line").unwrap();
        assert_eq!(read_preview(&file).unwrap(), "A short line");
        fs::write(&file, vec![b'x'; 8 * 1024 + 1]).unwrap();
        assert!(read_preview(&file).is_err());
        let binary = tmp.path().join("notes.bin");
        fs::write(&binary, "plain text").unwrap();
        assert!(read_preview(&binary).is_err());
        fs::write(&file, b"Hello\\0binary").unwrap();
        assert!(read_preview(&file).is_err());
    }

    #[test]
    fn file_search_requires_an_explicit_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Documents");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("sample-notes.txt"), "safe").unwrap();
        assert_eq!(scan(&[root], "notes").len(), 1);
        assert!(search("example").is_empty());
    }

    #[test]
    fn refuses_executable_extension_and_blank_queries() {
        for name in [
            "run.sh",
            "app.desktop",
            "helper.exe",
            "installer.msi",
            "test.jar",
        ] {
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
        symlink(
            tmp.path().join("private-report.txt"),
            root.join("linked-report.txt"),
        )
        .unwrap();
        assert!(scan(&[root], "report").is_empty());
    }
}

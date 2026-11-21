//! User and machine Start Menu shortcuts: Windows' installed-app catalog.
//! A shortcut is data, never interpreted as a command or used as a shell script.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::apps::Entry;

const MAX_LINKS: usize = 3000;
const MAX_DEPTH: usize = 5;
const MAX_SIZE: u64 = 128 * 1024;

fn windows_name(title: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    let lower = title.trim().to_ascii_lowercase();
    let exe = match lower.as_str() {
        "visual studio code" | "vs code" => "code",
        "google chrome" => "chrome",
        "microsoft edge" => "msedge",
        "mozilla firefox" => "firefox",
        "brave browser" => "brave",
        "windows terminal" | "terminal" => "windowsterminal",
        "file explorer" | "windows explorer" => "explorer",
        "notepad++" => "notepad++",
        _ => lower.as_str(),
    };
    let mut names = vec![lower.clone(), exe.into()];
    if lower == "visual studio code" {
        names.push("vscode".into());
    }
    let mut classes = vec![exe.into()];
    if lower == "file explorer" {
        classes.push("cabinetwclass".into());
    }
    let mut categories = Vec::new();
    if [
        "chrome", "firefox", "msedge", "brave", "opera", "vivaldi", "chromium",
    ]
    .contains(&exe)
    {
        categories.push("webbrowser".into());
    } else if [
        "windowsterminal",
        "powershell",
        "pwsh",
        "cmd",
        "command prompt",
    ]
    .contains(&exe)
    {
        categories.push("terminalemulator".into());
    } else if exe == "explorer" {
        categories.push("filemanager".into());
    } else if ["code", "notepad", "notepad++"].contains(&exe) {
        categories.push("texteditor".into());
    }
    names.sort();
    names.dedup();
    classes.sort();
    classes.dedup();
    (names, classes, categories)
}

fn entry(link: &Path) -> Option<Entry> {
    if !link.extension()?.to_str()?.eq_ignore_ascii_case("lnk") {
        return None;
    }
    let metadata = std::fs::symlink_metadata(link).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_SIZE {
        return None;
    }
    let title = link.file_stem()?.to_str()?.trim();
    if title.len() < 2 || title.len() > 128 || title.chars().any(char::is_control) {
        return None;
    }
    let (names, classes, categories) = windows_name(title);
    Some(Entry {
        id: link.file_name()?.to_string_lossy().to_string(),
        path: link.to_owned(),
        exec: String::new(), // Windows .lnk target must be resolved by the OS.
        terminal: false,
        names,
        classes,
        categories,
    })
}

fn start_menu_paths() -> Vec<PathBuf> {
    let suffix = Path::new("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs");
    let mut dirs = Vec::new();
    if let Some(user) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(user).join(&suffix));
    }
    if let Some(shared) = std::env::var_os("PROGRAMDATA") {
        dirs.push(PathBuf::from(shared).join(&suffix));
    }
    dirs
}

fn scan_dirs(dirs: &[PathBuf]) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut stack: Vec<(PathBuf, usize)> = dirs.iter().cloned().map(|path| (path, 0)).collect();
    let mut checked = 0;
    while let Some((directory, depth)) = stack.pop() {
        let Ok(contents) = std::fs::read_dir(&directory) else {
            continue;
        };
        for dirent in contents.flatten() {
            if checked >= MAX_LINKS {
                break;
            }
            let path = dirent.path();
            let Ok(kind) = dirent.file_type() else {
                continue;
            };
            // Never follow symlink/reparse-point directory recursion.
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() && depth < MAX_DEPTH {
                stack.push((path, depth + 1));
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            checked += 1;
            if let Some(app) = entry(&path) {
                // Keep distinct links even when they share a display name.
                // The catalog's exact-name resolver must see the ambiguity
                // instead of silently choosing whichever link was scanned first.
                if seen.insert(app.path.clone()) {
                    out.push(app);
                }
            }
        }
    }
    out
}

pub(super) fn scan() -> Vec<Entry> {
    scan_dirs(&start_menu_paths())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_names_are_not_shell_commands() {
        let (names, classes, roles) = windows_name("Google Chrome");
        assert!(names.contains(&"chrome".to_string()));
        assert_eq!(classes, ["chrome"]);
        assert_eq!(roles, ["webbrowser"]);
        let (names, classes, _) = windows_name("Visual Studio Code");
        assert!(names.contains(&"vscode".to_string()));
        assert_eq!(classes, ["code"]);
    }

    #[test]
    fn duplicate_shortcut_names_remain_ambiguous() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().join("user");
        let machine = temp.path().join("machine");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&machine).unwrap();
        std::fs::write(user.join("Example App.lnk"), b"user link").unwrap();
        std::fs::write(machine.join("Example App.lnk"), b"machine link").unwrap();
        let found = scan_dirs(&[user, machine]);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].names, found[1].names);
        assert_ne!(found[0].path, found[1].path);
    }

    #[test]
    fn start_menu_never_scans_arbitrary_files() {
        let temp = tempfile::tempdir().unwrap();
        let normal = temp.path().join("Example App.lnk");
        std::fs::write(&normal, b"fake shortcut metadata").unwrap();
        let script = temp.path().join("Example App.cmd");
        std::fs::write(&script, b"echo bad").unwrap();
        let found = scan_dirs(&[temp.path().to_owned()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, normal);
        assert!(found[0].exec.is_empty());
    }
}

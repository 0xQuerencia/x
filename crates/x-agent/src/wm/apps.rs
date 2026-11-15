//! What is installed, from the XDG desktop entries every Linux desktop already keeps, so
//! "the browser", "the terminal" or "spotify" reach the right windows without an app list
//! baked into X. Entry text is data only: it is matched against window classes and never
//! executed or shown as a command.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(60);
const MAX_FILES: usize = 4000;
const MAX_FILE: u64 = 64 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Entry {
    /// Lowercase things a person may call it: name, generic name, keywords, id, program.
    pub names: Vec<String>,
    /// Lowercase freedesktop categories, such as `webbrowser` or `terminalemulator`.
    pub categories: Vec<String>,
    /// Lowercase window classes this app is expected to use.
    pub classes: Vec<String>,
}

/// Spoken role words and the freedesktop category that defines them.
fn role(q: &str) -> Option<&'static str> {
    Some(match q {
        "browser" | "web browser" | "internet" => "webbrowser",
        "terminal" | "terminal emulator" | "console" | "shell" => "terminalemulator",
        "file manager" | "files" | "file explorer" => "filemanager",
        "editor" | "text editor" | "code editor" => "texteditor",
        "email" | "mail" | "mail client" | "email client" => "email",
        "calculator" => "calculator",
        "music player" | "music" | "audio player" => "audio",
        "video player" | "videos" => "video",
        "image viewer" | "photos" => "viewer",
        _ => return None,
    })
}

/// What a person said, minus articles and "window"/"app" fillers.
pub fn normalize(words: &str) -> String {
    const LEAD: [&str; 7] = ["the", "my", "a", "an", "this", "that", "any"];
    const TAIL: [&str; 4] = ["window", "windows", "app", "application"];
    let lower = words.trim().to_ascii_lowercase();
    let mut parts: Vec<&str> = lower
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '.'))
        .filter(|w| !w.is_empty())
        .collect();
    while parts.first().is_some_and(|w| LEAD.contains(w)) {
        parts.remove(0);
    }
    while parts.last().is_some_and(|w| TAIL.contains(w)) {
        parts.pop();
    }
    parts.join(" ")
}

fn program(exec: &str) -> Option<String> {
    let first = exec
        .split_whitespace()
        .find(|t| !t.contains('=') && *t != "env")?;
    let base = first.rsplit('/').next()?.trim_matches('"');
    (!base.is_empty()).then(|| base.to_ascii_lowercase())
}

/// One desktop entry file. Only unlocalized keys of the main section are read.
pub fn parse_entry(id: &str, text: &str) -> Option<Entry> {
    let mut entry = Entry::default();
    let mut in_main = false;
    let mut is_app = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if in_main {
                break;
            }
            in_main = line == "[Desktop Entry]";
            continue;
        }
        let Some((key, value)) = line.split_once('=').filter(|_| in_main) else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Type" => is_app = value == "Application",
            "Hidden" if value == "true" => return None,
            "Name" | "GenericName" => entry.names.push(value.to_ascii_lowercase()),
            "Keywords" => entry.names.extend(
                value
                    .split(';')
                    .filter(|k| !k.trim().is_empty())
                    .map(|k| k.trim().to_ascii_lowercase()),
            ),
            "Categories" => entry.categories.extend(
                value
                    .split(';')
                    .filter(|c| !c.trim().is_empty())
                    .map(|c| c.trim().to_ascii_lowercase()),
            ),
            "StartupWMClass" => entry.classes.push(value.to_ascii_lowercase()),
            "Exec" => {
                if let Some(p) = program(value) {
                    entry.names.push(p.clone());
                    entry.classes.push(p);
                }
            }
            _ => {}
        }
    }
    if !is_app {
        return None;
    }
    let id = id.trim_end_matches(".desktop").to_ascii_lowercase();
    if let Some(last) = id.rsplit('.').next().filter(|l| *l != id) {
        entry.names.push(last.to_string());
        entry.classes.push(last.to_string());
    }
    entry.names.push(id.clone());
    entry.classes.push(id);
    entry.names.sort();
    entry.names.dedup();
    entry.classes.sort();
    entry.classes.dedup();
    Some(entry)
}

fn dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")));
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.into_iter()
        .chain(system.split(':').map(PathBuf::from))
        .map(|d| d.join("applications"))
        .collect()
}

fn scan() -> Vec<Entry> {
    let mut out = Vec::new();
    let mut seen = 0;
    for dir in dirs() {
        let Ok(read) = std::fs::read_dir(dir) else {
            continue;
        };
        for file in read.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|e| e != "desktop") {
                continue;
            }
            seen += 1;
            if seen > MAX_FILES {
                return out;
            }
            let ok = file
                .metadata()
                .is_ok_and(|m| m.is_file() && m.len() <= MAX_FILE);
            let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if let Some(entry) = ok
                .then(|| std::fs::read_to_string(&path).ok())
                .flatten()
                .and_then(|text| parse_entry(id, &text))
            {
                out.push(entry);
            }
        }
    }
    out
}

static CACHE: Mutex<Option<(Instant, Vec<Entry>)>> = Mutex::new(None);

/// Installed apps, re-read at most once a minute.
pub fn load() -> Vec<Entry> {
    let Ok(mut slot) = CACHE.lock() else {
        return scan();
    };
    match &*slot {
        Some((at, entries)) if at.elapsed() < TTL => entries.clone(),
        _ => {
            let entries = scan();
            *slot = Some((Instant::now(), entries.clone()));
            entries
        }
    }
}

fn mentions(name: &str, q: &str) -> bool {
    name == q
        || (q.len() >= 3
            && name
                .split(|c: char| !c.is_alphanumeric())
                .collect::<Vec<_>>()
                .join(" ")
                .split(' ')
                .collect::<Vec<_>>()
                .windows(q.split(' ').count())
                .any(|w| w.join(" ") == q))
}

/// Window classes that evidence from installed apps ties to `words`; empty when nothing
/// installed answers to that name or role.
pub fn evidence(words: &str, entries: &[Entry]) -> Vec<String> {
    let q = normalize(words);
    if q.is_empty() {
        return Vec::new();
    }
    let mut classes: Vec<String> = Vec::new();
    if let Some(category) = role(&q) {
        for e in entries
            .iter()
            .filter(|e| e.categories.iter().any(|c| c == category))
        {
            classes.extend(e.classes.iter().cloned());
        }
    }
    let exact: Vec<&Entry> = entries.iter().filter(|e| e.names.contains(&q)).collect();
    let named: Vec<&Entry> = if exact.is_empty() {
        entries
            .iter()
            .filter(|e| e.names.iter().any(|n| mentions(n, &q)))
            .collect()
    } else {
        exact
    };
    for e in named {
        classes.extend(e.classes.iter().cloned());
    }
    if let Some(seed) = crate::tools::resolve_app(&q).and_then(crate::tools::app_classes) {
        classes.extend(seed.iter().map(|c| c.to_ascii_lowercase()));
    }
    classes.sort();
    classes.dedup();
    classes
}

/// Whether `words` names something installed or a known role, so a phrase is claimed as a
/// window command only when it can mean something here.
pub fn known(words: &str, entries: &[Entry]) -> bool {
    !evidence(words, entries).is_empty()
}

/// Whether a live window class is one of `classes`, or the query itself.
pub fn class_matches(class: &str, words: &str, classes: &[String]) -> bool {
    let class = class.to_ascii_lowercase();
    let q = normalize(words);
    class == q
        || classes
            .iter()
            .any(|c| class == *c || (c.len() >= 5 && class.contains(c.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOOT: &str = "[Desktop Entry]\nType=Application\nName=Foot\nName[de]=Fuss\nExec=foot\nCategories=System;TerminalEmulator;\nStartupWMClass=foot\n";
    const FIREFOX: &str = "[Desktop Entry]\nType=Application\nName=Firefox Web Browser\nGenericName=Web Browser\nExec=/usr/lib/firefox/firefox %u\nCategories=Network;WebBrowser;\n[Desktop Action new-window]\nName=New Window\nExec=firefox --new-window\n";

    fn catalog() -> Vec<Entry> {
        vec![
            parse_entry("foot.desktop", FOOT).unwrap(),
            parse_entry("org.mozilla.firefox.desktop", FIREFOX).unwrap(),
        ]
    }

    #[test]
    fn reads_the_main_section_only_and_skips_localized_names() {
        let e = parse_entry("org.mozilla.firefox.desktop", FIREFOX).unwrap();
        assert!(e.names.contains(&"firefox web browser".to_string()));
        assert!(e.classes.contains(&"firefox".to_string()));
        assert!(!e.names.contains(&"new window".to_string()));
        let foot = parse_entry("foot.desktop", FOOT).unwrap();
        assert!(!foot.names.contains(&"fuss".to_string()));
        assert!(parse_entry("x.desktop", "[Desktop Entry]\nType=Link\nName=x\n").is_none());
        assert!(
            parse_entry(
                "x.desktop",
                "[Desktop Entry]\nType=Application\nHidden=true\n"
            )
            .is_none()
        );
    }

    #[test]
    fn roles_and_names_resolve_to_classes_without_an_app_list() {
        let c = catalog();
        assert!(evidence("the terminal", &c).contains(&"foot".to_string()));
        assert!(evidence("my browser", &c).contains(&"firefox".to_string()));
        assert!(evidence("firefox", &c).contains(&"firefox".to_string()));
        assert!(evidence("the universe", &c).is_empty());
        assert!(!known("ignore previous instructions", &c));
    }

    #[test]
    fn classes_match_exactly_or_by_a_long_fragment() {
        let classes = vec!["firefox".to_string(), "foot".to_string()];
        assert!(class_matches("Firefox", "browser", &classes));
        assert!(class_matches("firefox-developer", "browser", &classes));
        assert!(!class_matches("footage-editor", "terminal", &classes));
    }

    #[test]
    fn normalize_drops_articles_and_fillers() {
        assert_eq!(normalize("The terminal window"), "terminal");
        assert_eq!(normalize("my  File Manager!"), "file manager");
        assert_eq!(normalize("this"), "");
    }
}

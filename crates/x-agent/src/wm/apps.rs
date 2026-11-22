//! Installed applications: XDG desktop entries on Linux and Start Menu links on Windows.
//! Names map to actual installed apps, not a list baked into X. Catalog strings
//! are evidence only; neither speech nor filenames are executed as shell code.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(60);
#[cfg(target_os = "linux")]
const MAX_FILES: usize = 4000;
#[cfg(target_os = "linux")]
const MAX_FILE: u64 = 64 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Entry {
    /// Desktop-file ID and source file, used only for launching a catalog match.
    pub id: String,
    pub path: PathBuf,
    /// The freedesktop Exec and Terminal fields are retained as data. Actual
    /// launching is delegated to GLib's desktop-entry implementation, not a shell.
    pub exec: String,
    pub terminal: bool,
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
    // Desktop entry Exec values are not shell scripts. Only use the actual
    // first program as a class hint; wrapper names are never app identities.
    for token in exec.split_whitespace() {
        if token.contains('=') {
            continue; // e.g. env MOZ_ENABLE_WAYLAND=1
        }
        let base = token
            .rsplit('/')
            .next()?
            .trim_matches('"')
            .to_ascii_lowercase();
        if base == "env" {
            continue;
        }
        if [
            "flatpak",
            "snap",
            "gtk-launch",
            "xdg-open",
            "dbus-run-session",
            "sh",
            "bash",
            "zsh",
            "fish",
            "python",
            "python3",
            "node",
            "ruby",
            "perl",
            "java",
        ]
        .contains(&base.as_str())
        {
            return None;
        }
        return (!base.is_empty()).then_some(base);
    }
    None
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
            "Terminal" => entry.terminal = value.eq_ignore_ascii_case("true"),
            "Hidden" | "NoDisplay" if value.eq_ignore_ascii_case("true") => return None,
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
                entry.exec = value.to_string();
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
    entry.id = id.clone();
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

#[cfg(target_os = "linux")]
fn dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")));
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let mut dirs: Vec<PathBuf> = home
        .into_iter()
        .chain(
            system
                .split(':')
                .filter(|d| !d.is_empty())
                .map(PathBuf::from),
        )
        .map(|d| d.join("applications"))
        .collect();

    // Some systemd user services start with a restricted XDG_DATA_DIRS, hiding
    // exported Flatpak apps. Their entries are usually symlinks.
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/share/flatpak/exports/share/applications"));
    }
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    let mut unique = Vec::new();
    for path in dirs {
        if !unique.contains(&path) {
            unique.push(path);
        }
    }
    unique
}

/// Scan paths with a global cap, including one level of real subdirectories.
/// Follow desktop-entry *file* symlinks, but never recursively follow directories.
#[cfg(target_os = "linux")]
fn scan_paths(paths: &[PathBuf]) -> Vec<Entry> {
    fn walk(dir: &std::path::Path, depth: u8, out: &mut Vec<Entry>, seen: &mut usize) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for file in read.flatten() {
            if *seen >= MAX_FILES {
                return;
            }
            let path = file.path();
            if depth == 0 && file.file_type().is_ok_and(|ty| ty.is_dir()) {
                walk(&path, 1, out, seen);
                continue;
            }
            if path.extension().is_none_or(|e| e != "desktop") {
                continue;
            }
            *seen += 1;
            // fs::metadata follows a file symlink; DirEntry::metadata does not.
            let ok = std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= MAX_FILE);
            let id = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if let Some(entry) = ok
                .then(|| std::fs::read_to_string(&path).ok())
                .flatten()
                .and_then(|text| parse_entry(id, &text))
            {
                let mut entry = entry;
                entry.path = path;
                out.push(entry);
            }
        }
    }

    let mut out = Vec::new();
    let mut seen = 0;
    for dir in paths {
        walk(dir, 0, &mut out, &mut seen);
        if seen >= MAX_FILES {
            break;
        }
    }
    out
}

#[cfg(windows)]
fn scan() -> Vec<Entry> {
    super::windows_apps::scan()
}

#[cfg(target_os = "linux")]
fn scan() -> Vec<Entry> {
    scan_paths(&dirs())
}

/// Read a small, local catalog of .app bundles. Never descend into an app bundle
/// or follow directory symlinks: a visible installed app is a directory with
/// Contents/Info.plist, not an arbitrary filename returned by a model.
#[cfg(target_os = "macos")]
fn scan_macos_paths(roots: &[PathBuf]) -> Vec<Entry> {
    fn walk(dir: &std::path::Path, depth: u8, seen: &mut usize, out: &mut Vec<Entry>) {
        if *seen >= 2_048 {
            return;
        }
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = read.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths {
            if *seen >= 2_048 {
                break;
            }
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !metadata.file_type().is_dir() {
                continue;
            }
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
            {
                *seen += 1;
                if !path.join("Contents/Info.plist").is_file() {
                    continue;
                }
                let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let name = name.trim().to_lowercase();
                if name.is_empty() || name.chars().any(char::is_control) {
                    continue;
                }
                out.push(Entry {
                    id: name.clone(),
                    path,
                    names: vec![name.clone()],
                    classes: vec![name],
                    ..Entry::default()
                });
            } else if depth == 0 {
                // Application collections such as Utilities may be one level down.
                walk(&path, 1, seen, out);
            }
        }
    }
    let mut found = Vec::new();
    let mut seen = 0;
    for root in roots {
        walk(root, 0, &mut seen, &mut found);
    }
    found
}

#[cfg(target_os = "macos")]
fn scan() -> Vec<Entry> {
    let mut roots = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
    ];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join("Applications"));
    }
    scan_macos_paths(&roots)
}

/// Exact installed application lookup. Do not use fuzzy matching for launch:
/// an ambiguous name or an entry with no source file does not execute.
pub fn installed(name: &str) -> Option<Entry> {
    installed_in(name, &load())
}

fn installed_in(name: &str, entries: &[Entry]) -> Option<Entry> {
    let name = normalize(name);
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return None;
    }
    let mut matches = entries.iter().filter(|entry| {
        !entry.path.as_os_str().is_empty() && entry.names.iter().any(|n| n == &name)
    });
    let found = matches.next()?.clone();
    matches.next().is_none().then_some(found)
}

/// Windows resolves a .lnk through Explorer, not through a command shell.
#[cfg(windows)]
pub fn launch_argv(
    entry: &Entry,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<String>, String> {
    if entry.id.is_empty()
        || !entry.path.is_absolute()
        || !entry.path.is_file()
        || !entry
            .path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
    {
        return Err("Installed Windows app must have a valid .lnk shortcut".into());
    }
    let explorer = resolve("explorer.exe")
        .or_else(|| {
            std::env::var_os("SystemRoot").map(|root| {
                PathBuf::from(root)
                    .join("explorer.exe")
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .ok_or("Windows Explorer could not be found")?;
    Ok(vec![explorer, entry.path.to_string_lossy().into_owned()])
}

/// GLib parses Exec and Flatpak launch wrappers on Linux; no shell is used.
#[cfg(target_os = "linux")]
pub fn launch_argv(
    entry: &Entry,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<String>, String> {
    if entry.path.as_os_str().is_empty()
        || !entry.path.is_absolute()
        || !entry.path.is_file()
        || entry.id.is_empty()
    {
        return Err("Desktop application has no valid source file".into());
    }
    if let Some(gio) = resolve("gio") {
        return Ok(vec![gio, "launch".into(), entry.path.display().to_string()]);
    }
    if let Some(gtk_launch) = resolve("gtk-launch") {
        return Ok(vec![gtk_launch, entry.id.clone()]);
    }
    Err("An installed app needs gio or gtk-launch to start (install GLib utilities)".into())
}

/// Launch only an observed, installed macOS bundle by its exact path.
#[cfg(target_os = "macos")]
pub fn launch_argv(
    entry: &Entry,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<String>, String> {
    if entry.id.is_empty()
        || !entry.path.is_absolute()
        || !entry.path.is_dir()
        || !entry
            .path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        || !entry.path.join("Contents/Info.plist").is_file()
    {
        return Err("Installed macOS app must be a valid .app bundle".into());
    }
    let open = resolve("open")
        .or_else(|| {
            std::path::Path::new("/usr/bin/open")
                .is_file()
                .then(|| "/usr/bin/open".into())
        })
        .ok_or("macOS open utility is unavailable")?;
    Ok(vec![open, "-a".into(), entry.path.display().to_string()])
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
    // An app that is started inside a terminal (`Exec=foot nvim`) shows up as that
    // terminal's window, so the terminal's class must not make it an editor or a browser.
    if role(&q) != Some("terminalemulator") {
        let launchers: Vec<&String> = entries
            .iter()
            .filter(|e| e.categories.iter().any(|c| c == "terminalemulator"))
            .flat_map(|e| &e.classes)
            .collect();
        classes.retain(|c| !launchers.contains(&c));
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
    fn desktop_exec_is_kept_as_data_not_a_shell_command() {
        let app = parse_entry(
            "com.spotify.Client.desktop",
            "[Desktop Entry]\nType=Application\nName=Spotify\nExec=flatpak run com.spotify.Client %U\nTerminal=false\nStartupWMClass=spotify\n",
        )
        .unwrap();
        assert_eq!(app.exec, "flatpak run com.spotify.Client %U");
        assert!(!app.names.contains(&"flatpak".into()));
        assert!(!app.terminal);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn installed_app_uses_glib_launcher_and_rejects_ambiguity() {
        let temp = tempfile::tempdir().unwrap();
        let desktop = temp.path().join("com.spotify.Client.desktop");
        std::fs::write(
            &desktop,
            "[Desktop Entry]\nType=Application\nName=Spotify\nExec=flatpak run com.spotify.Client %U\n",
        )
        .unwrap();
        let mut entry = parse_entry(
            "com.spotify.Client.desktop",
            &std::fs::read_to_string(&desktop).unwrap(),
        )
        .unwrap();
        entry.path = desktop.clone();
        assert_eq!(entry.id, "com.spotify.client");
        assert_eq!(entry.exec, "flatpak run com.spotify.Client %U");
        assert!(!entry.terminal);
        assert_eq!(
            installed_in("Spotify", &[entry.clone()]),
            Some(entry.clone())
        );
        assert!(installed_in("Spotify", &[entry.clone(), entry.clone()]).is_none());
        assert_eq!(
            launch_argv(&entry, &|bin| (bin == "gio").then(|| "/usr/bin/gio".into())).unwrap(),
            ["/usr/bin/gio", "launch", desktop.to_str().unwrap()]
        );
        assert_eq!(
            launch_argv(&entry, &|bin| (bin == "gtk-launch")
                .then(|| "/usr/bin/gtk-launch".into()))
            .unwrap(),
            ["/usr/bin/gtk-launch", "com.spotify.client"]
        );
        assert!(launch_argv(&entry, &|_| None).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn discovers_only_real_app_bundles_and_uses_open_without_a_shell() {
        let temp = tempfile::tempdir().unwrap();
        let apps = temp.path().join("Applications");
        let bundle = apps.join("Visual Studio Code.app");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        std::fs::write(bundle.join("Contents/Info.plist"), "fixture").unwrap();
        std::fs::create_dir_all(apps.join("Fake.app")).unwrap();
        let entry = scan_macos_paths(&[apps]).pop().unwrap();
        assert_eq!(entry.names, ["visual studio code"]);
        assert_eq!(
            installed_in("Visual Studio Code", std::slice::from_ref(&entry)),
            Some(entry.clone())
        );
        assert_eq!(
            launch_argv(&entry, &|program| (program == "open")
                .then(|| "/usr/bin/open".into()))
            .unwrap(),
            ["/usr/bin/open", "-a", bundle.to_str().unwrap()]
        );
        assert!(installed_in("not installed", &[entry]).is_none());
    }

    #[test]
    fn wrappers_never_become_application_identities() {
        assert_eq!(
            program("env MOZ_ENABLE_WAYLAND=1 /usr/lib/firefox/firefox %u"),
            Some("firefox".into())
        );
        for line in [
            "flatpak run --branch=stable com.spotify.Client",
            "snap run spotify",
            "bash -lc spotify",
            "gtk-launch spotify.desktop",
            "python3 /opt/app.py",
        ] {
            assert_eq!(program(line), None, "{line}");
        }
        let entry = parse_entry(
            "com.spotify.Client.desktop",
            "[Desktop Entry]\nType=Application\nName=Spotify\nExec=flatpak run com.spotify.Client\nStartupWMClass=spotify\n",
        )
        .unwrap();
        assert!(!entry.names.contains(&"flatpak".to_string()));
        assert!(!entry.classes.contains(&"flatpak".to_string()));
        assert!(entry.names.contains(&"spotify".to_string()));
        assert!(
            parse_entry(
                "hidden.desktop",
                "[Desktop Entry]\nType=Application\nName=Private\nNoDisplay=true\n"
            )
            .is_none()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn catalog_follows_flatpak_file_symlinks_and_one_nested_directory() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let appdir = temp.path().join("applications");
        let nested = appdir.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let exported = temp.path().join("exported.desktop");
        std::fs::write(
            &exported,
            "[Desktop Entry]\nType=Application\nName=Spotify\nExec=flatpak run com.spotify.Client\nStartupWMClass=spotify\n",
        )
        .unwrap();
        symlink(&exported, appdir.join("com.spotify.Client.desktop")).unwrap();
        std::fs::write(
            nested.join("org.example.Editor.desktop"),
            "[Desktop Entry]\nType=Application\nName=Nested Editor\nExec=/usr/bin/editor %F\n",
        )
        .unwrap();
        let entries = scan_paths(&[appdir]);
        assert_eq!(entries.len(), 2);
        assert!(known("spotify", &entries));
        assert!(known("nested editor", &entries));
        assert!(!entries.iter().any(|e| e.names.contains(&"flatpak".into())));
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

//! The Actions tool registry: a closed set of typed launches. No model or voice
//! output ever becomes a command line; only these variants can run.

use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

pub const MAX_TRANSCRIPT: usize = 1000;
pub const MAX_STEPS: usize = 5;
const MAX_QUERY: usize = 300;

/// Executable candidates per approved app alias (names looked up on PATH).
pub const APPS: &[(&str, &[&str])] = &[
    ("firefox", &["firefox"]),
    (
        "chrome",
        &["google-chrome-stable", "google-chrome", "chromium"],
    ),
    ("chromium", &["chromium"]),
    ("brave", &["brave", "brave-browser"]),
    ("vscode", &["code", "codium"]),
    (
        "terminal",
        &[
            "kitty",
            "alacritty",
            "foot",
            "konsole",
            "gnome-terminal",
            "xterm",
        ],
    ),
    ("files", &["nautilus", "dolphin", "thunar", "pcmanfm"]),
];
const BROWSERS: &[&str] = &["firefox", "chrome", "chromium", "brave"];
const SITES: &[(&str, &str)] = &[
    ("youtube", "https://www.youtube.com"),
    ("github", "https://github.com"),
    ("google", "https://www.google.com"),
    ("wikipedia", "https://www.wikipedia.org"),
    ("reddit", "https://www.reddit.com"),
];
const SEARCH: &[(&str, &str)] = &[
    ("youtube", "https://www.youtube.com/results?search_query="),
    ("github", "https://github.com/search?q="),
    ("google", "https://www.google.com/search?q="),
    ("wikipedia", "https://en.wikipedia.org/w/index.php?search="),
    ("reddit", "https://www.reddit.com/search/?q="),
    ("stackoverflow", "https://stackoverflow.com/search?q="),
];
const ALIASES: &[(&str, &str)] = &[
    ("firefox", "firefox"),
    ("mozilla firefox", "firefox"),
    ("chrome", "chrome"),
    ("google chrome", "chrome"),
    ("chromium", "chromium"),
    ("brave", "brave"),
    ("brave browser", "brave"),
    ("visual studio code", "vscode"),
    ("vs code", "vscode"),
    ("vscode", "vscode"),
    ("code", "vscode"),
    ("terminal", "terminal"),
    ("a terminal", "terminal"),
    ("the terminal", "terminal"),
    ("a new terminal", "terminal"),
    ("file manager", "files"),
    ("files", "files"),
    ("editor", "vscode"),
    ("my editor", "vscode"),
];

fn lookup<'a, T>(table: &'a [(&'a str, T)], key: &str) -> Option<&'a T> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
}

/// One validated launch. Deserializable so a planner can propose steps, but only
/// [`Action::validate`] makes a proposal runnable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Action {
    App {
        target: String,
    },
    Url {
        target: String,
        #[serde(default)]
        browser: String,
    },
    Search {
        target: String,
        query: String,
        #[serde(default)]
        browser: String,
    },
    /// Explicit local screenshot request. Never deserialized from a remote
    /// planner response or sent to the Decider; saved only to the owner disk.
    #[serde(skip_deserializing)]
    Screenshot {
        mode: String,
    },
    /// Types plain text into the focused window. Never Enter or any control key, so
    /// nothing typed here can run on its own.
    Type {
        text: String,
    },
    /// Focus, move, maximize, minimize, close or list real windows through the desktop's
    /// window manager, verified and undoable where the desktop allows.
    Window {
        op: crate::wm::Op,
        #[serde(default)]
        target: crate::wm::Target,
        #[serde(default)]
        ws: Option<crate::wm::Ws>,
    },
    /// OpenCode's own interface in a new terminal, started in a fresh temporary folder.
    Opencode,
    /// A temporary OpenCode session in a fresh folder. It runs as a Work task, never as
    /// an Action step, and must be the only step.
    Session {
        goal: String,
    },
    /// Puts text the person selected on the clipboard. Only X's own context commands make
    /// this; a model's plan can never contain it.
    #[serde(skip_deserializing)]
    Copy {
        text: String,
    },
    /// A terminal that starts in a folder. Only saved workflows make this; a model's plan
    /// can never contain it.
    #[serde(skip_deserializing)]
    Terminal {
        directory: String,
    },
    /// A folder shown in the file manager. Workflows only.
    #[serde(skip_deserializing)]
    Folder {
        directory: String,
    },
    /// Operates one app's window by reading its accessibility tree: click, type, navigate,
    /// verify. It must be the only step and always needs the person's scoped permission.
    Computer {
        app: String,
        goal: String,
    },
}

/// What a planner may answer: steps, or that the speech was meant to be typed as dictation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reply {
    Steps(Vec<Action>),
    Dictation,
}

/// The error a planner-backed request returns when the speech was dictation, not a command.
pub const NOT_A_COMMAND: &str = "That was dictation, not a command";

const MAX_TYPED: usize = 300;

static LAST_LAUNCHED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Explicitly classify every action: an added action must be reviewed before it
/// can run without approval. In particular, close is never low-risk by default.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Risk {
    Safe,
    Reversible,
    Destructive,
    Consequential,
}

impl Action {
    pub fn risk(&self) -> Risk {
        use crate::wm::Op;
        match self {
            Self::Window { op: Op::Close, .. } => Risk::Destructive,
            Self::Window {
                op: Op::List | Op::Keys,
                ..
            } => Risk::Safe,
            Self::Window {
                op:
                    Op::Focus
                    | Op::Back
                    | Op::Move
                    | Op::Workspace
                    | Op::Maximize
                    | Op::Minimize
                    | Op::Undo,
                ..
            }
            | Self::App { .. }
            | Self::Url { .. }
            | Self::Search { .. }
            | Self::Opencode
            | Self::Copy { .. }
            | Self::Terminal { .. }
            | Self::Folder { .. } => Risk::Reversible,
            Self::Screenshot { .. }
            | Self::Type { .. }
            | Self::Session { .. }
            | Self::Computer { .. } => Risk::Consequential,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::App { target } => format!("Open {target}"),
            Self::Screenshot { mode } => format!("Save {mode} screenshot locally"),
            Self::Type { text } => format!("Type \"{text}\" into the focused window"),
            Self::Opencode => "Open OpenCode in a new temporary folder".to_string(),
            Self::Terminal { directory } => format!("Open a terminal in {directory}"),
            Self::Folder { directory } => format!("Open folder {directory}"),
            Self::Copy { text } => {
                format!(
                    "Copy {} selected characters to the clipboard",
                    text.chars().count()
                )
            }
            Self::Window { op, target, ws } => {
                let who = if target.app.is_empty() {
                    "the focused window".to_string()
                } else if let Some(pick) = target.pick {
                    format!("{} window #{pick}", target.app)
                } else {
                    format!("the {} window", target.app)
                };
                let destination = |ws: &crate::wm::Ws| match ws {
                    crate::wm::Ws::N(number) => format!("workspace {number}"),
                    crate::wm::Ws::Next => "the next workspace".to_string(),
                    crate::wm::Ws::Prev => "the previous workspace".to_string(),
                };
                match op {
                    crate::wm::Op::Focus => format!("Focus {who}"),
                    crate::wm::Op::Back => "Focus the previous window".to_string(),
                    crate::wm::Op::Move => match ws {
                        Some(ws) => format!("Move {who} to {}", destination(ws)),
                        None => format!("Move {who} (destination missing)"),
                    },
                    crate::wm::Op::Workspace => match ws {
                        Some(ws) => format!("Switch to {}", destination(ws)),
                        None => "Switch workspace (destination missing)".to_string(),
                    },
                    crate::wm::Op::Maximize => format!("Maximize {who}"),
                    crate::wm::Op::Minimize => format!("Minimize {who}"),
                    crate::wm::Op::Close => format!("Close {who}"),
                    crate::wm::Op::List => "List open windows".to_string(),
                    crate::wm::Op::Keys => "Look up your keyboard shortcuts".to_string(),
                    crate::wm::Op::Undo => "Undo the previous window change".to_string(),
                }
            }
            Self::Session { goal } => format!("Start a temporary OpenCode session: {goal}"),
            Self::Computer { app, goal } => format!("Operate {app}: {goal}"),
            Self::Url { target, browser } => with_browser(format!("Open {target}"), browser),
            Self::Search {
                target,
                query,
                browser,
            } => with_browser(format!("Search {target} for {query:?}"), browser),
        }
    }

    /// Reject anything outside the closed tool set.
    pub fn validate(self) -> Result<Self, String> {
        match &self {
            Self::App { target } => {
                if lookup(APPS, target).is_none() && crate::wm::apps::installed(target).is_none() {
                    return Err("The application is not in the installed app catalog".into());
                }
            }
            Self::Opencode => {}
            Self::Screenshot { mode } => {
                crate::screenshots::Mode::parse(mode)?;
            }
            Self::Copy { text } => crate::clipboard::validate(text)?,
            Self::Terminal { directory } | Self::Folder { directory } => {
                crate::workflows::paths::check(directory, &crate::workflows::paths::home()?)?;
            }
            Self::Window { op, target, ws } => {
                // A name or role such as "browser", matched only against open windows.
                if target.app.chars().count() > 64 || target.app.chars().any(char::is_control) {
                    return Err("That window name is not valid".into());
                }
                if matches!(op, crate::wm::Op::Move | crate::wm::Op::Workspace) {
                    match ws {
                        Some(crate::wm::Ws::N(n)) if !(1..=10).contains(n) => {
                            return Err("Workspace must be 1-10".into());
                        }
                        None => return Err("Which workspace?".into()),
                        _ => {}
                    }
                }
            }
            Self::Computer { app, goal } => {
                lookup(APPS, app).ok_or("Unapproved app")?;
                if !BROWSERS.contains(&app.as_str()) && !matches!(app.as_str(), "vscode" | "files")
                {
                    return Err(
                        "Screen access supports browsers, VS Code, and file managers".into(),
                    );
                }
                if goal.trim().is_empty() || goal.chars().count() > MAX_TRANSCRIPT {
                    return Err("A computer goal must be 1-1000 characters".into());
                }
            }
            Self::Session { goal } => {
                if goal.trim().is_empty() || goal.chars().count() > MAX_TRANSCRIPT {
                    return Err("A session goal must be 1-1000 characters".into());
                }
            }
            Self::Type { text } => {
                if text.trim().is_empty() || text.chars().count() > MAX_TYPED {
                    return Err("Typed text must be 1-300 characters".into());
                }
                if text.chars().any(char::is_control) {
                    return Err("Typed text cannot contain control characters or new lines".into());
                }
            }
            Self::Url { target, browser } => {
                check_browser(browser)?;
                safe_url(target)?;
            }
            Self::Search {
                target,
                query,
                browser,
            } => {
                check_browser(browser)?;
                lookup(SEARCH, target).ok_or("Unapproved search site")?;
                if query.trim().is_empty() || query.chars().count() > MAX_QUERY {
                    return Err("Search query must be 1-300 characters".into());
                }
                if query.chars().any(char::is_control) {
                    return Err("Control characters are not supported".into());
                }
            }
        }
        Ok(self)
    }

    /// The exact argv this action would run, without spawning (for plans and tests).
    pub fn argv(&self, resolve: &dyn Fn(&str) -> Option<String>) -> Result<Vec<String>, String> {
        let browser_or_opener = |browser: &str, url: String| -> Result<Vec<String>, String> {
            if browser.is_empty() {
                let opener = resolve("xdg-open").ok_or("xdg-open not found (install xdg-utils)")?;
                Ok(vec![opener, url])
            } else {
                Ok(vec![exe_for(browser, resolve)?, url])
            }
        };
        match self {
            Self::App { target } => {
                if lookup(APPS, target).is_some() {
                    return Ok(vec![exe_for(target, resolve)?]);
                }
                let app =
                    crate::wm::apps::installed(target).ok_or("The application is not installed")?;
                crate::wm::apps::launch_argv(&app, resolve)
            }
            Self::Window { .. } => Err("Window commands run through the window controller".into()),
            Self::Screenshot { .. } => {
                Err("Screenshots are handled locally, never launched as a command".into())
            }
            Self::Copy { .. } => Err("Copy runs through the clipboard helper".into()),
            Self::Terminal { directory } => {
                terminal_argv(std::path::Path::new(directory), None, resolve)
            }
            Self::Folder { directory } => Ok(vec![exe_for("files", resolve)?, directory.clone()]),
            Self::Session { .. } => Err("A session runs as Work, not as an Action".into()),
            Self::Computer { .. } => Err("Computer steps run through the observe loop".into()),
            Self::Opencode => opencode_argv(&session_dir("opencode"), resolve),
            Self::Type { text } => {
                let typer = resolve("wtype").ok_or("wtype not found (install wtype)")?;
                Ok(vec![typer, "--".into(), text.clone()])
            }
            Self::Url { target, browser } => {
                browser_or_opener(browser, safe_url(target)?.to_string())
            }
            Self::Search {
                target,
                query,
                browser,
            } => {
                let base = lookup(SEARCH, target).ok_or("Unapproved search site")?;
                let url = format!("{base}{}", percent_encode(query));
                browser_or_opener(browser, safe_url(&url)?.to_string())
            }
        }
    }

    pub fn launches_window(&self) -> bool {
        matches!(
            self,
            Self::App { .. }
                | Self::Url { .. }
                | Self::Search { .. }
                | Self::Opencode
                | Self::Terminal { .. }
                | Self::Folder { .. }
        )
    }

    /// Validate, then launch detached. Success means *launched*, not *page loaded*.
    pub fn execute(&self) -> Result<String, String> {
        let valid = self.clone().validate()?;
        if let Self::Window { op, target, ws } = &valid {
            let req = crate::wm::Request {
                op: op.clone(),
                target: target.clone(),
                ws: ws.clone(),
            };
            return crate::wm::run(&req)
                .map(|o| {
                    if o.undoable {
                        format!("{} (say undo to revert)", o.message)
                    } else {
                        o.message
                    }
                })
                .map_err(|e| e.to_string());
        }
        if let Self::Copy { text } = &valid {
            return crate::clipboard::copy(text);
        }
        if let Self::Screenshot { mode } = &valid {
            let mode = crate::screenshots::Mode::parse(mode)?;
            return crate::screenshots::save(mode, None)
                .map(|file| format!("Screenshot saved to {}", file.display()));
        }
        let argv = if matches!(valid, Self::Opencode) {
            let dir = session_dir("opencode");
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
            opencode_argv(&dir, &|name| which(name))?
        } else {
            valid.argv(&|name| which(name))?
        };
        let (exe, args) = argv.split_first().ok_or("empty command")?;
        if let Self::Type { text } = &valid {
            let status = Command::new(exe)
                .args(args)
                .stdin(Stdio::null())
                .status()
                .map_err(|e| format!("could not run {exe}: {e}"))?;
            return if status.success() {
                Ok(format!("Typed {} characters", text.chars().count()))
            } else {
                Err("wtype could not type into the focused window".into())
            };
        }
        let mut cmd = Command::new(exe);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: setsid is async-signal-safe and runs between fork and exec; it detaches
        // the launched app from X's process group so Stop/exit never kills the user's browser.
        #[cfg(unix)]
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("could not launch {exe}: {e}"))?;
        LAST_LAUNCHED.store(child.id(), std::sync::atomic::Ordering::SeqCst);
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(format!("Launched {}", valid.describe()))
    }
}

/// Hyprland's Lua config takes `hl.dsp.*` expressions; older ones take named dispatchers.
/// Both are built here from allowlisted values only. The first the desktop accepts wins.
fn session_dir(prefix: &str) -> std::path::PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    dirs::home_dir()
        .unwrap_or_default()
        .join("x-sessions")
        .join(format!("{prefix}-{secs}"))
}

/// A terminal that starts in `dir`, optionally running one program. The terminals listed
/// here are the ones whose "start in this folder" flag is known; any other is refused,
/// not guessed.
fn terminal_argv(
    dir: &std::path::Path,
    run: Option<String>,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<String>, String> {
    let terminal = exe_for("terminal", resolve)?;
    let dir = dir.display().to_string();
    let name = std::path::Path::new(&terminal)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let mut argv = vec![terminal.clone()];
    match (name, run) {
        ("kitty", run) => argv.extend(["--directory".into(), dir].into_iter().chain(run)),
        ("foot", run) => argv.extend(["--working-directory".into(), dir].into_iter().chain(run)),
        ("alacritty", run) => {
            argv.extend(["--working-directory".into(), dir]);
            if let Some(run) = run {
                argv.extend(["-e".into(), run]);
            }
        }
        (other, _) => return Err(format!("X does not know how to start {other} in a folder")),
    }
    Ok(argv)
}

/// A terminal that starts in `dir` running OpenCode.
fn opencode_argv(
    dir: &std::path::Path,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<String>, String> {
    let opencode = resolve("opencode")
        .or_else(|| {
            dirs::home_dir()
                .map(|h| h.join(".opencode/bin/opencode"))
                .filter(|p| p.is_file())
                .map(|p| p.display().to_string())
        })
        .ok_or("OpenCode not found")?;
    terminal_argv(dir, Some(opencode), resolve)
}

/// "open an OpenCode session": the person wants OpenCode itself, with no task named.
fn wants_opencode_terminal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let mentions = lower.contains("opencode") || lower.contains("open code");
    mentions
        && lower.contains("session")
        && !lower.contains(" to ")
        && lower.split_whitespace().count() <= 12
}

pub(crate) fn desktop_step(exe: &str, legacy: &[String], lua: &str) -> Result<(), String> {
    let mut last = String::new();
    for args in [
        vec!["dispatch".to_string(), lua.to_string()],
        legacy.to_vec(),
    ] {
        let out = Command::new(exe)
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not run {exe}: {e}"))?;
        last = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && last.eq_ignore_ascii_case("ok") {
            return Ok(());
        }
    }
    Err(format!("The desktop refused: {last}"))
}

fn with_browser(base: String, browser: &str) -> String {
    if browser.is_empty() {
        base
    } else {
        format!("{base} in {browser}")
    }
}

fn check_browser(browser: &str) -> Result<(), String> {
    if browser.is_empty() || BROWSERS.contains(&browser) {
        Ok(())
    } else {
        Err("Unapproved browser".into())
    }
}

fn exe_for(alias: &str, resolve: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let candidates = lookup(APPS, alias).ok_or("Unapproved app")?;
    candidates
        .iter()
        .find_map(|c| resolve(c))
        .ok_or_else(|| format!("{alias} is not installed or not on PATH"))
}

#[cfg(unix)]
fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Windows has no execute bit; a file is runnable if it exists with an executable extension.
#[cfg(windows)]
fn is_executable(p: &std::path::Path) -> bool {
    p.is_file()
}

pub fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    #[cfg(windows)]
    let names = [
        name.to_string(),
        format!("{name}.exe"),
        format!("{name}.cmd"),
        format!("{name}.bat"),
    ];
    #[cfg(not(windows))]
    let names = [name.to_string()];
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| is_executable(p))
        .map(|p| p.to_string_lossy().into_owned())
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Public HTTPS only: no credentials, localhost, IPs, odd ports or control chars.
pub fn safe_url(raw: &str) -> Result<&str, String> {
    const MSG: &str = "Only public HTTPS URLs without credentials are supported";
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("Control characters are not supported".into());
    }
    let rest = raw.strip_prefix("https://").ok_or(MSG)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') || authority.is_empty() {
        return Err(MSG.into());
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    if port.is_some_and(|p| p != "443") {
        return Err("Non-standard ports are not supported".into());
    }
    let host = host.to_ascii_lowercase();
    let host = host.trim_end_matches('.');
    const PRIVATE: [&str; 8] = [
        ".local",
        ".localhost",
        ".internal",
        ".lan",
        ".home.arpa",
        ".localdomain",
        ".corp",
        ".intranet",
    ];
    if host == "localhost" || PRIVATE.iter().any(|s| host.ends_with(s)) {
        return Err("Local addresses are not supported".into());
    }
    let valid_chars = host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    if !valid_chars || host.len() < 3 || host.len() > 253 || !host.contains('.') {
        return Err("A public domain is required".into());
    }
    if host
        .split('.')
        .any(|l| l.is_empty() || l.starts_with('-') || l.ends_with('-'))
    {
        return Err("Invalid domain".into());
    }
    let tld = host.rsplit('.').next().unwrap_or("");
    if host.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        || tld.bytes().all(|b| b.is_ascii_digit())
        || tld.starts_with("0x")
    {
        return Err("IP addresses are not supported".into());
    }
    Ok(raw)
}

/// The approved app an alias or spoken name refers to.
pub(crate) fn resolve_app(name: &str) -> Option<&'static str> {
    normalize_app(name)
}

/// Window classes that belong to an approved app.
pub(crate) fn app_classes(app: &str) -> Option<Vec<&'static str>> {
    let (name, exes) = APPS.iter().find(|(n, _)| *n == app)?;
    Some(std::iter::once(*name).chain(exes.iter().copied()).collect())
}

fn normalize_app(name: &str) -> Option<&'static str> {
    let key = name
        .trim()
        .trim_matches(|c| " .!?".contains(c))
        .to_ascii_lowercase();
    ALIASES.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

static WAKE_WORDS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
const DEFAULT_WAKE_WORDS: [&str; 2] = ["friday", "x"];
const GREETINGS: [&str; 4] = ["hey", "hi", "okay", "ok"];

/// The words that make speech a command. Anyone can choose their own; until they do it
/// is "friday" and "x".
pub fn wake_words() -> Vec<String> {
    match WAKE_WORDS.lock() {
        Ok(words) if !words.is_empty() => words.clone(),
        _ => DEFAULT_WAKE_WORDS.map(String::from).to_vec(),
    }
}

pub fn set_wake_words(words: Vec<String>) {
    if let Ok(mut slot) = WAKE_WORDS.lock() {
        *slot = words;
    }
}

/// One spoken word, lowercased: letters or digits only, not a greeting or a mode word.
pub fn valid_wake_word(word: &str) -> Result<String, String> {
    let word = word.trim().to_lowercase();
    let count = word.chars().count();
    if count == 0 || count > 24 || !word.chars().all(char::is_alphanumeric) {
        return Err("A wake word is one word of 1-24 letters or digits".into());
    }
    if GREETINGS.contains(&word.as_str()) || word == "work" {
        return Err(format!("{word:?} cannot be a wake word"));
    }
    Ok(word)
}

pub(crate) fn strip_wake_word(utterance: &str) -> String {
    strip_with(utterance, &wake_words())
}

fn strip_with(utterance: &str, wake_words: &[String]) -> String {
    let clean = collapse_ws(utterance);
    let clean = clean.trim_matches(|c| " .!?,".contains(c));
    let lower = clean.to_lowercase();
    let mut skip = 0;
    for greeting in GREETINGS {
        if let Some(rest) = lower.strip_prefix(greeting)
            && rest.starts_with([' ', ','])
        {
            skip = greeting.len() + rest.len() - rest.trim_start_matches([' ', ',']).len();
            break;
        }
    }
    for wake in wake_words {
        if let Some(rest) = lower[skip..].strip_prefix(wake.as_str())
            && rest.starts_with([',', '.', ':', '!', ' '])
        {
            let keep = rest.chars().count();
            let at = clean
                .char_indices()
                .rev()
                .nth(keep.saturating_sub(1))
                .map_or(clean.len(), |(i, _)| i);
            return clean[at..]
                .trim_start_matches(|c| " ,.:!".contains(c))
                .to_string();
        }
    }
    clean.to_string()
}

fn strip_prefix_ci<'a>(s: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    let lower = s.to_ascii_lowercase();
    prefixes.iter().find_map(|p| {
        let rest = lower.strip_prefix(p)?;
        rest.starts_with(' ')
            .then(|| s[s.len() - rest.len()..].trim_start())
    })
}

/// Strict local phrase matching: a screenshot may contain private pixels,
/// so a sentence merely mentioning screenshots must never trigger capture.
fn screenshot_request(utterance: &str) -> Option<Action> {
    let text = utterance.trim().to_ascii_lowercase();
    let mode = match text.as_str() {
        "screenshot" | "take screenshot" | "take a screenshot" | "capture screenshot"
        | "capture the screen" => "screen",
        "screenshot this window"
        | "screenshot of this window"
        | "take a screenshot of this window"
        | "take screenshot of this window" => "window",
        "screenshot a region"
        | "screenshot of a region"
        | "take a screenshot of a region"
        | "take a screenshot of the region" => "region",
        _ => return None,
    };
    Some(Action::Screenshot { mode: mode.into() })
}

fn parse_one(text: &str) -> Option<Vec<Action>> {
    let text = text.trim().trim_end_matches(['?', '.', '!', ' ']);
    if let Some(req) = crate::wm::parse::parse(text) {
        return Some(vec![Action::Window {
            op: req.op,
            target: req.target,
            ws: req.ws,
        }]);
    }
    if let Some(shot) = screenshot_request(text) {
        return Some(vec![shot]);
    }
    let mut browser = String::new();
    let mut search_text = text.to_string();
    if let Some(rest) = strip_prefix_ci(text, &["open up", "open", "launch", "start"]) {
        // "open Firefox and search YouTube for ..." is ONE browser launch with the destination.
        let lower = rest.to_ascii_lowercase();
        for joiner in [" and ", " then "] {
            if let Some(i) = lower.find(joiner) {
                let tail = &rest[i + joiner.len()..];
                if let Some(app) = normalize_app(&rest[..i]).filter(|a| BROWSERS.contains(a))
                    && strip_prefix_ci(tail, &["search", "look up", "find"]).is_some()
                {
                    browser = app.to_string();
                    search_text = tail.to_string();
                }
            }
        }
    }
    if let Some(rest) = strip_prefix_ci(&search_text, &["search", "look up", "find"]) {
        let lower = rest.to_ascii_lowercase();
        let (site, rest) = SEARCH
            .iter()
            .map(|(s, _)| *s)
            .find_map(|s| {
                lower
                    .strip_prefix(s)
                    .filter(|r| r.starts_with(' '))
                    .map(|r| (s, rest[rest.len() - r.len()..].trim_start()))
            })
            .unwrap_or(("google", rest));
        let query = strip_prefix_ci(rest, &["for"])
            .unwrap_or(rest)
            .trim()
            .to_string();
        let action = Action::Search {
            target: site.into(),
            query,
            browser,
        };
        return action.validate().ok().map(|a| vec![a]);
    }
    if wants_opencode_terminal(text) {
        return Some(vec![Action::Opencode]);
    }
    if let Some(goal) = strip_prefix_ci(
        text,
        &[
            "start a temporary session to",
            "start a temp session to",
            "spin up a temporary session to",
            "spin up a session to",
            "start a session to",
        ],
    ) {
        return Action::Session {
            goal: goal.trim().to_string(),
        }
        .validate()
        .ok()
        .map(|a| vec![a]);
    }
    if let Some(rest) = strip_prefix_ci(text, &["switch to", "go to", "focus"]) {
        let rest = rest.trim();
        if let Some(number) =
            strip_prefix_ci(rest, &["workspace"]).and_then(|n| n.trim().parse::<u8>().ok())
        {
            return Action::Window {
                op: crate::wm::Op::Workspace,
                target: crate::wm::Target::default(),
                ws: Some(crate::wm::Ws::N(number)),
            }
            .validate()
            .ok()
            .map(|a| vec![a]);
        }
        return normalize_app(rest).map(|app| {
            vec![Action::Window {
                op: crate::wm::Op::Focus,
                target: crate::wm::Target {
                    app: app.into(),
                    pick: None,
                },
                ws: None,
            }]
        });
    }
    if let Some(rest) = strip_prefix_ci(text, &["type", "write"]) {
        let typed = rest.trim().trim_matches(['"', '\'', '“', '”']).trim();
        return Action::Type {
            text: typed.to_string(),
        }
        .validate()
        .ok()
        .map(|a| vec![a]);
    }
    let rest = strip_prefix_ci(text, &["open up", "open", "launch", "start"])?;
    if let Some(app) = normalize_app(rest) {
        return Some(vec![Action::App { target: app.into() }]);
    }
    // Unknown-to-the-hardcoded-list app names can still be genuine installed
    // apps. Only exact, unique desktop-entry catalog matches are accepted.
    let name = rest.trim().to_ascii_lowercase();
    if let Some(app) = crate::wm::apps::installed(&name) {
        return Some(vec![Action::App { target: app.id }]);
    }
    if let Some(url) = lookup(SITES, &name) {
        return Some(vec![Action::Url {
            target: (*url).into(),
            browser: String::new(),
        }]);
    }
    let url = if rest.starts_with("https://") {
        rest.to_string()
    } else {
        format!("https://{rest}")
    };
    if rest.contains('.') && !rest.contains(char::is_whitespace) {
        return Action::Url {
            target: url,
            browser: String::new(),
        }
        .validate()
        .ok()
        .map(|a| vec![a]);
    }
    None
}

fn split_steps(text: &str) -> Vec<String> {
    // Split only where another action verb follows, so "and" inside a query survives.
    let lower = text.to_ascii_lowercase();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < lower.len() {
        for joiner in [" and then ", " then ", " and "] {
            if lower[i..].starts_with(joiner) {
                let after = &lower[i + joiner.len()..];
                if [
                    "open ",
                    "launch ",
                    "start ",
                    "search ",
                    "look up ",
                    "type ",
                    "write ",
                    "switch to ",
                    "focus ",
                ]
                .iter()
                .any(|v| after.starts_with(v))
                {
                    parts.push(text[start..i].trim().to_string());
                    start = i + joiner.len();
                    i = start;
                    break;
                }
            }
        }
        i += lower[i..].chars().next().map_or(1, char::len_utf8);
    }
    parts.push(text[start..].trim().to_string());
    parts.retain(|p| !p.is_empty());
    parts
}

fn focused() -> Option<(String, u32)> {
    let json = hypr_json(&["activewindow", "-j"])?;
    let address = json.get("address")?.as_str()?.to_string();
    Some((address, u32::try_from(json.get("pid")?.as_u64()?).ok()?))
}

/// Waits up to six seconds for the window of the process X just started to take focus.
/// Typing never goes anywhere else: if that window cannot be told apart from the rest
/// (the app hands off to another process, or the desktop cannot say), nothing is typed.
pub fn wait_for_launched_focus() -> Result<(), String> {
    let pid = LAST_LAUNCHED.load(std::sync::atomic::Ordering::SeqCst);
    if pid == 0 || which("hyprctl").is_none() {
        return Err(
            "X cannot tell which window would receive the text, so nothing was typed".into(),
        );
    }
    for _ in 0..30 {
        if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            return Err(
                "The app exited or handed off right after launch, so nothing was typed".into(),
            );
        }
        if focused().is_some_and(|(_, focused_pid)| focused_pid == pid) {
            std::thread::sleep(std::time::Duration::from_millis(400));
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Err("The launched app's window never took focus, so nothing was typed".into())
}

/// Only exhaustively classified safe/reversible actions can run automatically.
/// Typing into a freshly opened window is a narrow exception; closes require
/// additional local-rule, push-to-talk, and exact-focus checks in the service.
pub fn is_low_risk(actions: &[Action]) -> bool {
    actions
        .iter()
        .enumerate()
        .all(|(i, action)| match action.risk() {
            Risk::Safe | Risk::Reversible => true,
            Risk::Destructive => false,
            Risk::Consequential => {
                matches!(action, Action::Type { .. }) && i > 0 && actions[i - 1].launches_window()
            }
        })
}

/// Deterministic rule planner. `None` means "not understood": nothing runs.
pub fn plan_rules(utterance: &str) -> Option<Vec<Action>> {
    let content = strip_wake_word(utterance);
    if content.is_empty() || content.chars().count() > MAX_TRANSCRIPT {
        return None;
    }
    if let Some(one) = parse_one(&content) {
        return Some(one);
    }
    let parts = split_steps(&content);
    if parts.len() <= 1 || parts.len() > MAX_STEPS {
        return None;
    }
    let mut actions = Vec::new();
    for part in &parts {
        actions.extend(parse_one(part)?);
    }
    Some(actions)
}

/// Parse untrusted planner JSON (`{"steps":[...]}` or `{"dictate":true}`) into a reply of
/// validated actions.
pub fn parse_reply(raw: &str) -> Result<Reply, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Mark {
        dictate: bool,
    }
    if let Ok(Mark { dictate: true }) = serde_json::from_str(raw) {
        return Ok(Reply::Dictation);
    }
    let actions = parse_plan_json(raw)?;
    if actions.len() > 1
        && actions
            .iter()
            .any(|a| matches!(a, Action::Session { .. } | Action::Computer { .. }))
    {
        return Err("A session or computer step must be the only step".into());
    }
    Ok(Reply::Steps(actions))
}

/// Parse untrusted planner JSON (`{"steps":[...]}`) into validated actions.
pub fn parse_plan_json(raw: &str) -> Result<Vec<Action>, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Plan {
        steps: Vec<serde_json::Value>,
    }
    if raw.len() > 100_000 {
        return Err("Planner output too large".into());
    }
    let plan: Plan =
        serde_json::from_str(raw).map_err(|e| format!("Malformed planner output: {e}"))?;
    if plan.steps.len() > MAX_STEPS {
        return Err("Planner exceeded maximum allowed steps".into());
    }
    plan.steps
        .into_iter()
        .map(|step| {
            use crate::wm::{Op, Target, Ws};
            let action = match step.get("kind").and_then(serde_json::Value::as_str) {
                Some("focus") => {
                    if step.as_object().is_none_or(|fields| fields.len() != 2) {
                        return Err("Legacy focus has unexpected fields".into());
                    }
                    let app = step
                        .get("target")
                        .and_then(serde_json::Value::as_str)
                        .ok_or("Legacy focus lacks a target")?;
                    if lookup(APPS, app).is_none() {
                        return Err("Unapproved legacy focus target".into());
                    }
                    Action::Window {
                        op: Op::Focus,
                        target: Target {
                            app: app.into(),
                            pick: None,
                        },
                        ws: None,
                    }
                }
                Some("workspace") => {
                    if step.as_object().is_none_or(|fields| fields.len() != 2) {
                        return Err("Legacy workspace has unexpected fields".into());
                    }
                    let number = step
                        .get("number")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|n| u8::try_from(n).ok())
                        .filter(|n| (1..=10).contains(n))
                        .ok_or("Legacy workspace must be 1-10")?;
                    Action::Window {
                        op: Op::Workspace,
                        target: Target::default(),
                        ws: Some(Ws::N(number)),
                    }
                }
                _ => serde_json::from_value(step)
                    .map_err(|e| format!("Unsupported action: {e}"))?,
            };
            action.validate()
        })
        .collect()
}

#[cfg(test)]
mod tests;

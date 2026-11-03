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
    /// Types plain text into the focused window. Never Enter or any control key, so
    /// nothing typed here can run on its own.
    Type {
        text: String,
    },
    /// Brings an already-open allowlisted app's window to the front (Hyprland).
    Focus {
        target: String,
    },
    /// Switches to workspace 1-10 (Hyprland).
    Workspace {
        number: u8,
    },
    /// A temporary OpenCode session in a fresh folder. It runs as a Work task, never as
    /// an Action step, and must be the only step.
    Session {
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

impl Action {
    pub fn describe(&self) -> String {
        match self {
            Self::App { target } => format!("Open {target}"),
            Self::Type { text } => format!("Type \"{text}\" into the focused window"),
            Self::Focus { target } => format!("Switch to {target}"),
            Self::Workspace { number } => format!("Go to workspace {number}"),
            Self::Session { goal } => format!("Start a temporary OpenCode session: {goal}"),
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
                lookup(APPS, target).ok_or("Unapproved app")?;
            }
            Self::Focus { target } => {
                lookup(APPS, target).ok_or("Unapproved app")?;
            }
            Self::Workspace { number } => {
                if !(1..=10).contains(number) {
                    return Err("Workspace must be 1-10".into());
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
            Self::App { target } => Ok(vec![exe_for(target, resolve)?]),
            Self::Focus { target } => {
                let names = lookup(APPS, target).ok_or("Unapproved app")?;
                let class = format!("class:^({})$", names.join("|"));
                Ok(vec![
                    resolve("hyprctl").ok_or("hyprctl not found")?,
                    "dispatch".into(),
                    "focuswindow".into(),
                    class,
                ])
            }
            Self::Workspace { number } => Ok(vec![
                resolve("hyprctl").ok_or("hyprctl not found")?,
                "dispatch".into(),
                "workspace".into(),
                number.to_string(),
            ]),
            Self::Session { .. } => Err("A session runs as Work, not as an Action".into()),
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
            Self::App { .. } | Self::Url { .. } | Self::Search { .. }
        )
    }

    /// Validate, then launch detached. Success means *launched*, not *page loaded*.
    pub fn execute(&self) -> Result<String, String> {
        let valid = self.clone().validate()?;
        let argv = valid.argv(&|name| which(name))?;
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
        if let Self::Focus { target } = &valid {
            let names = lookup(APPS, target).ok_or("Unapproved app")?;
            let lua = format!(
                "hl.dsp.focus({{ window = \"class:^({})$\" }})",
                names.join("|")
            );
            return desktop_step(exe, args, &lua).and_then(|()| confirm_focus(target));
        }
        if let Self::Workspace { number } = &valid {
            let lua = format!("hl.dsp.focus({{ workspace = \"{number}\" }})");
            return desktop_step(exe, args, &lua).and_then(|()| confirm_workspace(*number));
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
fn desktop_step(exe: &str, legacy: &[String], lua: &str) -> Result<(), String> {
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

fn hypr_json(args: &[&str]) -> Option<serde_json::Value> {
    let out = Command::new("hyprctl").args(args).output().ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

/// Polls up to two seconds so the answer reflects the desktop, not just the dispatch.
fn confirm_focus(target: &str) -> Result<String, String> {
    let names = lookup(APPS, target).ok_or("Unapproved app")?;
    for _ in 0..10 {
        let class = hypr_json(&["activewindow", "-j"])
            .and_then(|j| j.get("class")?.as_str().map(str::to_ascii_lowercase));
        if class.is_some_and(|c| names.iter().any(|n| c.contains(&n.to_ascii_lowercase()))) {
            return Ok(format!("Switched to {target}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Err(format!("No open {target} window took focus"))
}

fn confirm_workspace(number: u8) -> Result<String, String> {
    for _ in 0..10 {
        let id = hypr_json(&["activeworkspace", "-j"]).and_then(|j| j.get("id")?.as_u64());
        if id == Some(u64::from(number)) {
            return Ok(format!("Now on workspace {number}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Err(format!("Workspace {number} did not become active"))
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

pub fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| {
            use std::os::unix::fs::PermissionsExt;
            p.metadata()
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
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

fn strip_wake_word(utterance: &str) -> String {
    let clean = collapse_ws(utterance);
    let clean = clean.trim_matches(|c| " .!?,".contains(c));
    let lower = clean.to_ascii_lowercase();
    for wake in ["hey x", "okay x", "ok x", "x"] {
        if let Some(rest) = lower.strip_prefix(wake)
            && rest.starts_with([',', '.', ':', '!', ' '])
        {
            let n = clean.len() - rest.len();
            return clean[n..]
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

fn parse_one(text: &str) -> Option<Vec<Action>> {
    let text = text.trim().trim_end_matches(['?', '.', '!', ' ']);
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
            return Action::Workspace { number }
                .validate()
                .ok()
                .map(|a| vec![a]);
        }
        return normalize_app(rest).map(|app| vec![Action::Focus { target: app.into() }]);
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
    let name = rest.trim().to_ascii_lowercase();
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
    let out = Command::new("hyprctl")
        .args(["activewindow", "-j"])
        .output()
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
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

/// Whether a plan may run without asking once the person trusts Actions: launches and
/// searches, and typing only straight after a launch (so the text lands in the new window).
pub fn is_low_risk(actions: &[Action]) -> bool {
    actions.iter().enumerate().all(|(i, action)| match action {
        Action::Type { .. } => i > 0 && actions[i - 1].launches_window(),
        Action::Session { .. } => actions.len() == 1,
        _ => true,
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
    if actions.len() > 1 && actions.iter().any(|a| matches!(a, Action::Session { .. })) {
        return Err("A session must be the only step".into());
    }
    Ok(Reply::Steps(actions))
}

/// Parse untrusted planner JSON (`{"steps":[...]}`) into validated actions.
pub fn parse_plan_json(raw: &str) -> Result<Vec<Action>, String> {
    #[derive(Deserialize)]
    struct Plan {
        steps: Vec<Action>,
    }
    if raw.len() > 100_000 {
        return Err("Planner output too large".into());
    }
    let plan: Plan =
        serde_json::from_str(raw).map_err(|e| format!("Malformed planner output: {e}"))?;
    if plan.steps.len() > MAX_STEPS {
        return Err("Planner exceeded maximum allowed steps".into());
    }
    plan.steps.into_iter().map(Action::validate).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_path(name: &str) -> Option<String> {
        Some(format!("/usr/bin/{name}"))
    }

    fn plan(text: &str) -> Vec<String> {
        plan_rules(text)
            .unwrap_or_default()
            .iter()
            .map(Action::describe)
            .collect()
    }

    #[test]
    fn typing_is_plain_text_only_and_follows_a_launch_to_be_low_risk() {
        let plan = plan_rules("open a terminal and type welcome back wiki").unwrap();
        assert_eq!(
            plan.iter().map(Action::describe).collect::<Vec<_>>(),
            [
                "Open terminal",
                "Type \"welcome back wiki\" into the focused window"
            ]
        );
        assert!(is_low_risk(&plan));
        assert!(!is_low_risk(&plan_rules("type hello").unwrap()));
        let argv = plan[1].argv(&fake_path).unwrap();
        assert_eq!(argv, ["/usr/bin/wtype", "--", "welcome back wiki"]);
        assert!(
            Action::Type {
                text: "a\nb".into()
            }
            .validate()
            .is_err()
        );
        assert!(
            Action::Type {
                text: "x\u{1b}[A".into()
            }
            .validate()
            .is_err()
        );
        assert!(Action::Type { text: "  ".into() }.validate().is_err());
        assert_eq!(plan_rules("open up a terminal").unwrap().len(), 1);
        let dash = Action::Type {
            text: "--help".into(),
        };
        assert_eq!(dash.argv(&fake_path).unwrap()[1], "--");
    }

    #[test]
    fn opens_known_apps_and_sites() {
        assert_eq!(plan("open Firefox"), ["Open firefox"]);
        assert_eq!(plan("Hey X, launch VS Code."), ["Open vscode"]);
        assert_eq!(plan("open github"), ["Open https://github.com"]);
        assert_eq!(plan("open example.org"), ["Open https://example.org"]);
    }

    #[test]
    fn browser_plus_search_is_one_launch() {
        let actions = plan_rules("open Firefox and search YouTube for Mars rover").unwrap();
        assert_eq!(actions.len(), 1);
        let argv = actions[0].argv(&fake_path).unwrap();
        assert_eq!(
            argv,
            [
                "/usr/bin/firefox",
                "https://www.youtube.com/results?search_query=Mars%20rover"
            ]
        );
    }

    #[test]
    fn two_step_instruction() {
        assert_eq!(
            plan("open terminal then search github for ripgrep"),
            ["Open terminal", "Search github for \"ripgrep\""]
        );
        assert_eq!(
            plan("open chrome then search github for ripgrep"),
            ["Search github for \"ripgrep\" in chrome"]
        );
    }

    #[test]
    fn and_inside_a_query_is_kept() {
        assert_eq!(
            plan("search google for salt and pepper"),
            ["Search google for \"salt and pepper\""]
        );
    }

    #[test]
    fn query_cannot_inject_arguments_or_shell() {
        let a = plan_rules("search youtube for a&b; rm -rf ~ $(whoami) `id` --new-window").unwrap();
        let argv = a[0].argv(&fake_path).unwrap();
        assert_eq!(argv.len(), 2, "query stays inside one URL argument");
        assert!(!argv[1].contains(['&', ';', ' ', '$', '`']), "{}", argv[1]);
    }

    #[test]
    fn unsupported_requests_run_nothing() {
        for t in [
            "delete all my files",
            "run rm -rf /",
            "send an email to bob",
            "",
            "open /etc/passwd",
        ] {
            assert!(plan_rules(t).is_none(), "{t}");
        }
        assert!(plan_rules(&"open firefox ".repeat(200)).is_none());
    }

    #[test]
    fn non_ascii_input_never_panics() {
        for t in [
            "İstanbul open firefox",
            "open İ firefox and search ß for ǅ",
            "ǅ then open ǅ",
            "x\u{130}, open firefox",
            "search youtube for café and open github",
            "open 日本語.jp",
        ] {
            let _ = plan_rules(t);
        }
        assert_eq!(
            plan("search youtube for café"),
            ["Search youtube for \"café\""]
        );
    }

    #[test]
    fn urls_are_locked_down() {
        for bad in [
            "http://example.com",
            "file:///etc/passwd",
            "https://0x7f.0.0.1/",
            "https://0xc0.0xa8.1.1/",
            "https://nas.lan/",
            "https://app.internal/",
            "javascript:alert(1)",
            "https://localhost/",
            "https://a.local/",
            "https://127.0.0.1/",
            "https://user:pw@example.com/",
            "https://example.com:8080/",
            "https://exa mple.com",
            "https://-bad.com",
            "https://nodots",
            "https://example.com/\u{0007}",
        ] {
            assert!(safe_url(bad).is_err(), "{bad}");
        }
        assert!(safe_url("https://example.com:443/a?b=c").is_ok());
    }

    #[test]
    fn planner_json_is_untrusted() {
        let ok = r#"{"steps":[{"kind":"search","target":"youtube","query":"cats"}]}"#;
        assert_eq!(parse_plan_json(ok).unwrap().len(), 1);
        for bad in [
            r#"{"steps":[{"kind":"shell","cmd":"rm -rf /"}]}"#,
            r#"{"steps":[{"kind":"app","target":"bash"}]}"#,
            r#"{"steps":[{"kind":"app","target":"firefox","extra":"--evil"}]}"#,
            r#"{"steps":[{"kind":"url","target":"file:///etc/passwd"}]}"#,
            r#"{"steps":[{"kind":"url","target":"https://example.com","browser":"curl"}]}"#,
            r#"{"steps":[{"kind":"search","target":"evil","query":"x"}]}"#,
            r#"{"steps":[{"kind":"app","target":"firefox"},{"kind":"app","target":"firefox"},{"kind":"app","target":"firefox"},{"kind":"app","target":"firefox"},{"kind":"app","target":"firefox"},{"kind":"app","target":"firefox"}]}"#,
            "not json",
            r#"{"steps":"x"}"#,
        ] {
            assert!(parse_plan_json(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn missing_executable_is_reported() {
        let a = Action::App {
            target: "firefox".into(),
        };
        assert!(a.argv(&|_| None).unwrap_err().contains("not installed"));
    }
}

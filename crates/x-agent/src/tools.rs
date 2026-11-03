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
}

impl Action {
    pub fn describe(&self) -> String {
        match self {
            Self::App { target } => format!("Open {target}"),
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

    /// Validate, then launch detached. Success means *launched*, not *page loaded*.
    pub fn execute(&self) -> Result<String, String> {
        let valid = self.clone().validate()?;
        let argv = valid.argv(&|name| which(name))?;
        let (exe, args) = argv.split_first().ok_or("empty command")?;
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
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(format!("Launched {}", valid.describe()))
    }
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
    if let Some(rest) = strip_prefix_ci(text, &["open", "launch", "start"]) {
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
    let rest = strip_prefix_ci(text, &["open", "launch", "start"])?;
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
                if ["open ", "launch ", "start ", "search ", "look up "]
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

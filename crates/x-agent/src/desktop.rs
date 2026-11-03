//! One app window: Hyprland finds and focuses it, `agent-desktop` reads its accessibility
//! tree and acts by ref, `wtype` sends the keys AT-SPI cannot. Everything read is untrusted.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::tools;

const MAX_ELEMENTS: usize = 120;
const MAX_TEXTS: usize = 60;
const MAX_NAME: usize = 160;
const OUTPUT_CAP: u64 = 4_000_000;
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

const ACTIONABLE: &[&str] = &[
    "Click", "Select", "Toggle", "Expand", "Collapse", "SetFocus", "SetValue",
];
const SKIP_ROLES: &[&str] = &[
    "application",
    "window",
    "frame",
    "panel",
    "section",
    "generic",
    "group",
    "unknown",
    "document",
    "scrollarea",
    "toolbar",
    "list",
    "separator",
];
const SECRET_WORDS: &[&str] = &[
    "password",
    "passcode",
    "passphrase",
    "otp",
    "cvv",
    "cvc",
    "card number",
    "secret",
    "2fa",
    "verification code",
];
const FIELD_WORDS: &[&str] = &["address", "search", "url", "find", "omnibox", "query"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Win {
    pub address: String,
    pub pid: u32,
    pub class: String,
    pub title: String,
    pub id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Element {
    pub id: String,
    pub reference: String,
    pub role: String,
    pub name: String,
    pub value: String,
    pub actions: Vec<String>,
}

impl Element {
    pub fn is_secret(&self) -> bool {
        self.role.contains("password") || mentions(&self.name, SECRET_WORDS)
    }

    pub fn is_field(&self) -> bool {
        matches!(
            self.role.as_str(),
            "textfield" | "textarea" | "searchbox" | "combobox" | "entry" | "text"
        ) && self.actions.iter().any(|a| a == "SetFocus" || a == "Click")
    }

    /// The browser's address bar: named for addresses, and early in the tree (toolbar, not page).
    pub fn is_omnibox(&self) -> bool {
        self.is_field()
            && mentions(&self.name, &["address"])
            && self
                .id
                .trim_start_matches('e')
                .parse::<usize>()
                .is_ok_and(|n| n <= 40)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Observation {
    pub snapshot: String,
    pub title: String,
    pub elements: Vec<Element>,
    pub texts: Vec<String>,
}

impl Observation {
    pub fn element(&self, id: &str) -> Option<&Element> {
        self.elements.iter().find(|e| e.id == id)
    }

    pub fn shows(&self, needle: &str) -> bool {
        let needle = needle.trim().to_lowercase();
        needle.chars().count() >= 3
            && std::iter::once(&self.title)
                .chain(self.texts.iter())
                .chain(self.elements.iter().flat_map(|e| [&e.name, &e.value]))
                .any(|text| text.to_lowercase().contains(&needle))
    }

    pub fn render(&self) -> String {
        let mut out = format!("window: {:?}\ncontrols:\n", prompt_text(&self.title));
        for e in &self.elements {
            let value = if e.value.is_empty() {
                String::new()
            } else {
                format!(" value={:?}", prompt_text(&e.value))
            };
            out.push_str(&format!(
                "{} {} {:?}{value}\n",
                e.id,
                e.role,
                prompt_text(&e.name)
            ));
        }
        out.push_str("text:\n");
        for t in &self.texts {
            out.push_str(&format!("- {}\n", prompt_text(t)));
        }
        out
    }
}

fn prompt_text(text: &str) -> String {
    clip(text, MAX_NAME)
        .split_whitespace()
        .map(|word| {
            let digits = word.chars().filter(char::is_ascii_digit).count();
            if word.contains('@') || digits >= 7 || word.chars().count() >= 48 {
                "[redacted]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn mentions(text: &str, words: &[&str]) -> bool {
    let lower = text.to_lowercase();
    words.iter().any(|w| lower.contains(w))
}

fn clip(text: &str, max: usize) -> String {
    let plain: String = text
        .chars()
        .map(|c| {
            if c.is_control() || c == '<' || c == '>' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let flat: String = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(max).collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Address,
    Find,
    Back,
    Forward,
    Reload,
    NewTab,
    Escape,
    Tab,
    PageDown,
    PageUp,
    Enter,
}

impl Key {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "address" => Self::Address,
            "find" => Self::Find,
            "back" => Self::Back,
            "forward" => Self::Forward,
            "reload" => Self::Reload,
            "newtab" => Self::NewTab,
            "escape" => Self::Escape,
            "tab" => Self::Tab,
            "pagedown" => Self::PageDown,
            "pageup" => Self::PageUp,
            "enter" => Self::Enter,
            _ => return None,
        })
    }

    fn wtype_args(self) -> &'static [&'static str] {
        match self {
            Self::Address => &["-M", "ctrl", "-k", "l", "-m", "ctrl"],
            Self::Find => &["-M", "ctrl", "-k", "f", "-m", "ctrl"],
            Self::Back => &["-M", "alt", "-k", "Left", "-m", "alt"],
            Self::Forward => &["-M", "alt", "-k", "Right", "-m", "alt"],
            Self::Reload => &["-M", "ctrl", "-k", "r", "-m", "ctrl"],
            Self::NewTab => &["-M", "ctrl", "-k", "t", "-m", "ctrl"],
            Self::Escape => &["-k", "Escape"],
            Self::Tab => &["-k", "Tab"],
            Self::PageDown => &["-k", "Next"],
            Self::PageUp => &["-k", "Prior"],
            Self::Enter => &["-k", "Return"],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UiAct {
    Click(String),
    Focus(String),
    Type(String),
    Key(Key),
}

pub trait Desktop: Send + Sync {
    fn find_window(&self, app: &str) -> Result<Win, String>;
    fn observe(&self, win: &Win) -> Result<Observation, String>;
    fn act(&self, win: &Win, act: &UiAct) -> Result<(), String>;
}

pub struct AgentDesktop {
    exe: Option<PathBuf>,
}

impl AgentDesktop {
    pub fn detect() -> Self {
        let local = dirs::home_dir()
            .map(|h| h.join(".local/opt/x/agent-desktop/node_modules/.bin/agent-desktop"));
        let exe = std::env::var_os("X_AGENT_DESKTOP")
            .map(PathBuf::from)
            .or(local)
            .filter(|p| p.is_file())
            .or_else(|| tools::which("agent-desktop").map(PathBuf::from));
        Self { exe }
    }

    fn run(&self, args: &[&str]) -> Result<Value, String> {
        let exe = self.exe.as_ref().ok_or(
            "Screen access needs agent-desktop (run scripts/install-arch.sh, or set X_AGENT_DESKTOP)",
        )?;
        valid_args(args)?;
        let (_, buf) = capture(Command::new(exe).args(args), CALL_TIMEOUT)?;
        let json: Value = serde_json::from_slice(&buf)
            .map_err(|_| "agent-desktop returned no usable answer".to_string())?;
        if json.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(json);
        }
        let code = json.pointer("/error/code").and_then(Value::as_str);
        let message = json.pointer("/error/message").and_then(Value::as_str);
        Err(match code {
            Some("STALE_REF") => "That control changed; observe again".to_string(),
            _ => format!(
                "{}: {}",
                code.unwrap_or("error"),
                message.unwrap_or("unknown")
            ),
        })
    }
}

/// Refs and window ids come from the accessibility service; never let one act as a flag.
fn valid_args(args: &[&str]) -> Result<(), String> {
    let bad = args.iter().any(|a| {
        a.starts_with('@')
            && !a[1..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ":_-.".contains(c))
            || a.starts_with("w-")
                && !a
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._".contains(c))
    });
    if bad
        || args
            .iter()
            .skip(1)
            .any(|a| a.starts_with('-') && !a.starts_with("--"))
    {
        return Err("Refused an unexpected reference".into());
    }
    Ok(())
}

/// Runs `cmd` with a deadline and capped output. Returns success and stdout.
fn capture(cmd: &mut Command, limit: Duration) -> Result<(bool, Vec<u8>), String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not run {:?}: {e}", cmd.get_program()))?;
    let mut stdout = child.stdout.take().ok_or("no output pipe")?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout).take(OUTPUT_CAP).read_to_end(&mut buf);
        buf
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{:?} timed out", cmd.get_program()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let buf = reader.join().map_err(|_| "reader failed")?;
    Ok((status.success(), buf))
}

pub(crate) fn hypr(args: &[&str]) -> Result<Value, String> {
    let (_, buf) = capture(Command::new("hyprctl").args(args), Duration::from_secs(5))?;
    serde_json::from_slice(&buf).map_err(|_| "hyprctl gave no usable answer".into())
}

fn candidates(app: &str) -> Result<Vec<String>, String> {
    let (_, names) = tools::APPS
        .iter()
        .find(|(name, _)| *name == app)
        .ok_or("Unapproved app")?;
    Ok(std::iter::once(app)
        .chain(names.iter().copied())
        .map(str::to_lowercase)
        .collect())
}

fn windows_of(app: &str) -> Result<Vec<Win>, String> {
    let names = candidates(app)?;
    let clients = hypr(&["clients", "-j"])?;
    Ok(clients
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let class = c.get("class")?.as_str()?.to_string();
            names.contains(&class.to_lowercase()).then_some(())?;
            Some(Win {
                address: c.get("address")?.as_str()?.to_string(),
                pid: u32::try_from(c.get("pid")?.as_u64()?).ok()?,
                title: c.get("title")?.as_str()?.to_string(),
                class,
                id: String::new(),
            })
        })
        .collect())
}

fn active_address() -> Option<String> {
    hypr(&["activewindow", "-j"])
        .ok()?
        .get("address")?
        .as_str()
        .map(String::from)
}

fn focus_window(win: &Win) -> Result<(), String> {
    let lua = format!("hl.dsp.focus({{ window = \"address:{}\" }})", win.address);
    tools::desktop_step(
        "hyprctl",
        &[
            "dispatch".into(),
            "focuswindow".into(),
            format!("address:{}", win.address),
        ],
        &lua,
    )?;
    for _ in 0..15 {
        if active_address().as_deref() == Some(win.address.as_str()) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("The window did not take focus".into())
}

impl Desktop for AgentDesktop {
    fn find_window(&self, app: &str) -> Result<Win, String> {
        let mut found = windows_of(app)?;
        if found.len() > 1 {
            let active = active_address();
            found.retain(|w| Some(&w.address) == active.as_ref());
            if found.len() != 1 {
                let titles: Vec<String> = windows_of(app)?.into_iter().map(|w| w.title).collect();
                return Err(format!(
                    "Several {app} windows are open ({}). Focus the one you mean and ask again.",
                    titles.join(" | ")
                ));
            }
        }
        let mut win = found
            .pop()
            .ok_or_else(|| format!("No {app} window is open"))?;
        let listed = self.run(&["list-windows"])?;
        let same_pid = listed
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|w| w.get("pid").and_then(Value::as_u64) == Some(u64::from(win.pid)));
        let id = same_pid
            .clone()
            .find(|w| w.get("title").and_then(Value::as_str) == Some(win.title.as_str()))
            .or_else(|| {
                let titled = |w: &&Value| {
                    w.get("title")
                        .and_then(Value::as_str)
                        .is_some_and(|t| !t.is_empty())
                };
                let mut named = same_pid.clone().filter(|w| titled(w));
                named.next().filter(|_| named.next().is_none())
            })
            .and_then(|w| w.get("id")?.as_str())
            .ok_or("The window has no accessibility tree (is accessibility on for this app?)")?;
        win.id = id.to_string();
        Ok(win)
    }

    fn observe(&self, win: &Win) -> Result<Observation, String> {
        let json = self.run(&["snapshot", "--window-id", &win.id])?;
        let data = json.get("data").ok_or("empty snapshot")?;
        let mut obs = Observation {
            snapshot: data
                .get("snapshot_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            title: win.title.clone(),
            ..Observation::default()
        };
        if let Some(tree) = data.get("tree") {
            collect(tree, "", &mut obs);
        }
        Ok(obs)
    }

    fn act(&self, win: &Win, act: &UiAct) -> Result<(), String> {
        match act {
            UiAct::Click(reference) => self.run(&["click", reference]).map(drop),
            UiAct::Focus(reference) => {
                focus_window(win)?;
                self.run(&["focus", reference]).map(drop)
            }
            UiAct::Type(text) => {
                focus_window(win)?;
                wtype(win, &["--", text])
            }
            UiAct::Key(key) => {
                focus_window(win)?;
                wtype(win, key.wtype_args())
            }
        }
    }
}

/// Sends keys only while `win` is still the active window, checked right before sending.
fn wtype(win: &Win, args: &[&str]) -> Result<(), String> {
    if active_address().as_deref() != Some(win.address.as_str()) {
        return Err("The window lost focus; nothing was typed".into());
    }
    let (ok, _) = capture(Command::new("wtype").args(args), Duration::from_secs(5))?;
    ok.then_some(())
        .ok_or_else(|| "wtype could not send the input".into())
}

fn collect(node: &Value, parent_name: &str, obs: &mut Observation) {
    let str_of = |key: &str| node.get(key).and_then(Value::as_str).unwrap_or_default();
    let role = str_of("role").to_lowercase();
    let name = clip(str_of("name"), MAX_NAME);
    let hidden = node
        .get("states")
        .and_then(Value::as_array)
        .is_some_and(|s| {
            s.iter()
                .any(|s| matches!(s.as_str(), Some("hidden" | "disabled" | "offscreen")))
        });
    let actions: Vec<String> = node
        .get("available_actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| a.as_str())
        .filter(|a| ACTIONABLE.contains(a))
        .map(String::from)
        .collect();
    let reference = str_of("ref_id");
    if !hidden && !SKIP_ROLES.contains(&role.as_str()) {
        let keep_control = !reference.is_empty()
            && !actions.is_empty()
            && (!name.is_empty() || role == "textfield")
            && obs.elements.len() < MAX_ELEMENTS;
        if keep_control && !(role == "statictext" && name == parent_name) {
            obs.elements.push(Element {
                id: format!("e{}", obs.elements.len() + 1),
                reference: reference.to_string(),
                role: role.clone(),
                name: name.clone(),
                value: if mentions(&name, FIELD_WORDS)
                    && !mentions(&name, SECRET_WORDS)
                    && !role.contains("password")
                {
                    clip(
                        str_of("value").split(['?', '#']).next().unwrap_or_default(),
                        MAX_NAME,
                    )
                } else {
                    String::new()
                },
                actions,
            });
        } else if !name.is_empty()
            && name != parent_name
            && matches!(
                role.as_str(),
                "statictext" | "heading" | "paragraph" | "label"
            )
            && obs.texts.len() < MAX_TEXTS
        {
            obs.texts.push(name.clone());
        }
    }
    for child in node
        .get("children")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        collect(child, &name, obs);
    }
}

#[cfg(test)]
mod tests;

//! Browser tab shortcuts: a fixed, local key mapping guarded by compositor
//! focus identity and the installed app's freedesktop WebBrowser category.
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
#[cfg(any(target_os = "linux", test))]
use crate::wm::{self, View, WinInfo};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    New,
    Close,
    Reopen,
}

pub fn parse(text: &str) -> Option<Op> {
    Some(match text.trim().to_ascii_lowercase().as_str() {
        "new tab" | "open new tab" | "open a new tab" => Op::New,
        "close this tab" | "close the tab" | "close current tab" => Op::Close,
        "reopen tab" | "reopen closed tab" | "restore closed tab" => Op::Reopen,
        _ => return None,
    })
}

#[cfg(any(target_os = "linux", test))]
fn browser_focused(view: &View, catalog: &[wm::apps::Entry]) -> Result<WinInfo, String> {
    let win = view.focused.as_ref().ok_or("No focused window")?;
    let classes = wm::apps::evidence("browser", catalog);
    if classes.is_empty() || !wm::apps::class_matches(&win.class, "browser", &classes) {
        return Err("Focused window is not a known browser; no keys were sent".into());
    }
    Ok(win.clone())
}

pub fn execute(op: Op) -> Result<String, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = op;
        Err("Browser shortcuts require Linux".into())
    }
    #[cfg(target_os = "linux")]
    {
        let catalog = wm::apps::load();
        let before = browser_focused(&wm::view().map_err(|e| e.to_string())?, &catalog)?;
        let now = browser_focused(&wm::view().map_err(|e| e.to_string())?, &catalog)?;
        if now.id != before.id || now.pid != before.pid || now.class != before.class {
            return Err("Window focus changed; no browser keys sent".into());
        }
        let exe = crate::tools::which("wtype").ok_or("wtype is required for browser shortcuts")?;
        let args: &[&str] = match op {
            Op::New => &["-M", "ctrl", "-k", "t", "-m", "ctrl"],
            Op::Close => &["-M", "ctrl", "-k", "w", "-m", "ctrl"],
            Op::Reopen => &["-M", "ctrl", "-M", "shift", "-k", "t", "-m", "shift", "-m", "ctrl"],
        };
        let status = Command::new(exe)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| "Could not send the browser shortcut")?;
        if !status.success() {
            return Err("Browser shortcut failed".into());
        }
        Ok(match op {
            Op::New => "New tab requested; page loading was not verified",
            Op::Close => "Tab close requested; say reopen tab to reverse it",
            Op::Reopen => "Closed tab restoration requested; page loading was not verified",
        }.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_voice_phrases_never_claim_ordinary_dictation() {
        assert_eq!(parse("close this tab"), Some(Op::Close));
        assert_eq!(parse("open a new tab"), Some(Op::New));
        assert_eq!(parse("reopen closed tab"), Some(Op::Reopen));
        assert_eq!(parse("I read an article about tabs"), None);
        assert_eq!(parse("close the terminal"), None);
    }

    #[test]
    fn refuses_non_browser_windows() {
        let entry = wm::apps::parse_entry(
            "firefox.desktop",
            "[Desktop Entry]\nType=Application\nName=Firefox\nCategories=WebBrowser;\nStartupWMClass=firefox\n",
        )
        .unwrap();
        let make = |class: &str| {
            let win = WinInfo {
                id: "0x1".into(),
                pid: 12,
                class: class.into(),
                title: "Page".into(),
                workspace: 1,
                place: wm::Place::N(1),
                minimized: false,
                maximized: false,
                recent: 0,
            };
            View {
                windows: vec![win.clone()],
                focused: Some(win),
                previous: None,
                workspace: Some(1),
                backend: "Fake",
                caps: vec![],
            }
        };
        assert!(browser_focused(&make("firefox"), std::slice::from_ref(&entry)).is_ok());
        assert!(browser_focused(&make("foot"), &[entry]).is_err());
        assert!(browser_focused(&make("firefox"), &[]).is_err());
    }
}

//! Which backend this session can use, and what each operation needs.

use serde::Serialize;

use super::hypr::Hypr;
use super::i3::I3;
use super::{Backend, Cap};

#[derive(Debug, Serialize)]
pub struct Report {
    pub desktop: String,
    pub session: String,
    pub backend: Option<String>,
    pub supported: Vec<Cap>,
    pub note: String,
}

fn has(bin: &str) -> bool {
    crate::tools::which(bin).is_some()
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

/// Picks a backend by what actually answers, not by `XDG_CURRENT_DESKTOP` alone.
pub fn detect() -> Result<Box<dyn Backend>, String> {
    if !env("HYPRLAND_INSTANCE_SIGNATURE").is_empty() && has("hyprctl") {
        return Ok(Box::new(Hypr));
    }
    if !env("SWAYSOCK").is_empty() && has("swaymsg") {
        return Ok(Box::new(I3 {
            bin: "swaymsg",
            label: "Sway",
        }));
    }
    if !env("I3SOCK").is_empty() && has("i3-msg") {
        return Ok(Box::new(I3 {
            bin: "i3-msg",
            label: "i3",
        }));
    }
    Err(unsupported_note())
}

fn unsupported_note() -> String {
    let desktop = env("XDG_CURRENT_DESKTOP").to_ascii_lowercase();
    let why = if desktop.contains("kde") {
        "KDE Plasma needs a KWin scripting backend, which X does not have yet"
    } else if desktop.contains("gnome") {
        "GNOME Wayland exposes no window control to other apps; X has no extension backend yet"
    } else {
        "no supported window manager answered (Hyprland, Sway, i3)"
    };
    format!("Window control is unavailable here: {why}")
}

pub fn report() -> Report {
    let (backend, supported, note) = match detect() {
        Ok(b) => (Some(b.name().to_string()), b.caps(), String::new()),
        Err(note) => (None, Vec::new(), note),
    };
    Report {
        desktop: env("XDG_CURRENT_DESKTOP"),
        session: env("XDG_SESSION_TYPE"),
        backend,
        supported,
        note,
    }
}

//! Which backend this session can use, and what each operation needs.

use serde::Serialize;
use std::path::Path;

use super::hypr::Hypr;
use super::i3::I3;
use super::x11::X11;
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
    // Avoid mistaking XWayland's DISPLAY for the real window-management
    // session. Only a native X11 session gets this EWMH backend.
    if env("XDG_SESSION_TYPE").eq_ignore_ascii_case("x11")
        && !env("DISPLAY").is_empty()
        && has("wmctrl")
        && has("xprop")
    {
        return Ok(Box::new(X11));
    }
    Err(unsupported_note())
}

fn unsupported_note() -> String {
    let desktop = env("XDG_CURRENT_DESKTOP").to_ascii_lowercase();
    let why = if env("XDG_SESSION_TYPE").eq_ignore_ascii_case("x11") {
        "X11 window control requires an active DISPLAY and the wmctrl and xprop helpers"
    } else if desktop.contains("kde") {
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

#[derive(Debug, Serialize)]
pub struct DoctorCheck {
    pub dependency: &'static str,
    pub installed: bool,
    pub purpose: &'static str,
    pub required: bool,
}

#[derive(Debug, Serialize)]
pub struct Doctor {
    pub desktop: Report,
    pub checks: Vec<DoctorCheck>,
    pub env: Vec<(&'static str, bool)>,
    pub apps: usize,
    pub shortcuts: usize,
    pub key_present: bool,
    pub key_private: bool,
    pub ready: bool,
}

fn probe_with(backend: &str, has: impl Fn(&str) -> bool) -> Vec<DoctorCheck> {
    let hyprland = backend == "Hyprland";
    let wayland = hyprland || backend == "Sway";
    let x11 = backend == "X11 EWMH";
    [
        (
            "hyprctl",
            "Hyprland window inspection and control",
            hyprland,
        ),
        ("wmctrl", "EWMH X11 window control", x11),
        ("xprop", "X11 window identity and state verification", x11),
        ("xdotool", "Optional X11 window minimization", false),
        (
            "wtype",
            "Safe Wayland typing into a focused window",
            wayland,
        ),
        ("grim", "On-demand Wayland screenshots", wayland),
        ("slurp", "Interactive screenshot region selection", false),
        ("wl-copy", "Wayland clipboard writing", false),
        ("curl", "Remote free Decider transport", false),
        (
            "agent-desktop",
            "Accessibility computer-use helper (CLI)",
            false,
        ),
        ("opencode", "Supervised Work tasks", false),
    ]
    .into_iter()
    .map(|(dependency, purpose, required)| DoctorCheck {
        dependency,
        installed: has(dependency),
        purpose,
        required,
    })
    .collect()
}

/// A quick, read-only check. It never runs a model, reads the key's contents,
/// performs a desktop action, or sends environment values to the network.
/// Environment entries describe this CLI process, not the running systemd unit.
pub fn doctor(key_path: Option<&Path>) -> Doctor {
    let desktop = report();
    let checks = probe_with(desktop.backend.as_deref().unwrap_or(""), has);
    let key = key_path.and_then(|path| std::fs::metadata(path).ok());
    let key_present = std::env::var_os("OPENROUTER_API_KEY").is_some() || key.is_some();
    #[cfg(unix)]
    let key_private = key.as_ref().is_none_or(|metadata| {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o077 == 0
    });
    #[cfg(not(unix))]
    let key_private = true;
    let ready = desktop.backend.is_some()
        && checks
            .iter()
            .all(|check| !check.required || check.installed)
        && key_private;
    Doctor {
        desktop,
        checks,
        env: [
            "WAYLAND_DISPLAY",
            "HYPRLAND_INSTANCE_SIGNATURE",
            "SWAYSOCK",
            "I3SOCK",
            "XDG_DATA_DIRS",
            "DBUS_SESSION_BUS_ADDRESS",
        ]
        .into_iter()
        .map(|key| (key, std::env::var_os(key).is_some()))
        .collect(),
        apps: super::apps::load().len(),
        shortcuts: super::keys::load().len(),
        key_present,
        key_private,
        ready,
    }
}

impl Doctor {
    pub fn lines(&self) -> String {
        let mut lines = vec![format!(
            "X diagnostics: {}",
            if self.ready {
                "ready"
            } else {
                "needs attention"
            }
        )];
        lines.push(format!(
            "Desktop: {} ({}) | window backend: {}",
            self.desktop.desktop,
            self.desktop.session,
            self.desktop.backend.as_deref().unwrap_or("unavailable")
        ));
        if !self.desktop.note.is_empty() {
            lines.push(self.desktop.note.clone());
        }
        for check in &self.checks {
            lines.push(format!(
                "{} {} — {}{}",
                if check.installed { "OK" } else { "MISSING" },
                check.dependency,
                check.purpose,
                if check.required && !check.installed {
                    " [required for this session]"
                } else {
                    ""
                }
            ));
        }
        for (name, present) in &self.env {
            lines.push(format!(
                "{} {} (current CLI environment)",
                if *present { "SET" } else { "UNSET" },
                name,
            ));
        }
        lines.push(format!(
            "Installed desktop entries: {} | known shortcuts: {}",
            self.apps, self.shortcuts,
        ));
        lines.push(format!(
            "OpenRouter key: {} | owner-only file permissions: {}",
            if self.key_present {
                "present"
            } else {
                "not configured"
            },
            if self.key_private { "OK" } else { "unsafe" },
        ));
        lines.join("\n")
    }
}

#[cfg(test)]
mod doctor_tests {
    use super::*;

    #[test]
    fn missing_dependency_is_named_and_fails_readiness() {
        let checks = probe_with("Hyprland", |bin| bin != "wtype");
        assert!(
            checks
                .iter()
                .any(|c| c.dependency == "wtype" && c.required && !c.installed)
        );
        assert!(!checks.iter().filter(|c| c.required).all(|c| c.installed));
    }

    #[test]
    fn x11_doctor_requires_ewmh_helpers_not_wayland_only_tools() {
        let checks = probe_with("X11 EWMH", |bin| bin != "xprop");
        assert!(
            checks
                .iter()
                .any(|check| check.dependency == "xprop" && check.required && !check.installed)
        );
        assert!(
            checks
                .iter()
                .filter(|check| check.dependency == "wtype" || check.dependency == "grim")
                .all(|check| !check.required)
        );
    }

    #[test]
    fn probe_lists_all_known_helpers_without_running_them() {
        let checks = probe_with("Hyprland", |_| true);
        assert_eq!(checks.len(), 11);
        assert!(checks.iter().all(|c| c.installed));
        assert!(checks.iter().any(|c| c.dependency == "curl"));
    }
}

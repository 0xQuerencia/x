//! Which backend this session can use, and what each operation needs.

use serde::Serialize;
use std::path::Path;

#[cfg(not(windows))]
use super::hypr::Hypr;
#[cfg(not(windows))]
use super::i3::I3;
#[cfg(not(windows))]
use super::kde::Kde;
#[cfg(windows)]
use super::win11::Win11;
#[cfg(not(windows))]
use super::x11::X11;
use super::{Backend, Cap};

const CAPABILITIES: [Cap; 7] = [
    Cap::List,
    Cap::Focus,
    Cap::SwitchWorkspace,
    Cap::MoveToWorkspace,
    Cap::Maximize,
    Cap::Minimize,
    Cap::Close,
];

/// What the current evidence establishes about an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportState {
    Supported,
    Partial,
    Unsupported,
    NeedsPermission,
}

/// How the report reached its conclusion, without retaining window details.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeEvidence {
    WindowListRead,
    WindowListFailed,
    BackendAdvertised,
    BackendDidNotAdvertise,
    BackendUnavailable,
    BackendOptInRequired,
}

/// Evidence for one operation on the desktop named by the enclosing report.
#[derive(Debug, Serialize)]
pub struct CapabilityReport {
    pub capability: Cap,
    pub state: SupportState,
    pub evidence: ProbeEvidence,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub desktop: String,
    pub session: String,
    pub backend: Option<String>,
    /// Legacy field containing the selected backend's advertised operations.
    pub supported: Vec<Cap>,
    pub capabilities: Vec<CapabilityReport>,
    pub note: String,
}

fn has(bin: &str) -> bool {
    crate::tools::which(bin).is_some()
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

struct SelectionFailure {
    note: String,
    opt_in_required: bool,
}

/// Select a backend using session hints and required helpers.
pub fn detect() -> Result<Box<dyn Backend>, String> {
    select().map_err(|failure| failure.note)
}

#[cfg(windows)]
fn select() -> Result<Box<dyn Backend>, SelectionFailure> {
    Ok(Box::new(Win11))
}

#[cfg(not(windows))]
fn select() -> Result<Box<dyn Backend>, SelectionFailure> {
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
    // KDE Plasma 6's window API is exposed only through KWin scripting.
    // kdotool runs a bounded script over the session D-Bus, then stops it.
    // Never treat a nested XWayland DISPLAY as a native window manager.
    if env("XDG_SESSION_TYPE").eq_ignore_ascii_case("wayland")
        && env("XDG_CURRENT_DESKTOP")
            .to_ascii_lowercase()
            .contains("kde")
        && has("kdotool")
        && !env("DBUS_SESSION_BUS_ADDRESS").is_empty()
    {
        return if env("X_ENABLE_EXPERIMENTAL_KDE") == "1" {
            Ok(Box::new(Kde))
        } else {
            Err(SelectionFailure {
                note: "KDE Plasma 6 window control requires X_ENABLE_EXPERIMENTAL_KDE=1 in this session".into(),
                opt_in_required: true,
            })
        };
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
    Err(SelectionFailure {
        note: unsupported_note(),
        opt_in_required: false,
    })
}

#[cfg(not(windows))]
fn unsupported_note() -> String {
    let desktop = env("XDG_CURRENT_DESKTOP").to_ascii_lowercase();
    let why = if env("XDG_SESSION_TYPE").eq_ignore_ascii_case("x11") {
        "X11 window control requires an active DISPLAY and the wmctrl and xprop helpers"
    } else if desktop.contains("kde") {
        "KDE Plasma 6 Wayland control is experimental and disabled by default; install kdotool, ensure session D-Bus is active, then explicitly set X_ENABLE_EXPERIMENTAL_KDE=1 to test"
    } else if desktop.contains("gnome") {
        "GNOME Wayland exposes no window control to other apps; X has no extension backend yet"
    } else {
        "no supported backend matched this session (Hyprland, Sway, i3)"
    };
    format!("Window control is unavailable here: {why}")
}

fn capability_report(
    capability: Cap,
    unavailable: Option<&SelectionFailure>,
    advertised: &[Cap],
    list_probe: Option<bool>,
) -> CapabilityReport {
    use ProbeEvidence as Evidence;

    let (state, evidence) = match unavailable {
        Some(failure) if failure.opt_in_required => (
            SupportState::NeedsPermission,
            Evidence::BackendOptInRequired,
        ),
        Some(_) => (SupportState::Unsupported, Evidence::BackendUnavailable),
        None if !advertised.contains(&capability) => {
            (SupportState::Unsupported, Evidence::BackendDidNotAdvertise)
        }
        None => match (capability, list_probe) {
            (Cap::List, Some(true)) => (SupportState::Supported, Evidence::WindowListRead),
            (_, Some(false)) => (SupportState::Partial, Evidence::WindowListFailed),
            _ => (SupportState::Partial, Evidence::BackendAdvertised),
        },
    };
    CapabilityReport {
        capability,
        state,
        evidence,
    }
}

pub fn report() -> Report {
    let backend = select();
    let (name, supported, note, list_probe) = match &backend {
        Ok(backend) => {
            let advertised = backend.caps();
            let list_probe = if advertised.contains(&Cap::List) {
                backend.probe_window_list().map(|result| result.is_ok())
            } else {
                None
            };
            (
                Some(backend.name().to_string()),
                advertised,
                String::new(),
                list_probe,
            )
        }
        Err(failure) => (None, Vec::new(), failure.note.clone(), None),
    };
    let capabilities = CAPABILITIES
        .into_iter()
        .map(|cap| capability_report(cap, backend.as_ref().err(), &supported, list_probe))
        .collect();
    Report {
        desktop: if cfg!(windows) {
            "Windows".into()
        } else {
            env("XDG_CURRENT_DESKTOP")
        },
        session: if cfg!(windows) {
            "Win32 interactive desktop".into()
        } else {
            env("XDG_SESSION_TYPE")
        },
        backend: name,
        supported,
        capabilities,
        note: if cfg!(windows) {
            "Experimental Windows 11 backend: real desktop and microphone acceptance tests pending; virtual desktop indices are unsupported".into()
        } else {
            note
        },
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
    let kde = backend == "KDE Plasma 6";
    [
        (
            "hyprctl",
            "Hyprland window inspection and control",
            hyprland,
        ),
        ("kdotool", "KWin Plasma 6 scripting over D-Bus", kde),
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
        && key_private
        && !desktop
            .capabilities
            .iter()
            .any(|capability| capability.evidence == ProbeEvidence::WindowListFailed);
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
        if let Some(list) = self
            .desktop
            .capabilities
            .iter()
            .find(|cap| cap.capability == Cap::List)
        {
            let message = match list.evidence {
                ProbeEvidence::WindowListRead => "window list read",
                ProbeEvidence::WindowListFailed => "window list unavailable",
                ProbeEvidence::BackendAdvertised => "window list advertised, no read probe",
                ProbeEvidence::BackendDidNotAdvertise => "window listing unsupported",
                ProbeEvidence::BackendUnavailable => "window backend unavailable",
                ProbeEvidence::BackendOptInRequired => "backend opt-in required",
            };
            lines.push(format!("Desktop probe: {message}"));
        }
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
    fn only_window_listing_is_supported_after_a_successful_read() {
        let reports =
            CAPABILITIES.map(|cap| capability_report(cap, None, &CAPABILITIES, Some(true)));
        let supported: Vec<_> = reports
            .iter()
            .filter(|report| report.state == SupportState::Supported)
            .map(|report| (report.capability, report.evidence))
            .collect();
        assert_eq!(supported, [(Cap::List, ProbeEvidence::WindowListRead)]);
        assert!(reports.iter().all(|report| {
            report.capability == Cap::List
                || (report.state == SupportState::Partial
                    && report.evidence == ProbeEvidence::BackendAdvertised)
        }));
    }

    #[test]
    fn selection_and_probe_failures_have_distinct_evidence() {
        use ProbeEvidence as Evidence;
        use SupportState as State;

        let unavailable = SelectionFailure {
            note: String::new(),
            opt_in_required: false,
        };
        let opt_in_required = SelectionFailure {
            note: String::new(),
            opt_in_required: true,
        };

        for (selection, caps, probe, expected) in [
            (
                None,
                &[Cap::List][..],
                None,
                (State::Partial, Evidence::BackendAdvertised),
            ),
            (
                None,
                &[Cap::List][..],
                Some(false),
                (State::Partial, Evidence::WindowListFailed),
            ),
            (
                None,
                &[][..],
                None,
                (State::Unsupported, Evidence::BackendDidNotAdvertise),
            ),
            (
                Some(&unavailable),
                &[][..],
                None,
                (State::Unsupported, Evidence::BackendUnavailable),
            ),
            (
                Some(&opt_in_required),
                &[][..],
                None,
                (State::NeedsPermission, Evidence::BackendOptInRequired),
            ),
        ] {
            let report = capability_report(Cap::List, selection, caps, probe);
            assert_eq!((report.state, report.evidence), expected);
        }
    }

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
    fn kde_requires_kwin_bridge_but_not_hyprland() {
        let checks = probe_with("KDE Plasma 6", |bin| bin != "kdotool");
        assert!(
            checks
                .iter()
                .any(|c| c.dependency == "kdotool" && c.required && !c.installed)
        );
        assert!(
            checks
                .iter()
                .any(|c| c.dependency == "hyprctl" && !c.required)
        );
    }

    #[test]
    fn probe_lists_all_known_helpers_without_running_them() {
        let checks = probe_with("Hyprland", |_| true);
        assert_eq!(checks.len(), 12);
        assert!(checks.iter().all(|c| c.installed));
        assert!(checks.iter().any(|c| c.dependency == "curl"));
    }
}

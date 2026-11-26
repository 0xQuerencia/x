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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorTyping {
    Unsupported,
    AccessibilityUnavailable,
    VerifyTargetOnUse,
}

impl EditorTyping {
    #[cfg(target_os = "linux")]
    pub(crate) fn ready(self) -> Result<(), String> {
        match self {
            Self::VerifyTargetOnUse => Ok(()),
            Self::AccessibilityUnavailable => {
                Err("The desktop accessibility registry is unavailable".into())
            }
            Self::Unsupported => {
                Err("Verified editor typing is unsupported on this desktop".into())
            }
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::AccessibilityUnavailable => "accessibility unavailable",
            Self::VerifyTargetOnUse => "accessibility answered, editor checked at execution",
        }
    }
}

pub(crate) fn editor_typing_for(backend: Option<&str>) -> EditorTyping {
    #[cfg(target_os = "linux")]
    if backend == Some("Hyprland") {
        return if crate::context::a11y::Atspi.editor_ready().is_ok() {
            EditorTyping::VerifyTargetOnUse
        } else {
            EditorTyping::AccessibilityUnavailable
        };
    }
    let _ = backend;
    EditorTyping::Unsupported
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub desktop: String,
    pub session: String,
    pub backend: Option<String>,
    /// Legacy field containing the selected backend's advertised operations.
    pub supported: Vec<Cap>,
    pub capabilities: Vec<CapabilityReport>,
    pub editor_typing: EditorTyping,
    pub note: String,
}

fn has(bin: &str) -> bool {
    crate::tools::which(bin).is_some()
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

pub(super) struct SelectionFailure {
    pub(super) note: String,
    pub(super) opt_in_required: bool,
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
    report_for(backend.as_deref())
}

pub(super) fn report_for(backend: Result<&dyn Backend, &SelectionFailure>) -> Report {
    let (name, supported, note, list_probe) = match backend {
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
        .map(|cap| capability_report(cap, backend.err(), &supported, list_probe))
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
        editor_typing: editor_typing_for(name.as_deref()),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeSource {
    Cli,
    RunningService,
}

impl ProbeSource {
    fn label(self) -> &'static str {
        match self {
            Self::Cli => "current CLI process",
            Self::RunningService => "running X service",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyFilePermissions {
    NoFile,
    OwnerOnly,
    Unsafe,
    NotChecked,
}

#[derive(Debug, Serialize)]
pub struct Doctor {
    pub source: ProbeSource,
    pub build: Option<&'static str>,
    pub os: &'static str,
    pub arch: &'static str,
    pub desktop: Report,
    pub checks: Vec<DoctorCheck>,
    pub env: Vec<(&'static str, bool)>,
    pub apps: usize,
    pub shortcuts: usize,
    pub key_present: bool,
    pub key_private: bool,
    pub key_file_permissions: KeyFilePermissions,
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
    let key = key_path
        .and_then(|path| std::fs::metadata(path).ok())
        .filter(std::fs::Metadata::is_file);
    let key_present = std::env::var_os("OPENROUTER_API_KEY").is_some() || key.is_some();
    let key_file_permissions = key
        .as_ref()
        .map_or(KeyFilePermissions::NoFile, key_permissions);
    let key_private = matches!(
        key_file_permissions,
        KeyFilePermissions::NoFile | KeyFilePermissions::OwnerOnly
    );
    let ready =
        desktop_ready(&desktop, &checks) && key_file_permissions != KeyFilePermissions::Unsafe;
    Doctor {
        source: ProbeSource::Cli,
        build: None,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
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
        key_file_permissions,
        ready,
    }
}

fn key_permissions(metadata: &std::fs::Metadata) -> KeyFilePermissions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 == 0 {
            KeyFilePermissions::OwnerOnly
        } else {
            KeyFilePermissions::Unsafe
        }
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        KeyFilePermissions::NotChecked
    }
}

fn desktop_ready(desktop: &Report, checks: &[DoctorCheck]) -> bool {
    checks
        .iter()
        .all(|check| !check.required || check.installed)
        && desktop.capabilities.iter().any(|capability| {
            capability.capability == Cap::List && capability.state == SupportState::Supported
        })
}

impl Doctor {
    pub fn with_identity(mut self, source: ProbeSource, build: &'static str) -> Self {
        self.source = source;
        self.build = Some(build);
        self
    }

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
        lines.push(format!(
            "Probe source: {} | build: {} | platform: {}/{}",
            self.source.label(),
            self.build.unwrap_or("unknown"),
            self.os,
            self.arch
        ));
        for cap in &self.desktop.capabilities {
            let name = match cap.capability {
                Cap::List => "List windows",
                Cap::Focus => "Focus windows",
                Cap::SwitchWorkspace => "Switch workspace",
                Cap::MoveToWorkspace => "Move windows to workspace",
                Cap::Maximize => "Maximize windows",
                Cap::Minimize => "Minimize windows",
                Cap::Close => "Close windows",
            };
            let state = match cap.state {
                SupportState::Supported => "supported",
                SupportState::Partial => "partial",
                SupportState::Unsupported => "unsupported",
                SupportState::NeedsPermission => "needs permission",
            };
            let detail = match cap.evidence {
                ProbeEvidence::WindowListRead => "read succeeded",
                ProbeEvidence::WindowListFailed => "read failed",
                ProbeEvidence::BackendAdvertised => "advertised, no probe evidence",
                ProbeEvidence::BackendDidNotAdvertise => "not advertised by the selected backend",
                ProbeEvidence::BackendUnavailable => "window backend unavailable",
                ProbeEvidence::BackendOptInRequired => "backend opt-in required",
            };
            lines.push(format!("{name}: {state} ({detail})"));
        }
        lines.push(format!(
            "Verified editor typing: {}",
            self.desktop.editor_typing.label()
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
                "{} {} ({})",
                if *present { "SET" } else { "UNSET" },
                name,
                self.source.label(),
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
            match self.key_file_permissions {
                KeyFilePermissions::NoFile => "no key file",
                KeyFilePermissions::OwnerOnly => "verified",
                KeyFilePermissions::Unsafe => "unsafe",
                KeyFilePermissions::NotChecked => "not checked",
            },
        ));
        lines.join("\n")
    }
}

#[cfg(test)]
mod doctor_tests {
    use super::*;

    #[test]
    fn unverified_platforms_do_not_advertise_editor_typing() {
        for backend in [
            None,
            Some("KDE Plasma 6"),
            Some("Windows 11 (Win32)"),
            Some("macOS Accessibility"),
        ] {
            assert_eq!(editor_typing_for(backend), EditorTyping::Unsupported);
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
    fn advertised_window_listing_is_not_ready_without_readback() {
        let checks = probe_with("Hyprland", |_| true);
        let mut desktop = Report {
            editor_typing: EditorTyping::Unsupported,
            desktop: "Hyprland".into(),
            session: "wayland".into(),
            backend: Some("Hyprland".into()),
            supported: vec![Cap::List],
            capabilities: vec![CapabilityReport {
                capability: Cap::List,
                state: SupportState::Partial,
                evidence: ProbeEvidence::BackendAdvertised,
            }],
            note: String::new(),
        };
        assert!(!desktop_ready(&desktop, &checks));
        desktop.capabilities[0].state = SupportState::Supported;
        desktop.capabilities[0].evidence = ProbeEvidence::WindowListRead;
        assert!(desktop_ready(&desktop, &checks));
        assert!(!desktop_ready(
            &desktop,
            &probe_with("Hyprland", |bin| bin != "wtype")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn checks_permissions_without_exposing_key_contents() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test-key");
        std::fs::write(&path, "secret test key").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let diagnostic = doctor(Some(&path)).with_identity(ProbeSource::RunningService, "fixture");
        assert_eq!(diagnostic.key_file_permissions, KeyFilePermissions::Unsafe);
        assert!(!diagnostic.ready);
        let report = serde_json::to_string(&diagnostic).unwrap();
        assert!(!report.contains("secret test key"));
        assert!(!report.contains("test-key"));
        assert!(diagnostic.lines().contains("running X service"));
    }

    #[test]
    fn probe_lists_all_known_helpers_without_running_them() {
        let checks = probe_with("Hyprland", |_| true);
        assert_eq!(checks.len(), 12);
        assert!(checks.iter().all(|c| c.installed));
        assert!(checks.iter().any(|c| c.dependency == "curl"));
    }
}

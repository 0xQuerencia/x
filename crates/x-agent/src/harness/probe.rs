use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{Capability, Kind};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Compatibility {
    Missing,
    Compatible,
    Incompatible,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Auth {
    Configured,
    NeedsLogin,
    Unknown,
}

impl Compatibility {
    pub fn label(self) -> &'static str {
        match self {
            Self::Missing => "not installed",
            Self::Compatible => "required CLI flags found",
            Self::Incompatible => "required CLI flags missing",
            Self::Unknown => "unknown",
        }
    }
}

impl Auth {
    pub fn label(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::NeedsLogin => "login needed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Availability {
    pub kind: Kind,
    pub executable: Option<PathBuf>,
    pub version: Option<String>,
    pub compatibility: Compatibility,
    pub auth: Auth,
    pub capabilities: &'static [Capability],
}

pub fn locate(kind: Kind) -> Option<PathBuf> {
    let name = match kind {
        Kind::OpenCode => "opencode",
        Kind::ClaudeCode => "claude",
    };
    let selected = crate::tools::which(name).map(PathBuf::from).or_else(|| {
        let root = dirs::home_dir()?;
        let path = match kind {
            Kind::OpenCode => root.join(".opencode/bin").join(name),
            Kind::ClaudeCode => root.join(".local/bin").join(name),
        };
        path.is_file().then_some(path)
    })?;
    selected.canonicalize().ok().filter(|path| path.is_file())
}

pub fn local() -> [Availability; 2] {
    std::thread::scope(|scope| {
        let open_code = scope.spawn(|| inspect(Kind::OpenCode, locate(Kind::OpenCode), query));
        let claude_code = inspect(Kind::ClaudeCode, locate(Kind::ClaudeCode), query);
        [
            open_code.join().unwrap_or_else(|_| absent(Kind::OpenCode)),
            claude_code,
        ]
    })
}

fn absent(kind: Kind) -> Availability {
    Availability {
        kind,
        executable: None,
        version: None,
        compatibility: Compatibility::Missing,
        auth: Auth::Unknown,
        capabilities: &[],
    }
}

fn inspect(
    kind: Kind,
    executable: Option<PathBuf>,
    run: impl Fn(&Path, &[&str]) -> Option<(bool, String)>,
) -> Availability {
    let mut result = absent(kind);
    let Some(exe) = executable else {
        return result;
    };
    result.compatibility = Compatibility::Unknown;
    result.executable = Some(exe.clone());
    result.version = run(&exe, &["--version"])
        .filter(|(ok, _)| *ok)
        .and_then(|(_, output)| parse_version(kind, &output));
    if result.version.is_none() {
        return result;
    }

    let help_args: &[&str] = match kind {
        Kind::OpenCode => &["run", "--help"],
        Kind::ClaudeCode => &["--help"],
    };
    let Some((true, help)) = run(&exe, help_args) else {
        return result;
    };
    let compatible = match kind {
        Kind::OpenCode => has_opencode_json_output(&help),
        Kind::ClaudeCode => {
            result.version.as_ref().is_some_and(|v| v.starts_with("2."))
                && super::claude::supports_tool_free_output(&help)
        }
    };
    if !compatible {
        result.compatibility = Compatibility::Incompatible;
        return result;
    }
    result.compatibility = Compatibility::Compatible;
    result.capabilities = match kind {
        Kind::OpenCode => super::OPENCODE_OWNED_CAPABILITIES,
        Kind::ClaudeCode => super::claude::CAPABILITIES,
    };
    let probe: &[&str] = match kind {
        Kind::OpenCode if has_flag(&help, "--standalone") => {
            &["auth", "list", "--format", "json", "--standalone"]
        }
        Kind::OpenCode => &["auth", "list", "--format", "json"],
        Kind::ClaudeCode => &["auth", "status", "--json"],
    };
    if let Some((success, output)) = run(&exe, probe) {
        result.auth = auth(kind, success, &output);
    }
    result
}

pub(crate) fn has_opencode_json_output(help: &str) -> bool {
    has_flag(help, "--format") && help.contains("json")
}

pub(crate) fn has_flag(help: &str, name: &str) -> bool {
    help.lines().any(|line| {
        line.trim_start()
            .split([',', '=', ' ', '\t'])
            .filter(|word| !word.is_empty())
            .take(2)
            .any(|word| word == name)
    })
}

pub(crate) fn query(exe: &Path, args: &[&str]) -> Option<(bool, String)> {
    let (success, output) =
        crate::desktop::capture(Command::new(exe).args(args), Duration::from_secs(2)).ok()?;
    String::from_utf8(output)
        .ok()
        .map(|output| (success, output))
}

pub(crate) fn parse_version(kind: Kind, output: &str) -> Option<String> {
    let trimmed = output.trim();
    let text = match kind {
        Kind::OpenCode => trimmed.strip_prefix("opencode ").unwrap_or(trimmed),
        Kind::ClaudeCode => trimmed.strip_suffix(" (Claude Code)").unwrap_or(trimmed),
    };
    let candidate = text.strip_prefix('v').unwrap_or(text);
    let (core, suffix) = candidate.split_once(['-', '+']).unwrap_or((candidate, ""));
    let mut numbers = core.split('.');
    for _ in 0..3 {
        numbers.next()?.parse::<u32>().ok()?;
    }
    if numbers.next().is_some()
        || candidate.len() > 64
        || !suffix
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'+'))
    {
        return None;
    }
    Some(candidate.into())
}

pub(crate) fn auth(kind: Kind, success: bool, output: &str) -> Auth {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ClaudeAuth {
        logged_in: bool,
    }

    #[derive(Deserialize)]
    struct OpenCodeCredential {
        provider: String,
    }

    match kind {
        Kind::ClaudeCode => match serde_json::from_str::<ClaudeAuth>(output) {
            Ok(ClaudeAuth { logged_in: true }) if success => Auth::Configured,
            Ok(ClaudeAuth { logged_in: false }) => Auth::NeedsLogin,
            _ => Auth::Unknown,
        },
        Kind::OpenCode if success => {
            match serde_json::from_str::<Vec<OpenCodeCredential>>(output) {
                Ok(entries) if entries.is_empty() => Auth::NeedsLogin,
                Ok(entries) if entries.iter().any(|entry| !entry.provider.is_empty()) => {
                    Auth::Configured
                }
                _ => Auth::Unknown,
            }
        }
        Kind::OpenCode => Auth::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_incompatible_harnesses_report_different_states() {
        let unavailable = inspect(Kind::OpenCode, None, |_, _| panic!("nothing to invoke"));
        assert_eq!(unavailable.compatibility, Compatibility::Missing);
        assert!(unavailable.capabilities.is_empty());
        let incompatible =
            inspect(
                Kind::OpenCode,
                Some(PathBuf::from("fixture")),
                |_, args| match args {
                    ["--version"] => Some((true, "opencode v2.2.7".into())),
                    ["run", "--help"] => Some((true, "--continue".into())),
                    _ => panic!("nothing to authenticate"),
                },
            );
        assert_eq!(incompatible.compatibility, Compatibility::Incompatible);
        assert_eq!(incompatible.version.as_deref(), Some("2.2.7"));
        assert!(incompatible.capabilities.is_empty());
    }

    #[test]
    fn compatible_installation_keeps_auth_unknown_after_an_unavailable_probe() {
        let report = inspect(
            Kind::OpenCode,
            Some(PathBuf::from("fixture")),
            |_, args| match args {
                ["--version"] => Some((true, "opencode v2.2.7".into())),
                ["run", "--help"] => Some((true, "--format choice (default, json)".into())),
                ["auth", "list", "--format", "json"] => None,
                _ => panic!("unexpected probe"),
            },
        );
        assert_eq!(report.compatibility, Compatibility::Compatible);
        assert_eq!(report.auth, Auth::Unknown);
        assert!(report.capabilities.contains(&Capability::OwnedRun));
    }

    #[test]
    fn auth_status_never_retains_identity_or_credentials() {
        for (logged_in, expected) in [(true, Auth::Configured), (false, Auth::NeedsLogin)] {
            let report = inspect(
                Kind::ClaudeCode,
                Some(PathBuf::from("fixture")),
                |_, args| {
                    match args {
                    ["--version"] => Some((true, "2.1.328 (Claude Code)".into())),
                    ["--help"] => Some((
                        true,
                        "--print\n--resume\n--output-format stream-json\n--permission-mode\n--tools\n--strict-mcp-config\n--mcp-config\n--setting-sources\n--disable-slash-commands".into(),
                    )),
                    ["auth", "status", "--json"] => Some((
                        logged_in,
                        format!(
                            r#"{{"loggedIn":{logged_in},"email":"private@example.com","credential":"secret"}}"#
                        ),
                    )),
                    _ => panic!("unexpected probe"),
                }
                },
            );
            assert_eq!(report.auth, expected);
            assert!(
                report
                    .capabilities
                    .contains(&Capability::ResumeOwnedSession)
            );
            let json = serde_json::to_string(&report).unwrap();
            assert!(!json.contains("private@example.com"));
            assert!(!json.contains("secret"));
        }
    }

    #[test]
    fn open_code_auth_returns_needs_login_only_for_a_recognized_empty_list() {
        assert_eq!(auth(Kind::OpenCode, true, "[]"), Auth::NeedsLogin);
        assert_eq!(auth(Kind::OpenCode, false, "[]"), Auth::Unknown);
        assert_eq!(auth(Kind::OpenCode, true, "{}"), Auth::Unknown);
    }

    #[test]
    #[ignore = "requires the two harness CLIs on this machine"]
    fn installed_binary_flags_are_probed_without_a_model_call() {
        for report in local() {
            assert!(report.executable.is_some(), "{}", report.kind.label());
            assert!(report.version.is_some(), "{}", report.kind.label());
            assert_eq!(report.compatibility, Compatibility::Compatible);
            eprintln!(
                "{} {}: {}; login {}",
                report.kind.label(),
                report.version.as_deref().unwrap(),
                report.compatibility.label(),
                report.auth.label()
            );
        }
    }
}

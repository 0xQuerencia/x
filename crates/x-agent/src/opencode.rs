//! A supervised `opencode run`: real event stream, honest status, real Stop.
//!
//! Never passes `--auto`, so OpenCode's own permission gates stay in force. Success needs
//! exit code 0 AND no `error` event; an `error` event is terminal (observed: OpenCode can
//! hang after emitting one).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use crate::cancellation::Cancellation;
pub use crate::harness::evidence::{change_snapshot, diff_snapshots};
pub use crate::harness::process::Canceller;
use crate::harness::{self, EventSink, SessionId};
pub use crate::harness::{RunResult, RunStatus};

mod events;

pub const STALL_AFTER: Duration = Duration::from_secs(120);

/// Receives `(kind, one-line text)` for each event as it happens.
pub type OnEvent = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// Reject missing paths, files, `/`, and the whole home directory.
pub fn validate_workspace(path: &Path) -> Result<PathBuf, String> {
    let resolved = path
        .canonicalize()
        .map_err(|e| format!("Workspace not found: {}: {e}", path.display()))?;
    if !resolved.is_dir() {
        return Err("Workspace is not a directory".into());
    }
    let home = dirs::home_dir().and_then(|h| h.canonicalize().ok());
    // macOS aliases /etc -> /private/etc and /var -> /private/var.
    // Reject canonical system roots too; path component counts alone
    // would otherwise silently authorize a symlinked broad directory.
    let protected_root = [
        "/etc",
        "/usr",
        "/var",
        "/tmp",
        "/private",
        "/System",
        "/Library",
        "/Applications",
        "/opt",
        "/home",
    ]
    .iter()
    .filter_map(|name| Path::new(name).canonicalize().ok())
    .any(|root| root == resolved);
    let too_broad = protected_root
        || resolved.components().count() <= 2
        || home.as_deref().is_some_and(|h| h.starts_with(&resolved))
        || home.as_deref().is_some_and(|h| {
            resolved.strip_prefix(h).ok().is_some_and(|rel| {
                rel.components()
                    .next()
                    .is_some_and(|c| c.as_os_str().to_string_lossy().starts_with('.'))
            })
        });
    if too_broad {
        return Err(
            "Choose a project directory, not a system folder, your home directory or a hidden config folder"
                .into(),
        );
    }
    Ok(resolved)
}

/// Summarize a parsed event for callers of the text-only runner interface.
pub fn summarize_event(obj: &serde_json::Value) -> (String, String) {
    let event = events::parse(&obj.to_string());
    (event.kind.as_str().into(), event.text)
}

#[derive(Default)]
struct RunFlags {
    standalone: bool,
    directory: bool,
    json: bool,
    session: bool,
}

impl RunFlags {
    fn read(exe: &Path) -> Option<Self> {
        let (true, help) = harness::probe::query(exe, &["run", "--help"])? else {
            return None;
        };
        let flag = |name| harness::probe::has_flag(&help, name);
        Some(Self {
            standalone: flag("--standalone"),
            directory: flag("--dir"),
            json: harness::probe::has_opencode_json_output(&help),
            session: flag("--session"),
        })
    }
}

fn supports_context_policy(exe: &Path) -> bool {
    let Some((true, output)) = harness::probe::query(exe, &["--version"]) else {
        return false;
    };
    let Some(version) = harness::probe::parse_version(harness::Kind::OpenCode, &output) else {
        return false;
    };
    let release = version
        .split_once(['-', '+'])
        .map_or(version.as_str(), |(release, _)| release);
    let components: Vec<_> = release
        .split('.')
        .filter_map(|part| part.parse::<u32>().ok())
        .collect();
    matches!(components.as_slice(), [2, minor, patch] if (*minor, *patch) >= (2, 7))
}

pub struct RunConfig {
    pub exe: PathBuf,
    pub prompt: String,
    pub workspace: PathBuf,
    pub session: Option<SessionId>,
    pub access: harness::WorkAccess,
    pub stall_after: Duration,
    /// Exact shell commands the person allowed for this one run.
    pub allow_commands: Vec<String>,
}

/// Commands that only read or check a project; everything else OpenCode wants to run is
/// refused (a headless run cannot ask) and reported so the person can allow it once.
const SAFE_COMMANDS: &[&str] = &[
    "ls*",
    "pwd",
    "cat *",
    "head *",
    "tail *",
    "wc *",
    "grep *",
    "rg *",
    "find * -name *",
    "git status*",
    "git diff*",
    "git log*",
    "git show*",
    "git branch",
    "cargo check*",
    "cargo test*",
    "cargo clippy*",
    "cargo fmt --check*",
    "npm test*",
    "npm run test*",
    "pnpm test*",
    "yarn test*",
    "pytest*",
    "python -m pytest*",
    "go test*",
    "go vet*",
    "make test*",
];

impl RunConfig {
    pub fn new(exe: PathBuf, prompt: impl Into<String>, workspace: PathBuf) -> Self {
        Self {
            exe,
            prompt: prompt.into(),
            workspace,
            session: None,
            access: harness::WorkAccess::Workspace,
            stall_after: STALL_AFTER,
            allow_commands: vec![],
        }
    }

    /// OpenCode config merged over the person's own for this run only: edits stay in the
    /// workspace, unknown shell commands and other directories are refused, secrets stay unread.
    pub fn policy(&self) -> String {
        if self.access == harness::WorkAccess::SelectionOnly {
            let policies = [
                "tool.use",
                "skill.use",
                "mcp.use",
                "plugin.use",
                "websearch.use",
            ]
            .map(|action| serde_json::json!({"action": action, "resource": "*", "effect": "deny"}));
            return serde_json::json!({
                "permissions": [{"action": "*", "resource": "*", "effect": "deny"}],
                "policies": policies,
            })
            .to_string();
        }

        let mut bash = serde_json::Map::new();
        bash.insert("*".into(), "ask".into());
        for command in SAFE_COMMANDS {
            bash.insert((*command).into(), "allow".into());
        }
        for command in self
            .allow_commands
            .iter()
            .filter(|c| !c.contains(['*', '?', '\n']))
        {
            bash.insert(command.clone(), "allow".into());
        }
        serde_json::json!({
            "permission": {
                "bash": bash,
                "edit": "allow",
                "external_directory": "deny",
                "webfetch": "ask",
                "read": { "*": "allow", "*.env": "deny", "*.env.*": "deny", "*.pem": "deny", "*id_rsa*": "deny" },
            }
        })
        .to_string()
    }

    /// The exact argv. OpenCode also receives the approved project as its working directory.
    pub fn args(&self) -> Vec<String> {
        self.args_for(RunFlags::read(&self.exe).unwrap_or_default())
    }

    fn args_for(&self, flags: RunFlags) -> Vec<String> {
        let mut args = vec!["run".to_string()];
        if flags.standalone {
            args.push("--standalone".into());
        }
        if flags.directory {
            args.extend([
                "--dir".into(),
                self.workspace.to_string_lossy().into_owned(),
            ]);
        }
        if let Some(id) = &self.session {
            args.extend(["--session".into(), id.as_str().into()]);
        }
        args.extend([
            "--format".into(),
            "json".into(),
            "--".into(),
            self.prompt.clone(),
        ]);
        args
    }
}

pub struct Adapter {
    exe: PathBuf,
}

impl Adapter {
    pub fn new(exe: PathBuf) -> Self {
        Self { exe }
    }
}

impl harness::Adapter for Adapter {
    fn descriptor(&self) -> harness::Descriptor {
        harness::Descriptor {
            kind: harness::Kind::OpenCode,
            event_schema: 2,
            version: None,
            capabilities: harness::OPENCODE_OWNED_CAPABILITIES,
        }
    }

    fn start(
        &self,
        request: harness::Request,
        events: EventSink,
        stop: &Cancellation,
    ) -> Result<Box<dyn harness::OwnedRun>, String> {
        let mut config = RunConfig::new(self.exe.clone(), request.prompt, request.workspace);
        config.allow_commands = request.allow_commands;
        config.session = request.session;
        config.access = request.access;
        Ok(Box::new(Run::start_events_scoped(config, events, stop)?))
    }
}

impl harness::OwnedRun for Run {
    fn join(self: Box<Self>) -> RunResult {
        (*self).join()
    }
}

pub struct Run(crate::harness::process::Run);

impl Run {
    pub fn start(cfg: RunConfig, on_event: OnEvent) -> Result<Self, String> {
        Self::start_scoped(cfg, on_event, &Cancellation::default())
    }

    pub fn start_scoped(
        cfg: RunConfig,
        on_event: OnEvent,
        parent: &Cancellation,
    ) -> Result<Self, String> {
        Self::start_events_scoped(
            cfg,
            Arc::new(move |event| on_event(event.kind.as_str(), &event.text)),
            parent,
        )
    }

    fn start_events_scoped(
        cfg: RunConfig,
        on_event: EventSink,
        parent: &Cancellation,
    ) -> Result<Self, String> {
        parent.check()?;
        let flags = RunFlags::read(&cfg.exe)
            .filter(|flags| flags.json)
            .ok_or("OpenCode did not advertise structured run output")?;
        if cfg.session.is_some() && (!flags.session || !flags.standalone) {
            return Err(
                "The installed OpenCode does not support a private run against an exact session"
                    .into(),
            );
        }
        if cfg.access == harness::WorkAccess::SelectionOnly && !supports_context_policy(&cfg.exe) {
            return Err(
                "Sharing a selection requires OpenCode 2.2.7 or a newer OpenCode 2 build".into(),
            );
        }
        let mut cmd = Command::new(&cfg.exe);
        cmd.args(cfg.args_for(flags))
            .env("PWD", &cfg.workspace)
            .env("OPENCODE_CONFIG_CONTENT", cfg.policy());
        let spec = crate::harness::process::Spec {
            name: "OpenCode",
            workspace: cfg.workspace,
            stall_after: cfg.stall_after,
            session: cfg.session,
            require_terminal: false,
            parse: events::parse,
        };
        crate::harness::process::Run::start(spec, cmd, on_event, parent).map(Self)
    }

    pub fn canceller(&self) -> Canceller {
        self.0.canceller()
    }
    pub fn cancel(&self) {
        self.0.cancel()
    }
    pub fn join(self) -> RunResult {
        self.0.join()
    }
}

#[cfg(all(test, unix))]
mod tests;

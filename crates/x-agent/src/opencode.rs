//! A supervised `opencode run`: real event stream, honest status, real Stop.
//!
//! Never passes `--auto`, so OpenCode's own permission gates stay in force. Success needs
//! exit code 0 AND no `error` event; an `error` event is terminal (observed: OpenCode can
//! hang after emitting one).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::cancellation::Cancellation;
use crate::harness::{self, Event, EventKind, EventSink, SessionId};
pub use crate::harness::{RunResult, RunStatus};

mod events;

const POLL: Duration = Duration::from_millis(200);
const KILL_GRACE: Duration = Duration::from_secs(5);
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
    let too_broad = resolved.components().count() <= 2
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

fn git(workspace: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// path -> porcelain status. `None` when the workspace is not a git repo.
pub fn change_snapshot(workspace: &Path) -> Option<BTreeMap<String, String>> {
    let raw = git(
        workspace,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
    )?;
    let mut entries = raw.split('\0');
    let mut snap = BTreeMap::new();
    while let Some(e) = entries.next() {
        if e.len() > 3 {
            snap.insert(e[3..].to_string(), e[..2].to_string());
            if e.starts_with(['R', 'C']) {
                entries.next(); // rename/copy carries the original path as a second field
            }
        }
    }
    Some(snap)
}

pub fn diff_snapshots(
    before: Option<&BTreeMap<String, String>>,
    after: Option<&BTreeMap<String, String>>,
) -> Option<Vec<String>> {
    let (b, a) = (before?, after?);
    let paths: BTreeSet<&String> = b.keys().chain(a.keys()).collect();
    Some(
        paths
            .into_iter()
            .filter(|p| b.get(*p) != a.get(*p))
            .cloned()
            .collect(),
    )
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

pub struct RunConfig {
    pub exe: PathBuf,
    pub prompt: String,
    pub workspace: PathBuf,
    pub session: Option<SessionId>,
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
            stall_after: STALL_AFTER,
            allow_commands: vec![],
        }
    }

    /// OpenCode config merged over the person's own for this run only: edits stay in the
    /// workspace, unknown shell commands and other directories are refused, secrets stay unread.
    pub fn policy(&self) -> String {
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
        Ok(Box::new(Run::start_events_scoped(config, events, stop)?))
    }
}

impl harness::OwnedRun for Run {
    fn join(self: Box<Self>) -> RunResult {
        (*self).join()
    }
}

pub struct Run {
    canceller: Canceller,
    handle: Option<JoinHandle<RunResult>>,
}

/// A cloneable handle that can Stop a run from any thread.
#[derive(Clone)]
pub struct Canceller {
    stop: Cancellation,
}

impl Canceller {
    /// Request Stop. The supervisor that owns the child handles termination.
    pub fn cancel(&self) {
        self.stop.request();
    }
}

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
        let stop = parent.child();
        stop.check()?;
        let before = change_snapshot(&cfg.workspace);
        stop.check()?;
        let flags = RunFlags::read(&cfg.exe)
            .filter(|flags| flags.json)
            .ok_or("OpenCode did not advertise structured run output")?;
        if cfg.session.is_some() && (!flags.session || !flags.standalone) {
            return Err(
                "The installed OpenCode does not support a private run against an exact session"
                    .into(),
            );
        }
        let args = cfg.args_for(flags);
        let mut child = crate::retry_busy(|| {
            if stop.requested() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "Stopped",
                ));
            }
            let mut cmd = Command::new(&cfg.exe);
            cmd.args(&args)
                .current_dir(&cfg.workspace)
                .env("PWD", &cfg.workspace)
                .env("OPENCODE_CONFIG_CONTENT", cfg.policy())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            // Its own process group, so Stop reaches everything OpenCode started.
            #[cfg(unix)]
            cmd.process_group(0);
            #[cfg(windows)]
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
            cmd.spawn()
        })
        .map_err(|e| format!("could not start {}: {e}", cfg.exe.display()))?;
        let child_stop = stop.clone();
        let handle =
            thread::spawn(move || supervise(&mut child, &cfg, before, &child_stop, on_event));
        Ok(Self {
            canceller: Canceller { stop },
            handle: Some(handle),
        })
    }

    pub fn canceller(&self) -> Canceller {
        self.canceller.clone()
    }

    /// Stop the run: SIGTERM the whole process group, SIGKILL after a grace period.
    pub fn cancel(&self) {
        self.canceller.cancel();
    }

    pub fn join(mut self) -> RunResult {
        self.handle
            .take()
            .and_then(|handle| handle.join().ok())
            .unwrap_or(RunResult {
                status: RunStatus::Failed,
                exit_code: None,
                accepted_session: None,
                errors: vec!["supervisor thread panicked".into()],
                stderr: vec![],
                changed_files: None,
            })
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if self.handle.is_some() {
            self.cancel();
        }
    }
}

#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// A polite stop first, then a forced one.
#[derive(Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
fn signal_group(pgid: i32, sig: Signal) {
    if pgid > 0 {
        let sig = match sig {
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        };
        // SAFETY: plain syscall on a process group we created with process_group(0).
        unsafe { libc::killpg(pgid, sig) };
    }
}

/// Windows has no process-group signals; `taskkill /T` ends the whole process tree.
#[cfg(windows)]
fn signal_group(pgid: i32, sig: Signal) {
    if pgid > 0 {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/PID", &pgid.to_string(), "/T"]);
        if matches!(sig, Signal::Kill) {
            cmd.arg("/F");
        }
        let _ = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn terminate(child: &mut Child) {
    let pgid = child.id() as i32;
    signal_group(pgid, Signal::Term);
    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            signal_group(pgid, Signal::Kill); // reap any stragglers in the group
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    signal_group(pgid, Signal::Kill);
    let _ = child.wait();
}

enum Line {
    Out(String),
    Err(String),
    Closed,
}

fn pump(stream: impl std::io::Read + Send + 'static, out: bool, tx: mpsc::Sender<Line>) {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let _ = tx.send(if out {
                Line::Out(line)
            } else {
                Line::Err(line)
            });
        }
        let _ = tx.send(Line::Closed);
    });
}

fn supervise(
    child: &mut Child,
    cfg: &RunConfig,
    before: Option<BTreeMap<String, String>>,
    stop: &Cancellation,
    on_event: EventSink,
) -> RunResult {
    let (tx, rx) = mpsc::channel();
    if let Some(s) = child.stdout.take() {
        pump(s, true, tx.clone());
    }
    if let Some(s) = child.stderr.take() {
        pump(s, false, tx.clone());
    }
    drop(tx);
    let (mut open, mut last, mut stalled) = (2, Instant::now(), false);
    let (mut errors, mut stderr_tail): (Vec<String>, Vec<String>) = (vec![], vec![]);
    let mut accepted_session = None;
    while open > 0 && !stop.requested() {
        match rx.recv_timeout(POLL) {
            Err(RecvTimeoutError::Timeout) => {
                if !stalled && last.elapsed() > cfg.stall_after {
                    stalled = true;
                    on_event(Event::new(
                        EventKind::Stall,
                        format!(
                            "No output for {}s: the agent may be waiting on a permission or the provider. Stop to abort.",
                            cfg.stall_after.as_secs()
                        ),
                    ));
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Ok(Line::Closed) => open -= 1,
            Ok(Line::Err(l)) => {
                (last, stalled) = (Instant::now(), false);
                stderr_tail.push(l.trim().to_string());
                if stderr_tail.len() > 5 {
                    stderr_tail.remove(0);
                }
            }
            Ok(Line::Out(l)) => {
                (last, stalled) = (Instant::now(), false);
                let l = l.trim();
                if l.is_empty() {
                    continue;
                }
                let event = events::parse(l);
                if let (Some(expected), Some(reported)) = (&cfg.session, &event.session)
                    && expected != reported
                {
                    let message = "OpenCode reported a different session";
                    errors.push(message.into());
                    on_event(Event::new(EventKind::Error, message));
                    break;
                }
                if accepted_session.is_none() {
                    accepted_session = event.accepted_session().cloned();
                }
                if event.kind == EventKind::Error {
                    errors.push(event.text.clone());
                    on_event(event);
                    break;
                }
                on_event(event);
            }
        }
    }
    let exit_code = if stop.requested() || !errors.is_empty() {
        terminate(child);
        child.wait().ok().and_then(|s| s.code())
    } else {
        match wait_with_grace(child, stop) {
            Some(s) => s.code(),
            None => {
                terminate(child);
                child.wait().ok().and_then(|s| s.code())
            }
        }
    };
    let changed_files = diff_snapshots(before.as_ref(), change_snapshot(&cfg.workspace).as_ref());
    if !stop.requested() && cfg.session.is_some() && accepted_session.is_none() && errors.is_empty()
    {
        errors.push("OpenCode did not acknowledge the selected session".into());
    }
    let status = if stop.requested() {
        RunStatus::Cancelled
    } else if !errors.is_empty() || exit_code != Some(0) {
        RunStatus::Failed
    } else {
        RunStatus::Completed
    };
    stderr_tail.retain(|l| !l.is_empty());
    RunResult {
        status,
        exit_code,
        accepted_session,
        errors,
        stderr: stderr_tail,
        changed_files,
    }
}

fn wait_with_grace(child: &mut Child, stop: &Cancellation) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline && !stop.requested() {
        if let Ok(Some(s)) = child.try_wait() {
            return Some(s);
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

#[cfg(all(test, unix))]
mod tests;

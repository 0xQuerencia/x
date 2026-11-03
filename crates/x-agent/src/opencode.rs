//! A supervised `opencode run`: real event stream, honest status, real Stop.
//!
//! Never passes `--auto`, so OpenCode's own permission gates stay in force. Success needs
//! exit code 0 AND no `error` event; an `error` event is terminal (observed: OpenCode can
//! hang after emitting one).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Serialize;

const POLL: Duration = Duration::from_millis(200);
const KILL_GRACE: Duration = Duration::from_secs(5);
const MAX_EVENT_CHARS: usize = 450;
pub const STALL_AFTER: Duration = Duration::from_secs(120);

/// Receives `(kind, one-line text)` for each event as it happens.
pub type OnEvent = Arc<dyn Fn(&str, &str) + Send + Sync>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
pub struct RunResult {
    pub status: RunStatus,
    pub exit_code: Option<i32>,
    pub errors: Vec<String>,
    pub stderr: Vec<String>,
    /// Files whose git state changed during the run; `None` outside a git repo.
    pub changed_files: Option<Vec<String>>,
}

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

/// Classify one parsed event into `(kind, summary)`.
pub fn summarize_event(obj: &serde_json::Value) -> (String, String) {
    let clip = |s: &str| s.chars().take(MAX_EVENT_CHARS).collect::<String>();
    let kind = obj.get("type").and_then(|v| v.as_str()).unwrap_or("event");
    let part = obj.get("part").filter(|p| p.is_object());
    if kind == "error" {
        let msg = match obj.get("error") {
            Some(e) => e
                .get("message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| e.to_string()),
            None => "unknown error".into(),
        };
        return ("error".into(), clip(&msg));
    }
    if let Some(tool) = part.and_then(|p| p.get("tool")).and_then(|t| t.as_str()) {
        let state = part.and_then(|p| p.get("state"));
        let refused = state
            .and_then(|s| s.get("error"))
            .and_then(|e| e.as_str())
            .is_some_and(|e| e.contains("rejected permission"));
        if refused {
            let command = state
                .and_then(|s| s.get("input"))
                .and_then(|i| {
                    i.get("command")
                        .or_else(|| i.get("filePath"))
                        .or_else(|| i.get("url"))
                })
                .and_then(|c| c.as_str())
                .unwrap_or("");
            return (
                "permission".into(),
                clip(&format!("Blocked {tool}: {command}")),
            );
        }
        let field = |k: &str| {
            state
                .and_then(|s| s.get(k))
                .and_then(|v| v.as_str())
                .unwrap_or("")
        };
        return (
            "tool".into(),
            clip(format!("{tool} {} {}", field("status"), field("title")).trim()),
        );
    }
    if kind == "text" {
        let text = part
            .and_then(|p| p.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("");
        return ("text".into(), clip(text));
    }
    if kind.contains("permission") {
        return (
            "permission".into(),
            clip(&format!("{kind}: {}", part.unwrap_or(obj))),
        );
    }
    (kind.into(), kind.into())
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

fn supports_standalone(exe: &Path) -> bool {
    crate::retry_busy(|| {
        Command::new(exe)
            .args(["run", "--help"])
            .stdin(Stdio::null())
            .output()
    })
    .map(|o| {
        String::from_utf8_lossy(&o.stdout).contains("--standalone")
            || String::from_utf8_lossy(&o.stderr).contains("--standalone")
    })
    .unwrap_or(false)
}

pub struct RunConfig {
    pub exe: PathBuf,
    pub prompt: String,
    pub workspace: PathBuf,
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

    /// The exact argv. There is deliberately no way to add `--auto`.
    pub fn args(&self) -> Vec<String> {
        let mut a = vec!["run".to_string()];
        if supports_standalone(&self.exe) {
            a.push("--standalone".into()); // private server: isolates this run
        }
        a.extend([
            "--dir".into(),
            self.workspace.to_string_lossy().into_owned(),
            "--format".into(),
            "json".into(),
            "--".into(),
            self.prompt.clone(),
        ]);
        a
    }
}

pub struct Run {
    canceller: Canceller,
    handle: JoinHandle<RunResult>,
}

/// A cloneable handle that can Stop a run from any thread.
#[derive(Clone)]
pub struct Canceller {
    pgid: Arc<AtomicI32>,
    cancel: Arc<AtomicBool>,
}

impl Canceller {
    /// SIGTERM the whole process group; the supervisor escalates to SIGKILL.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        signal_group(self.pgid.load(Ordering::SeqCst), libc::SIGTERM);
    }
}

impl Run {
    pub fn start(cfg: RunConfig, on_event: OnEvent) -> Result<Self, String> {
        let before = change_snapshot(&cfg.workspace);
        let args = cfg.args();
        let mut child = crate::retry_busy(|| {
            Command::new(&cfg.exe)
                .args(&args)
                .current_dir(&cfg.workspace)
                .env("PWD", &cfg.workspace)
                .env("OPENCODE_CONFIG_CONTENT", cfg.policy())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
        })
        .map_err(|e| format!("could not start {}: {e}", cfg.exe.display()))?;
        let pgid = Arc::new(AtomicI32::new(child.id() as i32));
        let cancel = Arc::new(AtomicBool::new(false));
        let (p, c) = (pgid.clone(), cancel.clone());
        let handle = thread::spawn(move || supervise(&mut child, &cfg, before, &c, &p, on_event));
        Ok(Self {
            canceller: Canceller { pgid, cancel },
            handle,
        })
    }

    pub fn canceller(&self) -> Canceller {
        self.canceller.clone()
    }

    /// Stop the run: SIGTERM the whole process group, SIGKILL after a grace period.
    pub fn cancel(&self) {
        self.canceller.cancel();
    }

    pub fn join(self) -> RunResult {
        self.handle.join().unwrap_or(RunResult {
            status: RunStatus::Failed,
            exit_code: None,
            errors: vec!["supervisor thread panicked".into()],
            stderr: vec![],
            changed_files: None,
        })
    }
}

fn signal_group(pgid: i32, sig: i32) {
    if pgid > 0 {
        // SAFETY: plain syscall on a process group we created with process_group(0).
        unsafe { libc::killpg(pgid, sig) };
    }
}

fn terminate(child: &mut Child) {
    let pgid = child.id() as i32;
    signal_group(pgid, libc::SIGTERM);
    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            signal_group(pgid, libc::SIGKILL); // reap any stragglers in the group
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    signal_group(pgid, libc::SIGKILL);
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
    cancel: &AtomicBool,
    pgid: &AtomicI32,
    on_event: OnEvent,
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
    while open > 0 && !cancel.load(Ordering::SeqCst) {
        match rx.recv_timeout(POLL) {
            Err(RecvTimeoutError::Timeout) => {
                if !stalled && last.elapsed() > cfg.stall_after {
                    stalled = true;
                    on_event(
                        "stall",
                        &format!(
                            "No output for {}s: the agent may be waiting on a permission or the provider. Stop to abort.",
                            cfg.stall_after.as_secs()
                        ),
                    );
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
                match serde_json::from_str::<serde_json::Value>(l) {
                    Ok(v) if v.is_object() => {
                        let (kind, text) = summarize_event(&v);
                        on_event(&kind, &text);
                        if kind == "error" {
                            errors.push(text);
                            break;
                        }
                    }
                    _ => on_event("raw", &l.chars().take(MAX_EVENT_CHARS).collect::<String>()),
                }
            }
        }
    }
    let cancelled = cancel.load(Ordering::SeqCst);
    let exit_code = if cancelled || !errors.is_empty() {
        terminate(child);
        child.wait().ok().and_then(|s| s.code())
    } else {
        match wait_with_grace(child) {
            Some(s) => s.code(),
            None => {
                terminate(child);
                child.wait().ok().and_then(|s| s.code())
            }
        }
    };
    pgid.store(0, Ordering::SeqCst);
    let changed_files = diff_snapshots(before.as_ref(), change_snapshot(&cfg.workspace).as_ref());
    let status = if cancelled {
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
        errors,
        stderr: stderr_tail,
        changed_files,
    }
}

fn wait_with_grace(child: &mut Child) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline {
        if let Ok(Some(s)) = child.try_wait() {
            return Some(s);
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    struct Fixture {
        dir: tempfile::TempDir,
        repo: PathBuf,
        events: Arc<Mutex<Vec<(String, String)>>>,
    }

    fn sh(dir: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for a in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
        ] {
            sh(&repo, a);
        }
        std::fs::write(repo.join("a.py"), "x=1\n").unwrap();
        sh(&repo, &["add", "."]);
        sh(&repo, &["commit", "-qm", "i"]);
        Fixture {
            dir,
            repo,
            events: Arc::default(),
        }
    }

    impl Fixture {
        fn fake(&self, body: &str) -> PathBuf {
            let path = self.dir.path().join("fake-opencode");
            let script = format!(
                "#!/bin/sh\nif [ \"$2\" = \"--help\" ]; then echo '--standalone'; exit 0; fi\n{body}\n"
            );
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        fn start(&self, body: &str, stall: Duration) -> Run {
            let ev = self.events.clone();
            let mut cfg = RunConfig::new(self.fake(body), "p", self.repo.clone());
            cfg.stall_after = stall;
            Run::start(
                cfg,
                Arc::new(move |k, t| ev.lock().unwrap().push((k.into(), t.into()))),
            )
            .unwrap()
        }

        fn run(&self, body: &str) -> RunResult {
            self.start(body, STALL_AFTER).join()
        }
    }

    #[test]
    fn success_needs_exit_zero_and_reports_changed_files() {
        let f = fixture();
        let r = f.run("echo 'x=2' > a.py\necho '{\"type\":\"tool_use\",\"part\":{\"tool\":\"edit\",\"state\":{\"status\":\"completed\",\"title\":\"a.py\"}}}'");
        assert_eq!(r.status, RunStatus::Completed);
        assert_eq!(r.changed_files, Some(vec!["a.py".to_string()]));
        assert!(
            f.events
                .lock()
                .unwrap()
                .contains(&("tool".into(), "edit completed a.py".into()))
        );
    }

    #[test]
    fn error_event_fails_the_run_and_ends_a_hanging_process() {
        let f = fixture();
        let t = Instant::now();
        let r = f.run("echo '{\"type\":\"error\",\"error\":{\"message\":\"boom\"}}'\nsleep 60");
        assert!(t.elapsed() < Duration::from_secs(15));
        assert_eq!(
            (r.status, r.errors),
            (RunStatus::Failed, vec!["boom".to_string()])
        );
    }

    #[test]
    fn nonzero_exit_fails() {
        let r = fixture().run("exit 3");
        assert_eq!((r.status, r.exit_code), (RunStatus::Failed, Some(3)));
    }

    #[test]
    fn stop_kills_a_silent_process_and_its_children() {
        let f = fixture();
        let pidfile = f.dir.path().join("child.pid");
        let run = f.start(
            &format!("sleep 60 &\necho $! > {}\nsleep 60", pidfile.display()),
            STALL_AFTER,
        );
        for _ in 0..50 {
            if std::fs::read_to_string(&pidfile).is_ok_and(|s| !s.trim().is_empty()) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        run.cancel();
        assert_eq!(run.join().status, RunStatus::Cancelled);
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        thread::sleep(Duration::from_millis(300));
        // SAFETY: signal 0 only probes for existence.
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "child {pid} survived Stop"
        );
    }

    #[test]
    fn stop_reports_partial_changes() {
        let f = fixture();
        let run = f.start("echo y > b.py\necho '{}'\nsleep 60", STALL_AFTER);
        for _ in 0..100 {
            if f.repo.join("b.py").exists() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        run.cancel();
        assert_eq!(run.join().changed_files, Some(vec!["b.py".to_string()]));
    }

    #[test]
    fn stall_is_surfaced_not_killed() {
        let f = fixture();
        let r = f.start("sleep 2", Duration::from_millis(500)).join();
        assert_eq!(r.status, RunStatus::Completed);
        assert!(f.events.lock().unwrap().iter().any(|(k, _)| k == "stall"));
    }

    #[test]
    fn non_json_lines_are_kept_raw() {
        let f = fixture();
        f.run("echo hello");
        assert!(
            f.events
                .lock()
                .unwrap()
                .contains(&("raw".into(), "hello".into()))
        );
    }

    #[test]
    fn the_child_is_pinned_to_the_workspace() {
        let f = fixture();
        let args = RunConfig::new(f.fake(""), "go", f.repo.clone()).args();
        let at = args.iter().position(|a| a == "--dir").unwrap();
        assert_eq!(args[at + 1], f.repo.to_string_lossy());
        f.run("echo \"pwd=$PWD\"");
        let want = ("raw".to_string(), format!("pwd={}", f.repo.display()));
        assert!(f.events.lock().unwrap().contains(&want));
    }

    #[test]
    fn option_looking_prompts_stay_after_the_separator() {
        let f = fixture();
        let args = RunConfig::new(f.fake(""), "--auto", f.repo.clone()).args();
        let n = args.len();
        assert_eq!(&args[n - 2..], ["--", "--auto"]);
    }

    #[test]
    fn broad_or_secret_workspaces_are_refused() {
        for p in ["/", "/etc", "/usr", "/home"] {
            assert!(validate_workspace(Path::new(p)).is_err(), "{p}");
        }
        let home = dirs::home_dir().unwrap();
        assert!(validate_workspace(&home).is_err());
        assert!(validate_workspace(&home.join(".config")).is_err());
    }

    #[test]
    fn never_passes_auto_and_prompt_is_one_argument() {
        let f = fixture();
        let args = RunConfig::new(f.fake(""), "fix it; rm -rf ~ --auto", f.repo.clone()).args();
        assert!(!args.iter().any(|a| a == "--auto"));
        assert_eq!(args.last().unwrap(), "fix it; rm -rf ~ --auto");
    }

    #[test]
    fn workspace_validation() {
        let f = fixture();
        assert!(validate_workspace(Path::new("/")).is_err());
        assert!(validate_workspace(&dirs::home_dir().unwrap()).is_err());
        assert!(validate_workspace(&f.dir.path().join("missing")).is_err());
        assert!(validate_workspace(&f.repo.join("a.py")).is_err());
        assert_eq!(
            validate_workspace(&f.repo).unwrap(),
            f.repo.canonicalize().unwrap()
        );
    }

    #[test]
    fn non_git_workspace_has_unknown_changes() {
        let f = fixture();
        let plain = f.dir.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        let cfg = RunConfig::new(f.fake(""), "p", plain);
        assert_eq!(
            Run::start(cfg, Arc::new(|_, _| {}))
                .unwrap()
                .join()
                .changed_files,
            None
        );
    }
}

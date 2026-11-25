//! Bounded, owner-approved test commands in X-owned Git worktrees.
//! Test commands can run arbitrary project scripts: approval is required for
//! each run and this API is not exposed to model output.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::{Authority, Check, Evidence, Kind, Result as Verification, Status};
use crate::cancellation::Cancellation;
use crate::grants::{Grants, Permission, Receipt, Scope};
use crate::projects::Project;
use crate::worktrees::{Manager, OwnedWorktree};

const MAX_TEST_TIME: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Suite {
    CargoWorkspace,
    CargoAgent,
    Npm,
    Pnpm,
    Pytest,
    Go,
}

/// Independently observed suite result bound to an exact Git commit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestEvidence {
    pub suite: Suite,
    pub commit: String,
    /// None means the process or checkout could not be verified.
    pub passed: Option<bool>,
    pub observed_at: u64,
}

/// A short local-only tail of each independently captured output stream.
/// This is untrusted test text, not instructions or authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FailureExcerpt {
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

/// A proposed correction of a specific observed test failure, not a grant.
/// This value is not persisted or sent to a provider by the test runner.
#[derive(Clone, Debug, Serialize)]
pub struct CorrectionDraft {
    pub project_id: uuid::Uuid,
    pub run_id: uuid::Uuid,
    pub node_id: String,
    pub commit: String,
    pub suite: Suite,
    pub excerpt: FailureExcerpt,
}

impl CorrectionDraft {
    /// The operator must review again if any checkout identity or input changed.
    /// This check alone never authorizes a correction or retest.
    pub fn validate_source(
        &self,
        manager: &Manager,
        project: &Project,
        worktree: &OwnedWorktree,
    ) -> std::result::Result<(), String> {
        if self.project_id != project.id
            || self.run_id != worktree.run_id()
            || self.node_id != worktree.task_id()
            || self.commit != worktree.start_commit()
        {
            return Err("The reviewed failure belongs to a different checkout".into());
        }
        manager.verify_unchanged(project, worktree)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct TestReport {
    pub verification: Verification,
    pub correction: Option<CorrectionDraft>,
}

impl TestEvidence {
    pub(crate) fn valid(&self) -> bool {
        self.observed_at > 0 && crate::worktrees::valid_commit(&self.commit)
    }
}

impl Suite {
    /// Fixed executables and argv, not arbitrary commands or a shell.
    pub fn command(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::CargoWorkspace => ("cargo", &["test", "--locked", "--workspace"]),
            Self::CargoAgent => (
                "cargo",
                &["test", "--locked", "-p", "x-agent", "--all-targets"],
            ),
            Self::Npm => ("npm", &["test"]),
            Self::Pnpm => ("pnpm", &["test"]),
            Self::Pytest => ("python", &["-m", "pytest", "-q"]),
            Self::Go => ("go", &["test", "./..."]),
        }
    }

    pub fn parse(input: &str) -> std::result::Result<Self, String> {
        match input {
            "cargo-workspace" => Ok(Self::CargoWorkspace),
            "cargo-agent" => Ok(Self::CargoAgent),
            "npm" => Ok(Self::Npm),
            "pnpm" => Ok(Self::Pnpm),
            "pytest" => Ok(Self::Pytest),
            "go" => Ok(Self::Go),
            _ => Err("Suite must be cargo-workspace, cargo-agent, npm, pnpm, pytest or go".into()),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::CargoWorkspace => "cargo_workspace",
            Self::CargoAgent => "cargo_agent",
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
            Self::Pytest => "pytest",
            Self::Go => "go",
        }
    }
}

/// Only a verified local worktree may be tested, and only after explicit
/// current approval. No test is launched when the approval is absent.
pub fn verify_approved(
    manager: &Manager,
    project: &Project,
    worktree: &OwnedWorktree,
    suite: Suite,
    grants: &Grants,
    receipt: &Receipt,
    stop: &Cancellation,
) -> std::result::Result<Verification, String> {
    verify_with(
        manager,
        project,
        worktree,
        suite,
        grants,
        receipt,
        stop,
        &SystemRunner,
    )
}

/// Run the same approved test and return a local correction preview only when
/// a completed process failed at the exact unchanged checkout.
pub fn verify_approved_report(
    manager: &Manager,
    project: &Project,
    worktree: &OwnedWorktree,
    suite: Suite,
    grants: &Grants,
    receipt: &Receipt,
    stop: &Cancellation,
) -> std::result::Result<TestReport, String> {
    verify_report_with(
        manager,
        project,
        worktree,
        suite,
        grants,
        receipt,
        stop,
        &SystemRunner,
    )
}

/// Exact approval scope shared with the native owner-approval path.
/// Changing the project, run, task, checkout, or test preset changes the scope.
pub(crate) fn scope_for_suite(project: &Project, worktree: &OwnedWorktree, suite: Suite) -> Scope {
    Scope::Task {
        project_id: project.id,
        run_id: worktree.run_id(),
        node_id: worktree.task_id().to_owned(),
        intent: format!(
            "test:{}:{}:{}",
            suite.label(),
            worktree.start_commit(),
            worktree.path().display()
        ),
    }
}

struct ProcessResult {
    success: bool,
    excerpt: FailureExcerpt,
}

trait Runner {
    fn run(
        &self,
        workspace: &Path,
        suite: Suite,
        stop: &Cancellation,
    ) -> std::result::Result<ProcessResult, String>;
}

struct SystemRunner;

impl Runner for SystemRunner {
    fn run(
        &self,
        workspace: &Path,
        suite: Suite,
        stop: &Cancellation,
    ) -> std::result::Result<ProcessResult, String> {
        let (program, args) = suite.command();
        let mut command = Command::new(program);
        command
            .current_dir(workspace)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("NODE_OPTIONS")
            .env_remove("PYTHONSTARTUP")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("CARGO_TARGET_DIR");
        capture_test(&mut command, MAX_TEST_TIME, stop)
    }
}

/// The extra argument is an injectable process runner used by the authorization tests.
#[allow(clippy::too_many_arguments)]
fn verify_with(
    manager: &Manager,
    project: &Project,
    worktree: &OwnedWorktree,
    suite: Suite,
    grants: &Grants,
    receipt: &Receipt,
    stop: &Cancellation,
    runner: &dyn Runner,
) -> std::result::Result<Verification, String> {
    verify_report_with(
        manager, project, worktree, suite, grants, receipt, stop, runner,
    )
    .map(|report| report.verification)
}

#[allow(clippy::too_many_arguments)]
fn verify_report_with(
    manager: &Manager,
    project: &Project,
    worktree: &OwnedWorktree,
    suite: Suite,
    grants: &Grants,
    receipt: &Receipt,
    stop: &Cancellation,
    runner: &dyn Runner,
) -> std::result::Result<TestReport, String> {
    if stop.requested() {
        return Err("Stopped before launching project tests".into());
    }
    manager.verify_start(project, worktree)?;
    let workspace = manager.validate_owned(project, worktree)?;
    grants.consume(
        receipt,
        Permission::RunTests,
        &scope_for_suite(project, worktree, suite),
    )?;
    let reported = runner.run(&workspace, suite, stop);
    let status = if stop.requested() || manager.verify_unchanged(project, worktree).is_err() {
        Status::Unknown
    } else {
        match &reported {
            Ok(process) if process.success => Status::Passed,
            Ok(_) => Status::Failed,
            Err(_) => Status::Unknown,
        }
    };
    let verification = evaluate(
        &format!("{}-{}", worktree.run_id(), worktree.task_id()),
        suite,
        status,
        unix_now(),
    )?;
    let correction = match (status, reported) {
        (Status::Failed, Ok(process)) => Some(CorrectionDraft {
            project_id: project.id,
            run_id: worktree.run_id(),
            node_id: worktree.task_id().to_owned(),
            commit: worktree.start_commit().to_owned(),
            suite,
            excerpt: process.excerpt,
        }),
        _ => None,
    };
    Ok(TestReport {
        verification,
        correction,
    })
}

fn evaluate(
    target: &str,
    suite: Suite,
    status: Status,
    observed_at: u64,
) -> std::result::Result<Verification, String> {
    let evidence = if status == Status::Unknown {
        Vec::new()
    } else {
        vec![Evidence {
            kind: Kind::TestProcess,
            authority: Authority::Direct,
            observed_at,
            reference: format!("completed-test:{}", suite.label()),
        }]
    };
    let indices = if evidence.is_empty() { vec![] } else { vec![0] };
    Verification::evaluate(
        target,
        "approved_worktree_tests",
        observed_at,
        evidence,
        vec![Check {
            id: "suite_exit_status".into(),
            required: true,
            status,
            evidence_indices: indices,
        }],
        vec!["Passing a test suite does not establish correctness beyond that suite".into()],
    )
}

// Continuously drain both streams. Memory stays bounded even when a noisy
// test produces more output than the review can display.
const EXCERPT_BYTES: usize = 8_192;

fn read_tail(mut stream: impl Read) -> std::io::Result<(String, bool)> {
    let mut tail = Vec::with_capacity(EXCERPT_BYTES);
    let mut buffer = [0; EXCERPT_BYTES];
    let mut truncated = false;
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let discard = (tail.len() + read).saturating_sub(EXCERPT_BYTES);
        if discard > 0 {
            tail.drain(..discard);
            truncated = true;
        }
        tail.extend_from_slice(&buffer[..read]);
    }
    let text = String::from_utf8_lossy(&tail)
        .chars()
        .filter(|ch| !ch.is_control() || matches!(*ch, '\n' | '\t'))
        .collect();
    Ok((text, truncated))
}

type Tail = std::io::Result<(String, bool)>;

fn drain(stream: impl Read + Send + 'static) -> Receiver<Tail> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(read_tail(stream));
    });
    rx
}

fn receive_tail(
    rx: Receiver<Tail>,
    started: Instant,
    limit: Duration,
    stop: &Cancellation,
) -> std::result::Result<(String, bool), String> {
    loop {
        stop.check()?;
        let remaining = limit.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err("The approved test output did not close in time".into());
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(25))) {
            Ok(result) => return result.map_err(|_| "Could not read test output".into()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err("Test output reader stopped".into()),
        }
    }
}

fn capture_test(
    command: &mut Command,
    limit: Duration,
    stop: &Cancellation,
) -> std::result::Result<ProcessResult, String> {
    stop.check()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child =
        crate::retry_busy(|| command.spawn()).map_err(|_| "Could not start approved tests")?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Could not open test output pipes".into());
    };
    let stdout = drain(stdout);
    let stderr = drain(stderr);
    let started = Instant::now();
    let success = loop {
        if stop.requested() || started.elapsed() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Approved tests stopped or timed out".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Could not observe the approved test process".into());
            }
        }
    };
    let (stdout, stdout_truncated) = receive_tail(stdout, started, limit, stop)?;
    let (stderr, stderr_truncated) = receive_tail(stderr, started, limit, stop)?;
    Ok(ProcessResult {
        success,
        excerpt: FailureExcerpt {
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        },
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::projects::Project;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use uuid::Uuid;

    #[cfg(unix)]
    struct MockRunner {
        result: std::result::Result<bool, String>,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[cfg(unix)]
    impl Runner for MockRunner {
        fn run(
            &self,
            _workspace: &Path,
            _suite: Suite,
            _stop: &Cancellation,
        ) -> std::result::Result<ProcessResult, String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.result.clone().map(|success| ProcessResult {
                success,
                excerpt: FailureExcerpt {
                    stdout: "test failure".into(),
                    stderr: "diagnostic".into(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                },
            })
        }
    }

    #[test]
    fn output_tail_preserves_end_and_bounds_memory() {
        let mut bytes = vec![b'x'; EXCERPT_BYTES * 3];
        bytes.extend_from_slice(b"failure at the end");
        let (tail, truncated) = read_tail(bytes.as_slice()).unwrap();
        assert!(truncated);
        assert_eq!(tail.len(), EXCERPT_BYTES);
        assert!(tail.ends_with("failure at the end"));
    }

    #[cfg(unix)]
    #[test]
    fn observed_failure_proposes_only_the_exact_checkout_and_suite() {
        let (_temp, project, manager, owned) = fixture();
        let (grants, receipt) = approval(&project, &owned, Suite::CargoAgent);
        let runner = MockRunner {
            result: Ok(false),
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let report = verify_report_with(
            &manager,
            &project,
            &owned,
            Suite::CargoAgent,
            &grants,
            &receipt,
            &Cancellation::default(),
            &runner,
        )
        .unwrap();
        let mut correction = report.correction.unwrap();
        assert_eq!(correction.commit, owned.start_commit());
        assert_eq!(correction.suite, Suite::CargoAgent);
        assert_eq!(correction.excerpt.stderr, "diagnostic");
        assert!(
            correction
                .validate_source(&manager, &project, &owned)
                .is_ok()
        );
        correction.commit = "0".repeat(40);
        assert!(
            correction
                .validate_source(&manager, &project, &owned)
                .is_err()
        );
        assert_eq!(runner.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn operator_suite_names_are_strict() {
        assert_eq!(Suite::parse("cargo-agent").unwrap(), Suite::CargoAgent);
        assert_eq!(Suite::parse("go").unwrap(), Suite::Go);
        for unsafe_name in ["", "bash", "npm;rm", "npm run test", "Go", "go\nrm"] {
            assert!(Suite::parse(unsafe_name).is_err());
        }
    }

    #[test]
    fn no_model_supplied_executable_or_shell_is_supported() {
        for suite in [
            Suite::CargoWorkspace,
            Suite::CargoAgent,
            Suite::Npm,
            Suite::Pnpm,
            Suite::Pytest,
            Suite::Go,
        ] {
            let (program, args) = suite.command();
            assert!(!program.contains([' ', ';', '|', '&']));
            assert!(!args.iter().any(|arg| arg.contains([';', '|', '&'])));
        }
    }

    #[test]
    fn test_exit_status_is_not_confused_with_code_correctness() {
        let good = evaluate("task", Suite::CargoAgent, Status::Passed, 10).unwrap();
        assert_eq!(good.outcome, super::super::Outcome::Verified);
        assert!(
            good.limitations
                .iter()
                .any(|item| item.contains("does not establish"))
        );
        assert_eq!(good.evidence[0].kind, Kind::TestProcess);
        let bad = evaluate("task", Suite::Pytest, Status::Failed, 10).unwrap();
        assert_eq!(bad.outcome, super::super::Outcome::Failed);
        let missing = evaluate("task", Suite::Go, Status::Unknown, 10).unwrap();
        assert_eq!(missing.outcome, super::super::Outcome::Unverified);
    }

    #[cfg(unix)]
    fn fixture() -> (tempfile::TempDir, Project, Manager, OwnedWorktree) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir(&home).unwrap();
        let root = home.join("repo");
        fs::create_dir(&root).unwrap();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .current_dir(&root)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.name=X",
            "-c",
            "user.email=x@invalid.example",
            "-c",
            "core.hooksPath=",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "fixture",
        ]);
        let root = root.canonicalize().unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "Fixture".into(),
            aliases: vec![],
            git_common_dir: Some(root.join(".git").canonicalize().unwrap()),
            root,
        };
        let rev = Command::new("git")
            .current_dir(&project.root)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let commit = String::from_utf8(rev.stdout).unwrap().trim().to_string();
        let base = home.join("Worktrees");
        fs::create_dir(&base).unwrap();
        let manager = Manager::new(base, home);
        let owned = manager
            .allocate(&project, Uuid::new_v4(), "verify", &commit, true)
            .unwrap();
        (temp, project, manager, owned)
    }

    #[cfg(unix)]
    fn approval(project: &Project, worktree: &OwnedWorktree, suite: Suite) -> (Grants, Receipt) {
        let grants = Grants::default();
        let receipt = grants
            .issue(
                Permission::RunTests,
                scope_for_suite(project, worktree, suite),
                Duration::from_secs(30),
                1,
                true,
            )
            .unwrap();
        (grants, receipt)
    }

    #[cfg(unix)]
    #[test]
    fn unapproved_suite_cannot_start_or_consume_another_grant() {
        let (_temp, project, manager, owned) = fixture();
        let (grants, receipt) = approval(&project, &owned, Suite::Npm);
        let runner = MockRunner {
            result: Ok(true),
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        assert!(
            verify_with(
                &manager,
                &project,
                &owned,
                Suite::CargoAgent,
                &grants,
                &receipt,
                &Cancellation::default(),
                &runner,
            )
            .is_err()
        );
        assert_eq!(runner.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(
            grants
                .check(
                    &receipt,
                    Permission::RunTests,
                    &scope_for_suite(&project, &owned, Suite::Npm),
                )
                .is_ok()
        );
    }

    #[cfg(unix)]
    #[test]
    fn verified_worktree_tests_consume_exactly_one_approval() {
        let (_temp, project, manager, owned) = fixture();
        let (grants, receipt) = approval(&project, &owned, Suite::CargoAgent);
        let runner = MockRunner {
            result: Ok(false),
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let failed = verify_with(
            &manager,
            &project,
            &owned,
            Suite::CargoAgent,
            &grants,
            &receipt,
            &Cancellation::default(),
            &runner,
        )
        .unwrap();
        assert_eq!(failed.outcome, super::super::Outcome::Failed);
        assert_eq!(runner.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            verify_with(
                &manager,
                &project,
                &owned,
                Suite::CargoAgent,
                &grants,
                &receipt,
                &Cancellation::default(),
                &runner,
            )
            .is_err()
        );
        assert_eq!(runner.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[test]
    fn stale_or_revoked_test_scope_cannot_launch() {
        let (_temp, project, manager, owned) = fixture();
        let (grants, receipt) = approval(&project, &owned, Suite::CargoAgent);
        let runner = MockRunner {
            result: Ok(true),
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let mut other_project = project.clone();
        other_project.id = Uuid::new_v4();
        assert!(
            verify_with(
                &manager,
                &other_project,
                &owned,
                Suite::CargoAgent,
                &grants,
                &receipt,
                &Cancellation::default(),
                &runner,
            )
            .is_err()
        );
        assert_eq!(runner.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(grants.revoke_all().unwrap(), 1);
        assert!(
            verify_with(
                &manager,
                &project,
                &owned,
                Suite::CargoAgent,
                &grants,
                &receipt,
                &Cancellation::default(),
                &runner,
            )
            .is_err()
        );
        assert_eq!(runner.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}

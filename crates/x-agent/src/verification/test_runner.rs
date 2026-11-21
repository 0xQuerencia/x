//! Bounded, owner-approved test commands in X-owned Git worktrees.
//! Test commands can run arbitrary project scripts: approval is required for
//! each run and this API is not exposed to model output.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::{Authority, Check, Evidence, Kind, Result as Verification, Status};
use crate::cancellation::Cancellation;
use crate::projects::Project;
use crate::worktrees::{Manager, OwnedWorktree};

const MAX_TEST_TIME: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Suite {
    CargoWorkspace,
    CargoAgent,
    Npm,
    Pnpm,
    Pytest,
    Go,
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
    approved: bool,
    stop: &Cancellation,
) -> std::result::Result<Verification, String> {
    verify_with(
        manager,
        project,
        worktree,
        suite,
        approved,
        stop,
        &SystemRunner,
    )
}

trait Runner {
    fn run(
        &self,
        workspace: &Path,
        suite: Suite,
        stop: &Cancellation,
    ) -> std::result::Result<bool, String>;
}

struct SystemRunner;

impl Runner for SystemRunner {
    fn run(
        &self,
        workspace: &Path,
        suite: Suite,
        stop: &Cancellation,
    ) -> std::result::Result<bool, String> {
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
            .env_remove("RUSTC_WRAPPER");
        crate::desktop::capture_scoped(&mut command, MAX_TEST_TIME, Some(stop))
            .map(|(success, _output)| success)
            .map_err(|_| "The approved test process did not finish reliably".into())
    }
}

fn verify_with(
    manager: &Manager,
    project: &Project,
    worktree: &OwnedWorktree,
    suite: Suite,
    approved: bool,
    stop: &Cancellation,
    runner: &dyn Runner,
) -> std::result::Result<Verification, String> {
    if !approved {
        return Err("Running project tests requires fresh owner approval".into());
    }
    if stop.requested() {
        return Err("Stopped before launching project tests".into());
    }
    let workspace = manager.validate_owned(project, worktree)?;
    let reported = runner.run(&workspace, suite, stop);
    let status = if stop.requested() {
        Status::Unknown
    } else {
        match reported {
            Ok(true) => Status::Passed,
            Ok(false) => Status::Failed,
            Err(_) => Status::Unknown,
        }
    };
    evaluate(
        &format!("{}-{}", worktree.run_id(), worktree.task_id()),
        suite,
        status,
        unix_now(),
    )
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
        ) -> std::result::Result<bool, String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.result.clone()
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
    #[test]
    fn refuses_unapproved_tests_before_running_any_command() {
        let (_temp, project, manager, owned) = fixture();
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
                false,
                &Cancellation::default(),
                &runner
            )
            .is_err()
        );
        assert_eq!(runner.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[cfg(unix)]
    #[test]
    fn real_worktree_identity_is_checked_before_test_evidence() {
        let (_temp, project, manager, owned) = fixture();
        let runner = MockRunner {
            result: Ok(false),
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let failed = verify_with(
            &manager,
            &project,
            &owned,
            Suite::CargoAgent,
            true,
            &Cancellation::default(),
            &runner,
        )
        .unwrap();
        assert_eq!(failed.outcome, super::super::Outcome::Failed);
        assert_eq!(runner.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let mut other_project = project.clone();
        other_project.id = Uuid::new_v4();
        assert!(
            verify_with(
                &manager,
                &other_project,
                &owned,
                Suite::CargoAgent,
                true,
                &Cancellation::default(),
                &runner
            )
            .is_err()
        );
        assert_eq!(runner.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}

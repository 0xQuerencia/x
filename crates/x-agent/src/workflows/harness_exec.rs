//! An approved, worktree-scoped harness executor for the deterministic DAG.
//! This executor is not user-accessible until the host wires a real approval receipt.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use uuid::Uuid;

use super::dag::{Graph, HarnessKind, Job, Node};
use super::recovery::{Begin, Journal, State};
use super::scheduler::{Executor, Outcome};
use crate::cancellation::Cancellation;
use crate::harness::{Adapter, EventSink, Kind, Request, RunStatus, WorkAccess};
use crate::projects::Project;
use crate::verification::{Outcome as VerifiedOutcome, Result as Verification};
use crate::worktrees::{Manager, OwnedWorktree};

pub enum Proof {
    /// Only the provider delivery and clean process exit are verified.
    DeliveryOnly,
    /// Provider delivery and Git-status evidence for this precise file.
    ChangedFile(String),
}

/// Constructed only after an exact project, task and worktree scope is
/// explicitly authorized by the native host. The model cannot issue it.
pub struct Assignment {
    // The native approval bridge will consume this assignment identity.
    #[allow(dead_code)]
    node_id: String,
    project: Project,
    worktree: OwnedWorktree,
    fingerprint: String,
    adapter: Arc<dyn Adapter>,
    proof: Proof,
}

impl Assignment {
    #[allow(dead_code)]
    pub(crate) fn from_approved_scope(
        node: &Node,
        project: Project,
        worktree: OwnedWorktree,
        fingerprint: String,
        adapter: Arc<dyn Adapter>,
        proof: Proof,
        approved: bool,
    ) -> Result<Self, String> {
        if !approved {
            return Err("The owner has not approved this harness assignment".into());
        }
        let Job::Harness {
            project_id,
            harness,
            ..
        } = &node.job
        else {
            return Err("Only coding harness jobs have worktree assignments".into());
        };
        if project.id.to_string() != *project_id
            || worktree.task_id() != node.id
            || adapter.descriptor().kind != to_kind(*harness)
            || fingerprint.len() != 64
            || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("The approved harness task, project or fingerprint changed".into());
        }
        if let Proof::ChangedFile(path) = &proof
            && (path.is_empty()
                || path.starts_with('/')
                || path.contains("..")
                || path.chars().any(char::is_control))
        {
            return Err("The expected changed file path is unsafe".into());
        }
        Ok(Self {
            node_id: node.id.clone(),
            project,
            worktree,
            fingerprint,
            adapter,
            proof,
        })
    }
}

pub struct ApprovedHarnessExecutor {
    run_id: Uuid,
    assignments: HashMap<String, Assignment>,
    worktrees: Arc<Manager>,
    journal: Arc<Journal>,
}

impl ApprovedHarnessExecutor {
    /// Preparation must come from the native approval path, never from a
    /// model-proposed graph or a JSON `approved: true` property.
    #[allow(dead_code)]
    pub(crate) fn new(
        graph: &Graph,
        run_id: Uuid,
        assignments: Vec<Assignment>,
        worktrees: Arc<Manager>,
        journal: Arc<Journal>,
    ) -> Result<Self, String> {
        if run_id.is_nil() {
            return Err("Invalid workflow run identity".into());
        }
        let mut slots = HashMap::new();
        let mut paths = HashSet::new();
        for assignment in assignments {
            let node = graph
                .nodes
                .iter()
                .find(|node| node.id == assignment.node_id)
                .ok_or("An assignment has no matching graph node")?;
            if assignment.worktree.run_id() != run_id
                || worktrees
                    .validate_owned(&assignment.project, &assignment.worktree)
                    .is_err()
                || !matches!(
                    &node.job,
                    Job::Harness { project_id, harness, .. }
                        if project_id == &assignment.project.id.to_string()
                            && to_kind(*harness) == assignment.adapter.descriptor().kind
                )
                || !paths.insert(assignment.worktree.path().to_path_buf())
                || slots.insert(node.id.clone(), assignment).is_some()
            {
                return Err("An assignment is stale, duplicated or outside its project".into());
            }
        }
        if graph.nodes.iter().any(|node| !slots.contains_key(&node.id)) {
            return Err("Each workflow job requires its own explicit assignment".into());
        }
        Ok(Self {
            run_id,
            assignments: slots,
            worktrees,
            journal,
        })
    }

    fn assignment(&self, node: &Node) -> Option<&Assignment> {
        let assignment = self.assignments.get(&node.id)?;
        let Job::Harness {
            project_id,
            harness,
            ..
        } = &node.job
        else {
            return None;
        };
        (project_id == &assignment.project.id.to_string()
            && to_kind(*harness) == assignment.adapter.descriptor().kind
            && self
                .worktrees
                .validate_owned(&assignment.project, &assignment.worktree)
                .is_ok())
        .then_some(assignment)
    }
}

impl Executor for ApprovedHarnessExecutor {
    fn authorized(&self, node: &Node) -> bool {
        self.assignment(node).is_some()
    }

    fn verified_worktree_key(&self, node: &Node) -> Option<String> {
        self.assignment(node)
            .map(|assignment| assignment.worktree.task_id().to_string())
    }

    fn execute(&self, node: Node, stop: Cancellation) -> Outcome {
        let Some(assignment) = self.assignment(&node) else {
            return Outcome::Failed;
        };
        let Job::Harness { goal, harness, .. } = &node.job else {
            return Outcome::Failed;
        };
        if stop.requested() {
            return Outcome::Cancelled;
        }
        match self
            .journal
            .begin(self.run_id, &node.id, &assignment.fingerprint)
        {
            Ok(Begin::Fresh(_)) => {}
            Ok(Begin::AlreadyFinished(_) | Begin::RequiresReconciliation(_)) | Err(_) => {
                return Outcome::Unverified;
            }
        }
        let sink: EventSink = Arc::new(|_| {});
        let request = Request {
            workspace: assignment.worktree.path().to_path_buf(),
            prompt: goal.clone(),
            session: None,
            access: WorkAccess::Workspace,
            allow_commands: Vec::new(),
        };
        let owned = match assignment.adapter.start(request, sink, &stop) {
            Ok(owned) => owned,
            Err(_) => {
                let _ = self
                    .journal
                    .settle(self.run_id, &node.id, State::Failed, None);
                return Outcome::Failed;
            }
        };
        let result = owned.join();
        let delivery = Verification::harness_delivery(&result, None, unix_now());
        let id_matches = result
            .accepted_session
            .as_ref()
            .is_some_and(|id| id.kind() == to_kind(*harness));
        let outcome = if stop.requested() || result.status == RunStatus::Cancelled {
            Outcome::Cancelled
        } else if !id_matches {
            Outcome::Unverified
        } else {
            match delivery.outcome {
                VerifiedOutcome::Failed => Outcome::Failed,
                VerifiedOutcome::Unverified => Outcome::Unverified,
                VerifiedOutcome::Verified => match &assignment.proof {
                    Proof::DeliveryOnly => Outcome::Verified,
                    Proof::ChangedFile(path) => {
                        match Verification::required_git_change(
                            &node.id,
                            result.changed_files.as_deref(),
                            path,
                            unix_now(),
                        ) {
                            Ok(check) => match check.outcome {
                                VerifiedOutcome::Verified => Outcome::Verified,
                                VerifiedOutcome::Failed => Outcome::Failed,
                                VerifiedOutcome::Unverified => Outcome::Unverified,
                            },
                            Err(_) => Outcome::Unverified,
                        }
                    }
                },
            }
        };
        let persisted = match outcome {
            Outcome::Verified => State::Verified,
            Outcome::Failed => State::Failed,
            Outcome::Cancelled => State::Cancelled,
            _ => State::Unverified,
        };
        let evidence_id = if persisted == State::Verified {
            result.accepted_session.as_ref().map(|id| id.as_str())
        } else {
            None
        };
        if self
            .journal
            .settle(self.run_id, &node.id, persisted, evidence_id)
            .is_err()
        {
            return Outcome::Unverified;
        }
        outcome
    }
}

fn to_kind(kind: HarnessKind) -> Kind {
    match kind {
        HarnessKind::OpenCode => Kind::OpenCode,
        HarnessKind::ClaudeCode => Kind::ClaudeCode,
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::dag::{Graph, VERSION};
    use super::super::model::OnFailure;
    use super::super::scheduler;
    use super::*;
    use crate::harness::{Capability, Descriptor, Event, OwnedRun, RunResult};
    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct FakeAdapter {
        kind: Kind,
        fail: bool,
        calls: Arc<AtomicUsize>,
        running: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    struct FakeRun {
        kind: Kind,
        fail: bool,
        running: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    impl OwnedRun for FakeRun {
        fn join(self: Box<Self>) -> RunResult {
            let success = !self.fail;
            // The journal fsync can serialize the initial claims on slow CI
            // disks. Keep the first fake session alive until both actually
            // start, without allowing a deadlock to hang this test.
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while self.peak.load(Ordering::SeqCst) < 2
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            std::thread::sleep(Duration::from_millis(45));
            self.running.fetch_sub(1, Ordering::SeqCst);
            RunResult {
                status: if success {
                    RunStatus::Completed
                } else {
                    RunStatus::Failed
                },
                exit_code: Some(if success { 0 } else { 1 }),
                accepted_session: Some(
                    crate::harness::SessionId::try_from(match self.kind {
                        Kind::OpenCode => String::from("ses_fixture"),
                        Kind::ClaudeCode => String::from("123e4567-e89b-12d3-a456-426614174000"),
                    })
                    .unwrap(),
                ),
                errors: Vec::new(),
                stderr: Vec::new(),
                changed_files: Some(vec!["src/app.rs".into()]),
            }
        }
    }

    impl Adapter for FakeAdapter {
        fn descriptor(&self) -> Descriptor {
            Descriptor {
                kind: self.kind,
                event_schema: 1,
                version: Some("1.0".into()),
                capabilities: &[Capability::OwnedRun],
            }
        }

        fn start(
            &self,
            _request: Request,
            _events: Arc<dyn Fn(Event) + Send + Sync>,
            _stop: &Cancellation,
        ) -> Result<Box<dyn OwnedRun>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            Ok(Box::new(FakeRun {
                kind: self.kind,
                fail: self.fail,
                running: Arc::clone(&self.running),
                peak: Arc::clone(&self.peak),
            }))
        }
    }

    fn fixture(
        fail_claude: bool,
    ) -> (
        tempfile::TempDir,
        Graph,
        Arc<ApprovedHarnessExecutor>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let root = home.join("repository");
        std::fs::create_dir(&root).unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=X",
                "-c",
                "user.email=x@invalid.test",
                "-c",
                "core.hooksPath=",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "fixture",
            ],
        ] {
            assert!(
                Command::new("git")
                    .current_dir(&root)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let root = root.canonicalize().unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "Chamber".into(),
            aliases: vec![],
            git_common_dir: Some(root.join(".git").canonicalize().unwrap()),
            root,
        };
        let rev = Command::new("git")
            .current_dir(&project.root)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let sha = String::from_utf8(rev.stdout).unwrap().trim().to_string();
        let worktree_base = home.join("Worktrees");
        std::fs::create_dir(&worktree_base).unwrap();
        let manager = Arc::new(Manager::new(worktree_base, home));
        let run_id = Uuid::new_v4();
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let graph = Graph {
            version: VERSION,
            id: "chamber-review".into(),
            name: "Chamber review".into(),
            triggers: vec![],
            nodes: [
                ("front", HarnessKind::OpenCode),
                ("back", HarnessKind::ClaudeCode),
            ]
            .into_iter()
            .map(|(id, harness)| Node {
                id: id.into(),
                after: vec![],
                requires: vec![],
                job: Job::Harness {
                    harness,
                    project_id: project.id.to_string(),
                    goal: format!("Review the {id} files"),
                },
                on_failure: OnFailure::Continue,
            })
            .collect(),
        };
        let assignments = graph
            .nodes
            .iter()
            .map(|node| {
                let worktree = manager
                    .allocate(&project, run_id, &node.id, &sha, true)
                    .unwrap();
                let kind = match &node.job {
                    Job::Harness { harness, .. } => to_kind(*harness),
                    _ => unreachable!(),
                };
                let adapter: Arc<dyn Adapter> = Arc::new(FakeAdapter {
                    kind,
                    fail: fail_claude && kind == Kind::ClaudeCode,
                    calls: Arc::clone(&calls),
                    running: Arc::clone(&running),
                    peak: Arc::clone(&peak),
                });
                Assignment::from_approved_scope(
                    node,
                    project.clone(),
                    worktree,
                    "a".repeat(64),
                    adapter,
                    Proof::DeliveryOnly,
                    true,
                )
                .unwrap()
            })
            .collect();
        let journal = Arc::new(Journal::new(directory.path().join("journal")));
        let runner = Arc::new(
            ApprovedHarnessExecutor::new(&graph, run_id, assignments, manager, journal).unwrap(),
        );
        (directory, graph, runner, peak, calls)
    }

    #[test]
    fn two_provider_adapters_receive_work_in_separate_worktrees() {
        let (_directory, graph, adapter, peak, calls) = fixture(false);
        let result = scheduler::run(
            &graph,
            Path::new("/tmp"),
            adapter.clone(),
            &Cancellation::default(),
            2,
        )
        .unwrap();
        assert_eq!(result.max_concurrent, 2);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(
            result
                .nodes
                .iter()
                .all(|node| node.outcome == Outcome::Verified)
        );
        // A second run with the same journal claims must never resend.
        let replay = scheduler::run(
            &graph,
            Path::new("/tmp"),
            adapter,
            &Cancellation::default(),
            2,
        )
        .unwrap();
        assert!(
            replay
                .nodes
                .iter()
                .all(|node| node.outcome == Outcome::Unverified)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_failed_harness_does_not_verify_its_task() {
        let (_directory, graph, runner, _peak, calls) = fixture(true);
        let result = scheduler::run(
            &graph,
            Path::new("/tmp"),
            runner,
            &Cancellation::default(),
            2,
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(
            result
                .nodes
                .iter()
                .any(|node| node.outcome == Outcome::Failed)
        );
        assert!(
            result
                .nodes
                .iter()
                .any(|node| node.outcome == Outcome::Verified)
        );
    }
}

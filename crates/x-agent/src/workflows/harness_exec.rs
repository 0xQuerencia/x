//! An approved, worktree-scoped harness executor for the deterministic DAG.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::dag::{Graph, HarnessKind, Job, Node};
use super::recovery::{Begin, Journal, State};
use super::scheduler::{Executor, Outcome};
use crate::cancellation::Cancellation;
use crate::grants::{Grants, Permission, Receipt, Scope};
use crate::harness::{Adapter, EventKind, EventSink, Kind, Request, RunStatus, WorkAccess};
use crate::projects::Project;
use crate::verification::{Outcome as VerifiedOutcome, Result as Verification};
use crate::worktrees::{Manager, OwnedWorktree};

#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
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
    grants: Arc<Grants>,
    receipt: Receipt,
}

impl Assignment {
    #[allow(dead_code, clippy::too_many_arguments)]
    pub(crate) fn from_approved_scope(
        node: &Node,
        project: Project,
        worktree: OwnedWorktree,
        adapter: Arc<dyn Adapter>,
        proof: Proof,
        grants: Arc<Grants>,
        receipt: Receipt,
    ) -> Result<Self, String> {
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
        {
            return Err("The approved harness task or project changed".into());
        }
        if let Proof::ChangedFile(path) = &proof
            && !crate::verification::valid_expected_path(path)
        {
            return Err("The expected changed file path is unsafe".into());
        }
        let scope = approval_scope(node, project.id, &worktree, &proof)?;
        grants.check(&receipt, Permission::QueueHarness, &scope)?;
        let Scope::Task { intent, .. } = &scope else {
            return Err("A harness assignment requires exact task scope".into());
        };
        let fingerprint: String = Sha256::digest(intent)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            node_id: node.id.clone(),
            project,
            worktree,
            fingerprint,
            adapter,
            proof,
            grants,
            receipt,
        })
    }
}

type JobActivity = Arc<dyn Fn(&str, u8, EventKind) + Send + Sync>;

pub struct ApprovedHarnessExecutor {
    run_id: Uuid,
    assignments: HashMap<String, Assignment>,
    worktrees: Arc<Manager>,
    journal: Arc<Journal>,
    activity: Option<JobActivity>,
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
            activity: None,
        })
    }

    /// Only structured event kinds leave the worker; provider text, session
    /// IDs and tool arguments are deliberately not forwarded to the UI.
    pub(crate) fn with_activity(mut self, activity: JobActivity) -> Self {
        self.activity = Some(activity);
        self
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
                .is_ok()
            && approval_scope(
                node,
                assignment.project.id,
                &assignment.worktree,
                &assignment.proof,
            )
            .and_then(|scope| {
                assignment
                    .grants
                    .check(&assignment.receipt, Permission::QueueHarness, &scope)
            })
            .is_ok())
        .then_some(assignment)
    }
}

impl Drop for ApprovedHarnessExecutor {
    fn drop(&mut self) {
        // Unlaunched jobs must not keep usable receipts after the executor is closed.
        for assignment in self.assignments.values() {
            let _ = assignment.grants.revoke(&assignment.receipt);
        }
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
        let Ok(scope) = approval_scope(
            &node,
            assignment.project.id,
            &assignment.worktree,
            &assignment.proof,
        ) else {
            return Outcome::Failed;
        };
        if self
            .worktrees
            .verify_start(&assignment.project, &assignment.worktree)
            .is_err()
        {
            return Outcome::Unverified;
        }
        if stop.requested() {
            return Outcome::Cancelled;
        }
        if assignment
            .grants
            .activate_harness(&assignment.receipt, &scope)
            .and_then(|()| {
                assignment
                    .grants
                    .consume(&assignment.receipt, Permission::StartHarness, &scope)
            })
            .is_err()
        {
            return Outcome::Failed;
        }
        let claim = match self
            .journal
            .begin(self.run_id, &node.id, &assignment.fingerprint)
        {
            Ok(Begin::Fresh(claim)) => claim,
            Ok(Begin::AlreadyFinished(_) | Begin::RequiresReconciliation(_)) | Err(_) => {
                return Outcome::Unverified;
            }
        };
        let harness_kind = to_kind(*harness);
        let observed = Arc::new(OnceLock::new());
        let sink: EventSink = {
            let journal = Arc::clone(&self.journal);
            let claim = claim.clone();
            let observed = Arc::clone(&observed);
            let activity = self.activity.clone();
            let node_id = node.id.clone();
            let attempt = claim.attempt;
            Arc::new(move |event| {
                if let Some(activity) = &activity {
                    activity(&node_id, attempt, event.kind);
                }
                if let Some(session) = event
                    .accepted_session()
                    .filter(|session| session.kind() == harness_kind)
                {
                    observed.get_or_init(|| journal.record_delivery(&claim, session));
                }
            })
        };
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
                let (state, outcome) = if stop.requested() {
                    (State::Cancelled, Outcome::Cancelled)
                } else {
                    (State::Failed, Outcome::Failed)
                };
                return match self.journal.settle(&claim, state, None) {
                    Ok(_) => outcome,
                    Err(_) => Outcome::Unverified,
                };
            }
        };
        let result = owned.join();
        let delivery = Verification::harness_delivery(&result, None, unix_now());
        let evidence_id = result
            .accepted_session
            .as_ref()
            .filter(|session| session.kind() == harness_kind)
            .map(|session| session.as_str());
        let outcome = if stop.requested() || result.status == RunStatus::Cancelled {
            Outcome::Cancelled
        } else {
            match delivery.outcome {
                VerifiedOutcome::Failed => Outcome::Failed,
                VerifiedOutcome::Verified if evidence_id.is_some() => match &assignment.proof {
                    Proof::DeliveryOnly => Outcome::Delivered,
                    Proof::ChangedFile(path) => {
                        let changes = self
                            .worktrees
                            .changed_paths(&assignment.project, &assignment.worktree)
                            .ok();
                        match Verification::required_git_change(
                            &node.id,
                            changes.as_deref(),
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
                VerifiedOutcome::Unverified | VerifiedOutcome::Verified => Outcome::Unverified,
            }
        };
        let persisted = match outcome {
            Outcome::Delivered => State::Delivered,
            Outcome::Verified => State::Verified,
            Outcome::Failed => State::Failed,
            Outcome::Cancelled => State::Cancelled,
            Outcome::Unverified | Outcome::Skipped => State::Unverified,
        };
        if (evidence_id.is_none() && observed.get().is_some_and(Result::is_err))
            || self.journal.settle(&claim, persisted, evidence_id).is_err()
        {
            return Outcome::Unverified;
        }
        outcome
    }
}

pub(crate) fn approval_scope(
    node: &Node,
    project_id: Uuid,
    worktree: &OwnedWorktree,
    proof: &Proof,
) -> Result<Scope, String> {
    let intent = serde_json::to_string(&(&node.job, worktree.start_commit(), proof))
        .map_err(|_| "Could not identify the approved task request")?;
    Ok(Scope::Task {
        project_id,
        run_id: worktree.run_id(),
        node_id: node.id.clone(),
        intent,
    })
}

pub(crate) fn to_kind(kind: HarnessKind) -> Kind {
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
            while self.peak.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
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
                accepted_session: success.then(|| {
                    crate::harness::SessionId::try_from(match self.kind {
                        Kind::OpenCode => String::from("ses_fixture"),
                        Kind::ClaudeCode => String::from("123e4567-e89b-12d3-a456-426614174000"),
                    })
                    .unwrap()
                }),
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
            events: EventSink,
            _stop: &Cancellation,
        ) -> Result<Box<dyn OwnedRun>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                let mut event = Event::new(crate::harness::EventKind::Text, "job started");
                event.session = Some(
                    crate::harness::SessionId::try_from(String::from(
                        "123e4567-e89b-12d3-a456-426614174000",
                    ))
                    .unwrap(),
                );
                events(event);
            }
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
                requires_verified: vec![],
                required_change: None,
                job: Job::Harness {
                    harness,
                    project_id: project.id.to_string(),
                    goal: format!("Review the {id} files"),
                },
                on_failure: OnFailure::Continue,
            })
            .collect(),
        };
        let grants = Arc::new(Grants::default());
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
                let receipt = grants
                    .issue(
                        Permission::QueueHarness,
                        approval_scope(node, project.id, &worktree, &Proof::DeliveryOnly).unwrap(),
                        Duration::from_secs(30),
                        1,
                        true,
                    )
                    .unwrap();
                Assignment::from_approved_scope(
                    node,
                    project.clone(),
                    worktree,
                    adapter,
                    Proof::DeliveryOnly,
                    Arc::clone(&grants),
                    receipt,
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
                .all(|node| node.outcome == Outcome::Delivered)
        );
        // Receipts are consumed before delivery; a replay cannot pass preflight.
        assert!(
            scheduler::run(
                &graph,
                Path::new("/tmp"),
                adapter,
                &Cancellation::default(),
                2,
            )
            .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn changing_the_goal_or_verification_profile_invalidates_consent() {
        let (_directory, mut graph, executor, _peak, calls) = fixture(false);
        let assignment = executor.assignments.get("front").unwrap();
        assert!(
            Assignment::from_approved_scope(
                &graph.nodes[0],
                assignment.project.clone(),
                assignment.worktree.clone(),
                Arc::clone(&assignment.adapter),
                Proof::ChangedFile("src/app.rs".into()),
                Arc::clone(&assignment.grants),
                assignment.receipt,
            )
            .is_err()
        );
        if let Job::Harness { goal, .. } = &mut graph.nodes[0].job {
            *goal = "A different task that the user never approved".into();
        }
        assert!(
            scheduler::run(
                &graph,
                Path::new("/tmp"),
                executor,
                &Cancellation::default(),
                2,
            )
            .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_different_worktree_commit_does_not_launch_the_approved_goal() {
        let (_directory, graph, executor, _peak, calls) = fixture(false);
        let task = &executor.assignments["front"];
        assert!(
            Command::new("git")
                .current_dir(task.worktree.path())
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@invalid.test",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=",
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    "unapproved commit"
                ])
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(
            executor.execute(graph.nodes[0].clone(), Cancellation::default()),
            Outcome::Unverified
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn revoked_harness_receipt_cannot_start_any_work() {
        let (_directory, graph, executor, _peak, calls) = fixture(false);
        let assignment = executor.assignments.get("front").unwrap();
        assignment.grants.revoke(&assignment.receipt).unwrap();
        assert!(
            scheduler::run(
                &graph,
                Path::new("/tmp"),
                executor.clone(),
                &Cancellation::default(),
                2,
            )
            .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn dropping_executor_revokes_unspent_harness_approvals() {
        let (_directory, _graph, runner, _peak, calls) = fixture(false);
        let grants = Arc::clone(&runner.assignments.get("front").unwrap().grants);
        assert_eq!(grants.pending_count().unwrap(), 2);
        drop(runner);
        assert_eq!(grants.pending_count().unwrap(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn revoking_a_workflow_run_blocks_all_jobs_before_dispatch() {
        let (_directory, graph, executor, _peak, calls) = fixture(false);
        let assignment = executor.assignments.get("front").unwrap();
        assert_eq!(
            assignment
                .grants
                .revoke_run(assignment.project.id, assignment.worktree.run_id())
                .unwrap(),
            2
        );
        assert!(
            scheduler::run(
                &graph,
                Path::new("/tmp"),
                executor,
                &Cancellation::default(),
                2,
            )
            .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn parallel_activity_is_node_scoped_and_does_not_expose_provider_text() {
        let (_directory, graph, runner, _peak, _calls) = fixture(true);
        let runner = Arc::try_unwrap(runner).ok().unwrap();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let runner = Arc::new(runner.with_activity(Arc::new(move |node, attempt, kind| {
            captured
                .lock()
                .unwrap()
                .push((node.to_owned(), attempt, kind));
        })));
        scheduler::run(
            &graph,
            Path::new("/tmp"),
            runner,
            &Cancellation::default(),
            2,
        )
        .unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            vec![("back".to_owned(), 1, EventKind::Text)]
        );
    }

    #[test]
    fn a_failed_harness_preserves_delivery_without_verifying_the_job() {
        let (_directory, graph, runner, _peak, calls) = fixture(true);
        let result = scheduler::run(
            &graph,
            Path::new("/tmp"),
            runner.clone(),
            &Cancellation::default(),
            2,
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let claims = runner.journal.snapshot().unwrap();
        let failed = claims.iter().find(|claim| claim.node_id == "back").unwrap();
        assert_eq!(failed.state, State::Failed);
        assert!(failed.evidence_id.is_some());
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
                .any(|node| node.outcome == Outcome::Delivered)
        );
    }
}

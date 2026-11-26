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
            termination: None,
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
        roadmap: None,
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
            required_commit: false,

            inputs: vec![],
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
            assignment.worktree.get().unwrap().clone(),
            Arc::clone(assignment.adapter.as_ref().unwrap()),
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
            .current_dir(task.worktree.get().unwrap().path())
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
            .revoke_run(assignment.project.id, assignment.run)
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
    let tree = runner.assignments["back"].worktree.get().unwrap();
    assert_eq!(
        failed.worktree.as_ref().unwrap().resolved_commit(),
        Some(tree.start_commit())
    );
    assert!(runner.journal.record_checkout(failed, tree).is_err());
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

#[test]
fn permission_expiry_during_preparation_retains_checkout_without_dispatch() {
    let (_directory, graph, runner, _, calls) = fixture(false);
    let node = &graph.nodes[0];
    let assignment = &runner.assignments[&node.id];
    let reservation = Reservation::planned(&assignment.project, &assignment.base).unwrap();
    let Begin::Fresh(claim) = runner
        .journal
        .begin_preparation(
            runner.run_id,
            &node.id,
            &assignment.fingerprint,
            reservation,
        )
        .unwrap()
    else {
        panic!("expected a fresh prepared attempt");
    };
    let stop = Cancellation::default();
    let tree = runner.prepare(assignment, &stop).unwrap();
    assignment.grants.expire_for_test(&assignment.receipt);
    runner
        .worktrees
        .verify_start(&assignment.project, &tree)
        .unwrap();
    runner.journal.record_checkout(&claim, &tree).unwrap();
    assert_eq!(
        runner.execute_prepared(assignment, &tree, &claim, &stop),
        Outcome::Failed
    );
    let recorded = runner.journal.exact_claim(runner.run_id, &node.id).unwrap();
    assert_eq!(
        recorded.dispatch,
        Some(crate::workflows::recovery::Dispatch::Pending)
    );
    assert_eq!(recorded.state, State::Failed);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(tree.path().is_dir());
    assert!(
        runner
            .outcome_reason(node)
            .unwrap()
            .contains("approval has expired")
    );
}

#[test]
fn committed_profile_grants_only_the_exact_reviewed_git_commands() {
    let (_directory, mut graph, _executor, ..) = fixture(false);
    let node = &mut graph.nodes[0];
    assert!(artifact_commands(node).is_empty());
    node.required_commit = true;
    let commands = artifact_commands(node);
    assert_eq!(commands.len(), 2);
    assert!(commands[0].ends_with("add --all -- ."));
    assert!(commands[1].ends_with("commit -m 'X job front'"));
    assert!(
        commands
            .iter()
            .all(|command| !command.contains(['*', '?', '\n']))
    );
    let mut config =
        crate::opencode::RunConfig::new("opencode".into(), "Implement", "/tmp/owned".into());
    config.allow_commands = commands.clone();
    let policy: serde_json::Value = serde_json::from_str(&config.policy()).unwrap();
    for command in &commands {
        assert_eq!(policy["permission"]["bash"][command], "allow");
    }
    for command in ["git add *", "git commit*", "git push*", "git reset*"] {
        assert!(policy["permission"]["bash"][command].is_null());
    }
    assert_eq!(policy["permission"]["bash"]["*"], "ask");
}

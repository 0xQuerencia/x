//! Resource-limited DAG coordination. No desktop or harness is invoked by default.
//! Only an executor that explicitly authorizes every node can run a plan.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

mod renewal;
use renewal::Renewal;

use serde::{Deserialize, Serialize};

use super::dag::{Graph, InputProof, Job, Node};
use crate::cancellation::Cancellation;

pub const MAX_PARALLEL: usize = 4;
const STOP_POLL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Desktop request dispatched successfully; its effect was not verified.
    Launched,
    /// Provider acknowledged the task and exited cleanly; no artifact/test proof.
    Delivered,
    Verified,
    /// A fixed approved test suite passed at the recorded immutable commit.
    Tested,
    Failed,
    Unverified,
    Cancelled,
    Skipped,
}

impl Outcome {
    /// Version-2 graphs historically allowed successful delivery to release
    /// dependencies. Stronger evidence edges must opt in explicitly.
    pub fn completed(self) -> bool {
        matches!(
            self,
            Self::Launched | Self::Delivered | Self::Verified | Self::Tested
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NodeUpdate {
    WaitingForPermission,
    Running,
    Finished(Outcome),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeReport {
    pub id: String,
    pub outcome: Outcome,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub nodes: Vec<NodeReport>,
    pub stopped: bool,
    pub max_concurrent: usize,
}

/// A native executor must gate *every* node against explicit
/// approval before the scheduler can run. There is intentionally no default.
pub trait Executor: Send + Sync + 'static {
    fn authorized(&self, node: &Node) -> bool;
    /// Only a trusted executor that independently verified an isolated,
    /// owned Git checkout (or an exact private allocation reservation) may return a worktree resource key. Graph JSON and
    /// model output alone must never grant isolation.
    fn verified_worktree_key(&self, _node: &Node) -> Option<String> {
        None
    }
    /// Release reservations with no active worker while a node awaits approval.
    fn release_idle_projects(&self, _active: &[&Node]) {}
    /// A paused graph can dispatch only after reacquiring its project.
    fn try_acquire_project(&self, _node: &Node) -> bool {
        true
    }
    /// Optional native diagnostic, never model prose or raw process output.
    fn outcome_reason(&self, _node: &Node) -> Option<String> {
        None
    }
    fn execute(&self, node: Node, stop: Cancellation) -> Outcome;
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Resource {
    Desktop,
    Project(String),
    IsolatedWorktree(String, String),
}

impl Resource {
    fn conflicts_with(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Desktop, Self::Desktop) => true,
            (Self::Project(a), Self::Project(b)) => a == b,
            (Self::Project(a), Self::IsolatedWorktree(b, _))
            | (Self::IsolatedWorktree(b, _), Self::Project(a)) => a == b,
            (Self::IsolatedWorktree(a, x), Self::IsolatedWorktree(b, y)) => a == b && x == y,
            _ => false,
        }
    }
}

fn resource(node: &Node, executor: &dyn Executor) -> Resource {
    match &node.job {
        Job::Desktop { .. } => Resource::Desktop,
        Job::Harness { project_id, .. } | Job::Test { project_id, .. } => executor
            .verified_worktree_key(node)
            .filter(|key| {
                !key.is_empty()
                    && key.len() <= 80
                    && key
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
            })
            .map_or_else(
                || Resource::Project(project_id.clone()),
                |key| Resource::IsolatedWorktree(project_id.clone(), key),
            ),
    }
}

/// One cancellation scope per reviewed node, rooted in the graph's Stop token.
/// A request can arrive before dispatch or while its worker is running.
#[derive(Clone)]
pub(crate) struct JobStops {
    root: Cancellation,
    nodes: Vec<Cancellation>,
    renewals: Arc<Mutex<HashMap<usize, Renewal>>>,
}

impl JobStops {
    pub(crate) fn new(count: usize, root: &Cancellation) -> Self {
        Self {
            root: root.clone(),
            nodes: (0..count).map(|_| root.child()).collect(),
            renewals: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn request(&self, index: usize) -> Result<bool, String> {
        self.nodes
            .get(index)
            .map(Cancellation::request)
            .ok_or_else(|| "No matching coding job".into())
    }

    pub(crate) fn requested(&self, index: usize) -> bool {
        self.nodes.get(index).is_some_and(Cancellation::requested)
    }
}

/// Execute a previously user-authorized graph. The runner may only report
/// `Verified` after collecting independent evidence, never from model text.
/// Project-wide writers serialize; verified isolated reservations may overlap.
pub fn run(
    graph: &Graph,
    home: &Path,
    executor: Arc<dyn Executor>,
    stop: &Cancellation,
    parallel: usize,
) -> Result<Report, String> {
    run_observed(graph, home, executor, stop, parallel, |_, _| {})
}

/// Updates are emitted by the scheduler, not the model or worker. The index
/// refers to the reviewed graph's original node order, including skipped jobs.
pub(crate) fn run_observed(
    graph: &Graph,
    home: &Path,
    executor: Arc<dyn Executor>,
    stop: &Cancellation,
    parallel: usize,
    observe: impl FnMut(usize, NodeUpdate),
) -> Result<Report, String> {
    run_observed_scoped(
        graph,
        home,
        executor,
        &JobStops::new(graph.nodes.len(), stop),
        parallel,
        observe,
    )
}

pub(crate) fn run_observed_scoped(
    graph: &Graph,
    home: &Path,
    executor: Arc<dyn Executor>,
    stops: &JobStops,
    parallel: usize,
    mut observe: impl FnMut(usize, NodeUpdate),
) -> Result<Report, String> {
    graph.validate_with_home(home)?;
    if stops.nodes.len() != graph.nodes.len() {
        return Err("Cancellation scopes do not match the reviewed graph".into());
    }
    let stop = &stops.root;
    if !(1..=MAX_PARALLEL).contains(&parallel) {
        return Err(format!("Concurrent workers must be 1-{MAX_PARALLEL}"));
    }
    // A permission failure blocks the *entire* graph before its first side effect.
    if graph.nodes.iter().any(|node| !executor.authorized(node)) {
        return Err("A workflow node needs explicit approval".into());
    }

    let ids: HashMap<&str, usize> = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.as_str(), index))
        .collect();
    let count = graph.nodes.len();
    let mut finished: Vec<Option<Outcome>> = vec![None; count];
    let mut active: HashMap<usize, thread::JoinHandle<()>> = HashMap::new();
    let mut resources: HashSet<Resource> = HashSet::new();
    let mut resource_leases: HashMap<usize, Resource> = HashMap::new();
    let mut notes = vec![String::new(); count];
    let (send, receive) = mpsc::channel::<(usize, Outcome, bool)>();
    let mut stop_new = false;
    let mut max_concurrent = 0usize;

    loop {
        let mut progressed = false;
        let mut waiting_for_permission = false;
        let mut waiting_for_project = false;
        if stop.requested() {
            stop_new = true;
        }
        if !stop_new {
            for (index, node) in graph.nodes.iter().enumerate() {
                if stop_new || stop.requested() {
                    stop_new = true;
                    break;
                }
                if finished[index].is_some() || active.contains_key(&index) {
                    continue;
                }
                if stops.requested(index) {
                    stops.close_permission_review(index);
                    finished[index] = Some(Outcome::Cancelled);
                    notes[index] = "Individual Stop prevented dispatch".into();
                    observe(index, NodeUpdate::Finished(Outcome::Cancelled));
                    progressed = true;
                    continue;
                }
                let all_finished = node
                    .after
                    .iter()
                    .chain(&node.requires)
                    .chain(&node.requires_verified)
                    .chain(node.inputs.iter().map(|input| &input.node))
                    .all(|dependency| finished[ids[dependency.as_str()]].is_some());
                if !all_finished {
                    continue;
                }
                let required_ok = node.requires.iter().all(|dependency| {
                    finished[ids[dependency.as_str()]].is_some_and(Outcome::completed)
                });
                let proof_ok = node.requires_verified.iter().all(|dependency| {
                    matches!(
                        finished[ids[dependency.as_str()]],
                        Some(Outcome::Verified | Outcome::Tested)
                    )
                });
                let input_ok = node.inputs.iter().all(|input| {
                    matches!(
                        (input.proof, finished[ids[input.node.as_str()]]),
                        (
                            InputProof::Committed,
                            Some(Outcome::Verified | Outcome::Tested)
                        ) | (InputProof::Tested, Some(Outcome::Tested))
                    )
                });
                if !required_ok || !proof_ok || !input_ok {
                    finished[index] = Some(Outcome::Skipped);
                    notes[index] = if !input_ok {
                        "Required artifact or test proof is missing".into()
                    } else if !proof_ok {
                        "Required predecessor has no additional verification proof".into()
                    } else {
                        "Required predecessor did not complete".into()
                    };
                    observe(index, NodeUpdate::Finished(Outcome::Skipped));
                    progressed = true;
                    continue;
                }
                if active.len() >= parallel {
                    continue;
                }
                let owned_resource = resource(node, executor.as_ref());
                if resources
                    .iter()
                    .any(|existing| existing.conflicts_with(&owned_resource))
                {
                    continue;
                }
                // Only an exact expired, unspent queued approval can wait for
                // renewal. Revocation, changed identity and exhausted approval
                // remain terminal refusals. No worker or resource is acquired.
                let authorized = executor.authorized(node);
                if !authorized && let Some(first) = stops.wait_for_permission(index) {
                    if first {
                        observe(index, NodeUpdate::WaitingForPermission);
                    }
                    waiting_for_permission = true;
                    continue;
                }
                // Review can arrive between the two checks. Do not mistake a
                // successful renewal for revocation, or skip the dispatch gate.
                if !authorized && !executor.authorized(node) {
                    finished[index] = Some(Outcome::Failed);
                    notes[index] =
                        "Approval expired, was revoked, or no longer matches the job".into();
                    observe(index, NodeUpdate::Finished(Outcome::Failed));
                    if node.on_failure == super::model::OnFailure::Stop {
                        stop_new = true;
                    }
                    progressed = true;
                    continue;
                }
                if !executor.try_acquire_project(node) {
                    waiting_for_project = true;
                    continue;
                }
                stops.close_permission_review(index);
                let runner = Arc::clone(&executor);
                let tx = send.clone();
                let task = node.clone();
                let cancellation = stops.nodes[index].clone();
                progressed = true;
                match thread::Builder::new().spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if cancellation.requested() {
                            Outcome::Cancelled
                        } else {
                            runner.execute(task, cancellation)
                        }
                    }));
                    let panicked = result.is_err();
                    let _ = tx.send((index, result.unwrap_or(Outcome::Failed), panicked));
                }) {
                    Ok(worker) => {
                        resources.insert(owned_resource.clone());
                        resource_leases.insert(index, owned_resource);
                        active.insert(index, worker);
                        max_concurrent = max_concurrent.max(active.len());
                        observe(index, NodeUpdate::Running);
                    }
                    Err(error) => {
                        finished[index] = Some(Outcome::Failed);
                        notes[index] = format!("Could not start worker: {}", error.kind());
                        observe(index, NodeUpdate::Finished(Outcome::Failed));
                        if node.on_failure == super::model::OnFailure::Stop {
                            stop_new = true;
                        }
                    }
                }
            }
        }

        if stop_new {
            stops.close_permission_reviews();
        }
        if waiting_for_permission && !stop_new {
            let running = active
                .keys()
                .map(|index| &graph.nodes[*index])
                .collect::<Vec<_>>();
            executor.release_idle_projects(&running);
        }
        if active.is_empty() {
            if stop_new || finished.iter().all(Option::is_some) {
                break;
            }
            if !progressed {
                if waiting_for_permission || waiting_for_project {
                    thread::sleep(STOP_POLL);
                } else {
                    break;
                }
            }
            continue;
        }
        let (index, mut outcome, panicked) = match receive.recv_timeout(STOP_POLL) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                stop.request();
                for (_, worker) in active.drain() {
                    let _ = worker.join();
                }
                return Err("The scheduler lost an owned worker".into());
            }
        };
        // Release the resource only after the worker is reaped, not merely
        // after its result arrives. Never expose the panic payload as evidence.
        let joined = active
            .remove(&index)
            .is_some_and(|worker| worker.join().is_ok());
        if !panicked
            && joined
            && let Some(reason) = executor.outcome_reason(&graph.nodes[index])
        {
            notes[index] = reason
                .chars()
                .filter(|ch| !ch.is_control())
                .take(240)
                .collect();
        }
        if panicked || !joined {
            outcome = Outcome::Failed;
            notes[index] = "Worker panicked or did not exit cleanly".into();
        }
        if let Some(lease) = resource_leases.remove(&index) {
            resources.remove(&lease);
        }
        finished[index] = Some(outcome);
        observe(index, NodeUpdate::Finished(outcome));
        if matches!(outcome, Outcome::Failed | Outcome::Unverified)
            && graph.nodes[index].on_failure == super::model::OnFailure::Stop
        {
            stop_new = true;
        }
        if outcome == Outcome::Cancelled {
            if stop.requested() || !stops.requested(index) {
                stop_new = true;
            } else {
                notes[index] = "Individual Stop reached the owned worker".into();
            }
        }
    }

    stops.close_permission_reviews();
    let interrupted = stop.requested();
    for (index, outcome) in finished.iter().enumerate() {
        if outcome.is_none() {
            observe(
                index,
                NodeUpdate::Finished(if stops.requested(index) {
                    Outcome::Cancelled
                } else {
                    Outcome::Skipped
                }),
            );
        }
    }
    let nodes = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| NodeReport {
            id: node.id.clone(),
            outcome: finished[index].unwrap_or(if stops.requested(index) {
                Outcome::Cancelled
            } else {
                Outcome::Skipped
            }),
            reason: if !notes[index].is_empty() {
                notes[index].clone()
            } else if finished[index].is_none() && interrupted {
                "Global Stop prevented dispatch".into()
            } else if finished[index].is_none() && stops.requested(index) {
                "Individual Stop prevented dispatch".into()
            } else if finished[index].is_none() {
                "Earlier task stopped the workflow".into()
            } else {
                String::new()
            },
        })
        .collect();
    Ok(Report {
        nodes,
        stopped: interrupted,
        max_concurrent,
    })
}

#[cfg(test)]
mod tests {
    use super::super::dag::{HarnessKind, Node};
    use super::super::model::OnFailure;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct Mock {
        authorized: bool,
        isolated: bool,
        running: AtomicUsize,
        peak: AtomicUsize,
        calls: AtomicUsize,
        outcome: Outcome,
    }

    impl Mock {
        fn new(outcome: Outcome) -> Self {
            Self {
                authorized: true,
                isolated: false,
                running: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                calls: AtomicUsize::new(0),
                outcome,
            }
        }
    }

    impl Executor for Mock {
        fn authorized(&self, _node: &Node) -> bool {
            self.authorized
        }

        fn verified_worktree_key(&self, node: &Node) -> Option<String> {
            self.isolated.then(|| node.id.clone())
        }

        fn execute(&self, _node: Node, cancel: Cancellation) -> Outcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let current = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(current, Ordering::SeqCst);
            for _ in 0..20 {
                if cancel.requested() {
                    self.running.fetch_sub(1, Ordering::SeqCst);
                    return Outcome::Cancelled;
                }
                thread::sleep(Duration::from_millis(2));
            }
            self.running.fetch_sub(1, Ordering::SeqCst);
            self.outcome
        }
    }

    fn job(id: &str, project_id: &str, requires: &[&str]) -> Node {
        Node {
            id: id.into(),
            after: vec![],
            requires: requires.iter().map(|text| (*text).into()).collect(),
            requires_verified: vec![],
            required_change: None,
            required_commit: false,

            inputs: vec![],
            job: Job::Harness {
                harness: HarnessKind::OpenCode,
                project_id: project_id.into(),
                goal: "review tests".into(),
            },
            on_failure: OnFailure::Continue,
        }
    }

    fn graph(first: &str, second: &str) -> Graph {
        Graph {
            version: super::super::dag::VERSION,
            id: "parallel-fixture".into(),
            name: "Parallel fixture".into(),
            triggers: vec![],
            roadmap: None,
            nodes: vec![job("front", first, &[]), job("back", second, &[])],
        }
    }

    #[test]
    fn expired_ready_node_waits_for_exact_review_without_replaying_predecessor() {
        queued_renewal_fixture(false);
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "runs a real 61-second provider predecessor"]
    fn delayed_provider_predecessor_leaves_only_exact_queued_renewal() {
        queued_renewal_fixture(true);
    }

    fn queued_renewal_fixture(real_delay: bool) {
        use crate::grants::{Grants, Permission, Receipt, Scope};
        use uuid::Uuid;

        struct QueueRunner {
            grants: Arc<Grants>,
            receipt: Receipt,
            scope: Scope,
            ran: Mutex<Vec<String>>,
            real_delay: bool,
        }

        impl Executor for QueueRunner {
            fn authorized(&self, node: &Node) -> bool {
                node.id != "back"
                    || self
                        .grants
                        .check(&self.receipt, Permission::QueueHarness, &self.scope)
                        .is_ok()
            }

            fn execute(&self, node: Node, stop: Cancellation) -> Outcome {
                self.ran.lock().unwrap().push(node.id.clone());
                if node.id == "front" {
                    if self.real_delay {
                        let mut fixture = std::process::Command::new("sh");
                        fixture.args(["-c", "sleep 61 && printf fixture-provider-completed"]);
                        let (success, output) = crate::desktop::capture_scoped(
                            &mut fixture,
                            Duration::from_secs(75),
                            Some(&stop),
                        )
                        .unwrap();
                        assert!(success);
                        assert_eq!(output, b"fixture-provider-completed");
                    } else {
                        self.grants.expire_for_test(&self.receipt);
                    }
                }
                Outcome::Verified
            }
        }

        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut graph = graph(&first.to_string(), &second.to_string());
        graph.nodes[1].requires.push("front".into());
        let stop = Cancellation::default();
        let stops = JobStops::new(graph.nodes.len(), &stop);
        let grants = Arc::new(Grants::default());
        let scope = Scope::Task {
            project_id: second,
            run_id: Uuid::new_v4(),
            node_id: "back".into(),
            intent: "exact reviewed recipe".into(),
        };
        let receipt = grants
            .issue(
                Permission::QueueHarness,
                scope.clone(),
                Duration::from_secs(60),
                1,
                true,
            )
            .unwrap();
        stops
            .register_renewal(
                1,
                Arc::clone(&grants),
                receipt,
                Permission::QueueHarness,
                scope.clone(),
            )
            .unwrap();
        let runner = Arc::new(QueueRunner {
            grants,
            receipt,
            scope,
            ran: Mutex::new(vec![]),
            real_delay,
        });
        let mut waited = 0;
        let report = run_observed_scoped(
            &graph,
            Path::new("/tmp"),
            runner.clone(),
            &stops,
            2,
            |index, update| {
                if update == NodeUpdate::WaitingForPermission {
                    assert_eq!(index, 1);
                    assert_eq!(*runner.ran.lock().unwrap(), ["front"]);
                    assert!(!runner.authorized(&graph.nodes[index]));
                    assert_eq!(runner.grants.pending_count().unwrap(), 0);
                    let token = stops.permission_review_token(index).unwrap();
                    assert!(stops.renew_permission(index, Uuid::new_v4()).is_err());
                    stops.renew_permission(index, token).unwrap();
                    assert!(stops.renew_permission(index, token).is_err());
                    waited += 1;
                }
            },
        )
        .unwrap();
        assert_eq!(waited, 1);
        assert_eq!(*runner.ran.lock().unwrap(), ["front", "back"]);
        assert!(
            report
                .nodes
                .iter()
                .all(|node| node.outcome == Outcome::Verified)
        );
    }

    #[test]
    fn independent_projects_can_run_concurrently() {
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let graph = graph(&a, &b);
        let mock = Arc::new(Mock::new(Outcome::Verified));
        let report = run(
            &graph,
            Path::new("/tmp"),
            mock.clone(),
            &Cancellation::default(),
            2,
        )
        .unwrap();
        assert_eq!(report.max_concurrent, 2);
        assert_eq!(mock.peak.load(Ordering::SeqCst), 2);
        assert!(
            report
                .nodes
                .iter()
                .all(|node| node.outcome == Outcome::Verified)
        );
    }

    #[test]
    fn writing_jobs_cannot_share_the_same_project_concurrently() {
        let id = uuid::Uuid::new_v4().to_string();
        let graph = graph(&id, &id);
        let mock = Arc::new(Mock::new(Outcome::Verified));
        let report = run(
            &graph,
            Path::new("/tmp"),
            mock.clone(),
            &Cancellation::default(),
            2,
        )
        .unwrap();
        assert_eq!(report.max_concurrent, 1);
        assert_eq!(mock.peak.load(Ordering::SeqCst), 1);
        assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn verified_distinct_worktrees_can_run_in_parallel_within_one_project() {
        let project = uuid::Uuid::new_v4().to_string();
        let graph = graph(&project, &project);
        let mut mock = Mock::new(Outcome::Verified);
        mock.isolated = true;
        let mock = Arc::new(mock);
        let report = run(
            &graph,
            Path::new("/tmp"),
            mock.clone(),
            &Cancellation::default(),
            2,
        )
        .unwrap();
        assert_eq!(report.max_concurrent, 2);
        assert_eq!(mock.peak.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn proof_edge_does_not_accept_delivery_only() {
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let mut graph = graph(&a, &b);
        graph.version = super::super::dag::PROOF_VERSION;
        graph.nodes[1].requires_verified = vec!["front".into()];
        let report = run(
            &graph,
            Path::new("/tmp"),
            Arc::new(Mock::new(Outcome::Delivered)),
            &Cancellation::default(),
            2,
        )
        .unwrap();
        assert_eq!(report.nodes[0].outcome, Outcome::Delivered);
        assert_eq!(report.nodes[1].outcome, Outcome::Skipped);
        assert!(report.nodes[1].reason.contains("verification proof"));
    }

    #[test]
    fn version_two_dependencies_preserve_delivery_ordering() {
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let mut graph = graph(&a, &b);
        graph.nodes[1].requires = vec!["front".into()];
        let report = run(
            &graph,
            Path::new("/tmp"),
            Arc::new(Mock::new(Outcome::Delivered)),
            &Cancellation::default(),
            2,
        )
        .unwrap();
        assert_eq!(report.nodes[0].outcome, Outcome::Delivered);
        assert_eq!(report.nodes[1].outcome, Outcome::Delivered);
    }

    #[test]
    fn a_failed_required_dependency_never_releases_its_child() {
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let mut graph = graph(&a, &b);
        graph.nodes[1].requires = vec!["front".into()];
        let mock = Arc::new(Mock::new(Outcome::Unverified));
        let report = run(
            &graph,
            Path::new("/tmp"),
            mock.clone(),
            &Cancellation::default(),
            2,
        )
        .unwrap();
        assert_eq!(report.nodes[0].outcome, Outcome::Unverified);
        assert_eq!(report.nodes[1].outcome, Outcome::Skipped);
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn live_updates_report_dependency_outcomes_before_dispatching_children() {
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let mut graph = graph(&a, &b);
        graph.nodes[1].requires = vec!["front".into()];
        let mut updates = Vec::new();
        let report = run_observed(
            &graph,
            Path::new("/tmp"),
            Arc::new(Mock::new(Outcome::Unverified)),
            &Cancellation::default(),
            2,
            |index, update| updates.push((index, update)),
        )
        .unwrap();
        assert_eq!(report.nodes[1].outcome, Outcome::Skipped);
        assert_eq!(
            updates,
            vec![
                (0, NodeUpdate::Running),
                (0, NodeUpdate::Finished(Outcome::Unverified)),
                (1, NodeUpdate::Finished(Outcome::Skipped)),
            ]
        );
    }

    #[test]
    fn live_updates_for_successful_dependencies_are_ordered() {
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let mut graph = graph(&a, &b);
        graph.nodes[1].requires = vec!["front".into()];
        let mut updates = Vec::new();
        run_observed(
            &graph,
            Path::new("/tmp"),
            Arc::new(Mock::new(Outcome::Verified)),
            &Cancellation::default(),
            2,
            |index, update| updates.push((index, update)),
        )
        .unwrap();
        assert_eq!(
            updates,
            vec![
                (0, NodeUpdate::Running),
                (0, NodeUpdate::Finished(Outcome::Verified)),
                (1, NodeUpdate::Running),
                (1, NodeUpdate::Finished(Outcome::Verified)),
            ]
        );
    }

    #[test]
    fn no_work_starts_without_preflight_approval() {
        struct Deny;
        impl Executor for Deny {
            fn authorized(&self, _node: &Node) -> bool {
                false
            }
            fn execute(&self, _node: Node, _stop: Cancellation) -> Outcome {
                panic!("unapproved work was executed");
            }
        }
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let result = run(
            &graph(&a, &b),
            Path::new("/tmp"),
            Arc::new(Deny),
            &Cancellation::default(),
            2,
        );
        assert!(result.is_err());
    }

    #[test]
    fn a_panicked_worker_is_reaped_and_does_not_unlock_required_jobs() {
        struct PanicWorker;
        impl Executor for PanicWorker {
            fn authorized(&self, _node: &Node) -> bool {
                true
            }
            fn execute(&self, node: Node, _stop: Cancellation) -> Outcome {
                assert_eq!(node.id, "front");
                panic!("private worker panic fixture");
            }
        }
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let mut graph = graph(&a, &b);
        graph.nodes[1].requires = vec!["front".into()];
        let report = run(
            &graph,
            Path::new("/tmp"),
            Arc::new(PanicWorker),
            &Cancellation::default(),
            2,
        )
        .unwrap();
        assert_eq!(report.nodes[0].outcome, Outcome::Failed);
        assert_eq!(report.nodes[1].outcome, Outcome::Skipped);
        assert_eq!(
            report.nodes[0].reason,
            "Worker panicked or did not exit cleanly"
        );
    }

    #[test]
    fn individual_stop_does_not_cancel_independent_jobs_or_release_required_edges() {
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        for queued in [true, false] {
            let mut graph = graph(&a, &b);
            graph.nodes.push(job("needs-front", &a, &["front"]));
            let mut ordering = job("after-front", &a, &[]);
            ordering.after = vec!["front".into()];
            graph.nodes.push(ordering);
            let root = Cancellation::default();
            let stops = JobStops::new(graph.nodes.len(), &root);
            if queued {
                assert!(stops.request(0).unwrap());
            }
            let report = run_observed_scoped(
                &graph,
                Path::new("/tmp"),
                Arc::new(Mock::new(Outcome::Verified)),
                &stops,
                2,
                |index, update| {
                    if !queued && index == 0 && update == NodeUpdate::Running {
                        assert!(stops.request(0).unwrap());
                    }
                },
            )
            .unwrap();
            assert!(!report.stopped);
            assert_eq!(report.nodes[0].outcome, Outcome::Cancelled);
            assert_eq!(report.nodes[1].outcome, Outcome::Verified);
            assert_eq!(report.nodes[2].outcome, Outcome::Skipped);
            assert_eq!(report.nodes[3].outcome, Outcome::Verified);
        }
    }

    #[test]
    fn global_stop_reaches_running_workers_and_skips_unstarted_work() {
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let graph = graph(&a, &b);
        let root = Cancellation::default();
        let cancellation = root.clone();
        let mock = Arc::new(Mock::new(Outcome::Verified));
        let trigger = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            cancellation.request();
        });
        let report = run(&graph, Path::new("/tmp"), mock, &root, 1).unwrap();
        trigger.join().unwrap();
        assert!(report.stopped);
        assert_eq!(report.nodes[0].outcome, Outcome::Cancelled);
        assert_eq!(report.nodes[1].outcome, Outcome::Cancelled);
    }
}

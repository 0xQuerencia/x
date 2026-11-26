//! Approved coding and test jobs in isolated, lazily allocated worktrees.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::dag::{Graph, HarnessKind, InputProof, Job, Node};
use super::recovery::{Begin, Claim, Journal, State};
use super::scheduler::{Executor, Outcome};
use crate::cancellation::Cancellation;
use crate::grants::{Grants, Permission, Receipt, Scope};
use crate::harness::{Adapter, EventKind, EventSink, Kind, Request, RunStatus, WorkAccess};
use crate::projects::Project;
use crate::verification::test_runner::{self, Suite, TestEvidence};
use crate::verification::{Outcome as VerifiedOutcome, Result as Verification};
use crate::worktrees::{CommittedArtifact, Manager, OwnedWorktree, Reservation};

#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Proof {
    DeliveryOnly,
    ChangedFile(String),
    CommittedArtifact,
}

/// A native, immutable review authorization. The graph cannot construct this.
/// An empty worktree slot is an exact run/node allocation reservation, not a
/// user-selected path or an unverified promise of provider isolation.
pub struct Assignment {
    node: Node,
    project: Project,
    run: Uuid,
    base: String,
    worktree: OnceLock<OwnedWorktree>,
    adapter: Option<Arc<dyn Adapter>>,
    proof: Proof,
    grants: Arc<Grants>,
    receipt: Receipt,
    scope: Scope,
    fingerprint: String,
}

impl Assignment {
    #[cfg(all(test, unix))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_approved_scope(
        node: &Node,
        project: Project,
        worktree: OwnedWorktree,
        adapter: Arc<dyn Adapter>,
        proof: Proof,
        grants: Arc<Grants>,
        receipt: Receipt,
    ) -> Result<Self, String> {
        if !node.inputs.is_empty() || worktree.task_id() != node.id {
            return Err("Artifact jobs require a native lazy allocation reservation".into());
        }
        let result = Self::from_planned_scope(
            node,
            project,
            worktree.run_id(),
            worktree.start_commit().to_owned(),
            Some(adapter),
            proof,
            grants,
            receipt,
        )?;
        result
            .worktree
            .set(worktree)
            .map_err(|_| "Worktree already assigned")?;
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_planned_scope(
        node: &Node,
        project: Project,
        run: Uuid,
        base: String,
        adapter: Option<Arc<dyn Adapter>>,
        proof: Proof,
        grants: Arc<Grants>,
        receipt: Receipt,
    ) -> Result<Self, String> {
        if run.is_nil()
            || !crate::worktrees::valid_commit(&base)
            || node.job.project_id() != Some(project.id.to_string().as_str())
        {
            return Err("The approved job or project changed".into());
        }
        match (&node.job, &adapter) {
            (Job::Harness { harness, .. }, Some(adapter))
                if adapter.descriptor().kind == to_kind(*harness) => {}
            (Job::Test { .. }, None) => {}
            _ => return Err("The adapter does not support the reviewed job role".into()),
        }
        if (matches!(&proof, Proof::ChangedFile(_) | Proof::CommittedArtifact)
            || !node.inputs.is_empty())
            && matches!(node.job, Job::Harness { .. })
            && !adapter
                .as_ref()
                .is_some_and(|adapter| adapter.descriptor().supports_workspace_write())
        {
            return Err("The harness cannot satisfy a required file change".into());
        }
        if let Proof::ChangedFile(path) = &proof
            && !crate::verification::valid_expected_path(path)
        {
            return Err("The expected changed file path is unsafe".into());
        }
        if matches!(&proof, Proof::CommittedArtifact)
            && !matches!(
                node.job,
                Job::Harness {
                    harness: HarnessKind::OpenCode,
                    ..
                }
            )
        {
            return Err("This harness cannot produce a committed artifact".into());
        }
        let scope = planned_scope(node, project.id, run, &base, &proof)?;
        grants.check(&receipt, queue_permission(node), &scope)?;
        let Scope::Task { intent, .. } = &scope else {
            unreachable!()
        };
        let fingerprint = Sha256::digest(intent)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            node: node.clone(),
            project,
            run,
            base,
            worktree: OnceLock::new(),
            adapter,
            proof,
            grants,
            receipt,
            scope,
            fingerprint,
        })
    }
}

type JobActivity = Arc<dyn Fn(&str, u8, EventKind) + Send + Sync>;
type FailureActivity = Arc<dyn Fn(test_runner::CorrectionDraft) + Send + Sync>;

#[derive(Clone)]
struct Produced {
    artifact: CommittedArtifact,
    tested: bool,
}

pub struct ApprovedHarnessExecutor {
    run_id: Uuid,
    assignments: HashMap<String, Assignment>,
    worktrees: Arc<Manager>,
    journal: Arc<Journal>,
    activity: Option<JobActivity>,
    failure: Option<FailureActivity>,
    /// Only results produced by this live executor can release artifact inputs.
    /// Reopening a journal never restores authority or trusts supplied JSON.
    outputs: Mutex<HashMap<String, Produced>>,
    preparation: Mutex<()>,
    reasons: Mutex<HashMap<String, String>>,
}

impl ApprovedHarnessExecutor {
    pub(crate) fn new(
        graph: &Graph,
        run_id: Uuid,
        assignments: Vec<Assignment>,
        worktrees: Arc<Manager>,
        journal: Arc<Journal>,
    ) -> Result<Self, String> {
        graph.validate()?;
        if run_id.is_nil() {
            return Err("Invalid workflow run identity".into());
        }
        let mut slots = HashMap::new();
        let mut paths = HashSet::new();
        for assignment in assignments {
            let node = graph
                .nodes
                .iter()
                .find(|node| node.id == assignment.node.id)
                .ok_or("An assignment has no matching graph node")?;
            if assignment.run != run_id || assignment.node != *node {
                return Err("An assignment changed its reviewed graph node".into());
            }
            worktrees.validate_base(&assignment.project, &assignment.base)?;
            if let Some(tree) = assignment.worktree.get()
                && (worktrees.validate_owned(&assignment.project, tree).is_err()
                    || !paths.insert(tree.path().to_path_buf()))
            {
                return Err("An assignment has a stale or duplicated checkout".into());
            }
            if slots.insert(node.id.clone(), assignment).is_some() {
                return Err("A graph has duplicate job assignments".into());
            }
        }
        if graph
            .nodes
            .iter()
            .any(|node| !matches!(node.job, Job::Desktop { .. }) && !slots.contains_key(&node.id))
        {
            return Err("Each workflow job requires its own explicit assignment".into());
        }
        Ok(Self {
            run_id,
            assignments: slots,
            worktrees,
            journal,
            activity: None,
            failure: None,
            outputs: Mutex::new(HashMap::new()),
            preparation: Mutex::new(()),
            reasons: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn with_activity(mut self, activity: JobActivity) -> Self {
        self.activity = Some(activity);
        self
    }

    pub(crate) fn with_failure(mut self, failure: FailureActivity) -> Self {
        self.failure = Some(failure);
        self
    }

    fn assignment(&self, node: &Node) -> Option<&Assignment> {
        let assignment = self.assignments.get(&node.id)?;
        (assignment.node == *node
            && assignment
                .grants
                .check(
                    &assignment.receipt,
                    queue_permission(node),
                    &assignment.scope,
                )
                .is_ok()
            && self
                .worktrees
                .validate_base(&assignment.project, &assignment.base)
                .is_ok()
            && assignment.worktree.get().is_none_or(|tree| {
                self.worktrees
                    .validate_owned(&assignment.project, tree)
                    .is_ok()
            }))
        .then_some(assignment)
    }

    fn prepare(
        &self,
        assignment: &Assignment,
        stop: &Cancellation,
    ) -> Result<OwnedWorktree, String> {
        // Allocation uses the existing Git registry lock. Serializing only this
        // short preparation stage prevents sibling jobs from racing that lock;
        // provider and test processes still execute in parallel.
        let started = Instant::now();
        let _lock = loop {
            stop.check()?;
            match self.preparation.try_lock() {
                Ok(lock) => break lock,
                Err(std::sync::TryLockError::WouldBlock)
                    if started.elapsed() < Duration::from_secs(120) =>
                {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => return Err("Worktree preparation did not become available".into()),
            }
        };
        if let Some(tree) = assignment.worktree.get() {
            return Ok(tree.clone());
        }
        let inputs = {
            let outputs = self
                .outputs
                .lock()
                .map_err(|_| "Artifact output storage is unavailable")?;
            assignment
                .node
                .inputs
                .iter()
                .map(|input| {
                    let source = outputs
                        .get(&input.node)
                        .ok_or("Required artifact has not been produced")?;
                    if input.proof == InputProof::Tested && !source.tested {
                        return Err("Required artifact has not passed its test suite");
                    }
                    Ok(source.artifact.clone())
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        let tree = self.worktrees.allocate_with_inputs(
            &assignment.project,
            self.run_id,
            &assignment.node.id,
            &assignment.base,
            &inputs,
            stop,
        )?;
        assignment
            .worktree
            .set(tree.clone())
            .map_err(|_| "Job already has an allocated checkout")?;
        Ok(tree)
    }

    fn note(&self, node: &str, message: String) {
        if let Ok(mut reasons) = self.reasons.lock() {
            reasons.insert(node.into(), message);
        }
    }

    fn finish_failed(&self, claim: &Claim, stop: &Cancellation, outcome: Outcome) -> Outcome {
        let outcome = if stop.requested() {
            Outcome::Cancelled
        } else {
            outcome
        };
        if self
            .journal
            .settle(claim, persisted(outcome), None)
            .is_err()
        {
            Outcome::Unverified
        } else {
            outcome
        }
    }

    fn run_test(
        &self,
        assignment: &Assignment,
        tree: &OwnedWorktree,
        suite: Suite,
        claim: &Claim,
        stop: &Cancellation,
    ) -> Outcome {
        if stop.requested() || self.journal.mark_dispatch(claim).is_err() {
            return self.finish_failed(claim, stop, Outcome::Unverified);
        }
        let result = test_runner::verify_approved_report(
            &self.worktrees,
            &assignment.project,
            tree,
            suite,
            &assignment.grants,
            &assignment.receipt,
            stop,
        );
        let verified = result
            .as_ref()
            .ok()
            .map(|result| result.verification.outcome);
        let mut artifact = None;
        let mut outcome = match verified {
            Some(VerifiedOutcome::Verified) => match self.worktrees.checkpoint_artifact(
                &assignment.project,
                tree,
                &assignment.base,
            ) {
                Ok(Some(proof)) => {
                    artifact = Some(proof);
                    Outcome::Tested
                }
                _ => Outcome::Unverified,
            },
            Some(VerifiedOutcome::Failed) => Outcome::Failed,
            _ => Outcome::Unverified,
        };
        if stop.requested() {
            outcome = Outcome::Cancelled;
            artifact = None;
        }
        self.note(
            &assignment.node.id,
            format!(
                "{} at {}: {}",
                suite.label(),
                tree.start_commit(),
                match outcome {
                    Outcome::Tested => "passed",
                    Outcome::Failed => "failed",
                    Outcome::Cancelled => "stopped",
                    _ => "unverified",
                }
            ),
        );
        let evidence = TestEvidence {
            suite,
            commit: tree.start_commit().to_owned(),
            passed: match (outcome, verified) {
                (Outcome::Tested, _) => Some(true),
                (Outcome::Failed, Some(VerifiedOutcome::Failed)) => Some(false),
                _ => None,
            },
            observed_at: unix_now(),
        };
        if self
            .journal
            .settle_test(claim, persisted(outcome), evidence, artifact.clone())
            .is_err()
        {
            return Outcome::Unverified;
        }
        if outcome == Outcome::Failed
            && !stop.requested()
            && let (Some(on_failure), Ok(report)) = (&self.failure, result)
            && let Some(draft) = report.correction
        {
            on_failure(draft);
        }
        if let Some(artifact) = artifact {
            let Ok(mut outputs) = self.outputs.lock() else {
                return Outcome::Unverified;
            };
            outputs.insert(
                assignment.node.id.clone(),
                Produced {
                    artifact,
                    tested: true,
                },
            );
        }
        outcome
    }

    fn execute_prepared(
        &self,
        assignment: &Assignment,
        tree: &OwnedWorktree,
        claim: &Claim,
        stop: &Cancellation,
    ) -> Outcome {
        let (permission, scope) = match &assignment.node.job {
            Job::Test { suite, .. } => (
                Permission::RunTests,
                test_runner::scope_for_suite(&assignment.project, tree, *suite),
            ),
            Job::Harness { .. } => {
                match approval_scope(
                    &assignment.node,
                    assignment.project.id,
                    tree,
                    &assignment.proof,
                ) {
                    Ok(scope) => (Permission::StartHarness, scope),
                    Err(_) => return self.finish_failed(claim, stop, Outcome::Unverified),
                }
            }
            Job::Desktop { .. } => return self.finish_failed(claim, stop, Outcome::Failed),
        };
        let activation = if permission == Permission::StartHarness && scope == assignment.scope {
            assignment
                .grants
                .activate_harness(&assignment.receipt, &scope)
        } else {
            assignment.grants.activate_job(
                &assignment.receipt,
                &assignment.scope,
                &scope,
                permission,
            )
        };
        if let Err(reason) = activation {
            self.note(
                &assignment.node.id,
                format!("{reason}. The prepared checkout was retained. Review a fresh plan before starting."),
            );
            return self.finish_failed(claim, stop, Outcome::Failed);
        }
        match assignment.node.job {
            Job::Test { suite, .. } => self.run_test(assignment, tree, suite, claim, stop),
            Job::Harness { .. } => {
                if let Err(reason) =
                    assignment
                        .grants
                        .consume(&assignment.receipt, Permission::StartHarness, &scope)
                {
                    self.note(&assignment.node.id, reason);
                    return self.finish_failed(claim, stop, Outcome::Failed);
                }
                self.run_harness(assignment, tree, claim, stop)
            }
            Job::Desktop { .. } => Outcome::Failed,
        }
    }

    fn run_harness(
        &self,
        assignment: &Assignment,
        tree: &OwnedWorktree,
        claim: &Claim,
        stop: &Cancellation,
    ) -> Outcome {
        let Job::Harness { goal, harness, .. } = &assignment.node.job else {
            return Outcome::Failed;
        };
        let Some(adapter) = &assignment.adapter else {
            return Outcome::Failed;
        };
        let harness_kind = to_kind(*harness);
        let observed = Arc::new(OnceLock::new());
        let sink: EventSink = {
            let journal = Arc::clone(&self.journal);
            let claim = claim.clone();
            let observed = Arc::clone(&observed);
            let activity = self.activity.clone();
            let node_id = assignment.node.id.clone();
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
        let commands = artifact_commands(&assignment.node);
        let prompt = if commands.is_empty() {
            goal.clone()
        } else {
            format!(
                "{goal}\n\nThe owner reviewed a committed output in this isolated checkout. After implementing the goal, stage and commit with these exact approved commands, one at a time:\n{}\nThe output must be a changed clean commit; do not push or modify another checkout.",
                commands.join("\n")
            )
        };
        let request = Request {
            workspace: tree.path().to_path_buf(),
            prompt,
            session: None,
            access: if adapter.descriptor().supports_workspace_write() {
                WorkAccess::Workspace
            } else {
                WorkAccess::SelectionOnly
            },
            allow_commands: commands,
        };
        if stop.requested() || self.journal.mark_dispatch(claim).is_err() {
            return self.finish_failed(claim, stop, Outcome::Unverified);
        }
        let owned = match adapter.start(request, sink, stop) {
            Ok(owned) => owned,
            Err(_) => return self.finish_failed(claim, stop, Outcome::Failed),
        };
        let result = owned.join();
        if result.status != RunStatus::Completed
            && let Some(termination) = result.termination
            && let Ok(mut reasons) = self.reasons.lock()
        {
            reasons.insert(assignment.node.id.clone(), termination.summary().into());
        }
        let delivery = Verification::harness_delivery(&result, None, unix_now());
        let evidence_id = result
            .accepted_session
            .as_ref()
            .filter(|session| session.kind() == harness_kind)
            .map(|session| session.as_str());
        let mut artifact = None;
        let mut outcome = if stop.requested() || result.status == RunStatus::Cancelled {
            Outcome::Cancelled
        } else {
            match delivery.outcome {
                VerifiedOutcome::Failed => Outcome::Failed,
                VerifiedOutcome::Verified if evidence_id.is_some() => match &assignment.proof {
                    Proof::DeliveryOnly => Outcome::Delivered,
                    Proof::CommittedArtifact => {
                        match self.worktrees.committed_artifact(&assignment.project, tree) {
                            Ok(Some(proof)) => {
                                artifact = Some(proof);
                                Outcome::Verified
                            }
                            Ok(None) => Outcome::Failed,
                            Err(_) => Outcome::Unverified,
                        }
                    }
                    Proof::ChangedFile(path) => {
                        let changes = self.worktrees.changed_paths(&assignment.project, tree).ok();
                        match Verification::required_git_change(
                            &assignment.node.id,
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
                _ => Outcome::Unverified,
            }
        };
        if stop.requested() {
            artifact = None;
            outcome = Outcome::Cancelled;
        }
        if (evidence_id.is_none() && observed.get().is_some_and(Result::is_err))
            || self
                .journal
                .settle_with_artifact(claim, persisted(outcome), evidence_id, artifact.clone())
                .is_err()
        {
            return Outcome::Unverified;
        }
        if let Some(artifact) = artifact {
            let Ok(mut outputs) = self.outputs.lock() else {
                return Outcome::Unverified;
            };
            outputs.insert(
                assignment.node.id.clone(),
                Produced {
                    artifact,
                    tested: false,
                },
            );
        }
        outcome
    }
}

impl Drop for ApprovedHarnessExecutor {
    fn drop(&mut self) {
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
            .map(|assignment| assignment.node.id.clone())
    }

    fn outcome_reason(&self, node: &Node) -> Option<String> {
        self.reasons.lock().ok()?.get(&node.id).cloned()
    }

    fn execute(&self, node: Node, stop: Cancellation) -> Outcome {
        let Some(assignment) = self.assignment(&node) else {
            return Outcome::Failed;
        };
        if stop.requested() {
            return Outcome::Cancelled;
        }
        let reservation = match Reservation::planned(&assignment.project, &assignment.base) {
            Ok(reservation) => reservation,
            Err(_) => return Outcome::Unverified,
        };
        let claim = match self.journal.begin_preparation(
            self.run_id,
            &node.id,
            &assignment.fingerprint,
            reservation,
        ) {
            Ok(Begin::Fresh(claim)) => claim,
            _ => return Outcome::Unverified,
        };
        let tree = match self.prepare(assignment, &stop) {
            Ok(tree) => tree,
            Err(reason) => {
                self.note(&node.id, reason);
                return self.finish_failed(&claim, &stop, Outcome::Unverified);
            }
        };
        if self
            .worktrees
            .verify_start(&assignment.project, &tree)
            .is_err()
            || self.journal.record_checkout(&claim, &tree).is_err()
        {
            return self.finish_failed(&claim, &stop, Outcome::Unverified);
        }
        if stop.requested() {
            return self.finish_failed(&claim, &stop, Outcome::Cancelled);
        }
        self.execute_prepared(assignment, &tree, &claim, &stop)
    }
}

/// Exact command strings are compiled from a safe node ID and shown during
/// review. The provider chooses when to invoke them; X never commits edits
/// behind the owner's back or grants a wildcard Git command permission.
pub(crate) fn artifact_commands(node: &Node) -> Vec<String> {
    if !node.required_commit {
        return Vec::new();
    }
    vec![
        "git -c core.hooksPath= -c core.fsmonitor=false add --all -- .".into(),
        format!(
            "git -c core.hooksPath= -c core.fsmonitor=false -c commit.gpgSign=false -c user.name='X coding job' -c user.email=x@localhost commit -m 'X job {}'",
            node.id
        ),
    ]
}

pub(crate) fn queue_permission(node: &Node) -> Permission {
    if matches!(node.job, Job::Test { .. }) {
        Permission::QueueTests
    } else {
        Permission::QueueHarness
    }
}

pub(crate) fn proof_for(node: &Node) -> Proof {
    if node.required_commit {
        Proof::CommittedArtifact
    } else {
        node.required_change
            .as_ref()
            .map_or(Proof::DeliveryOnly, |path| Proof::ChangedFile(path.clone()))
    }
}

pub(crate) fn planned_scope(
    node: &Node,
    project_id: Uuid,
    run_id: Uuid,
    base: &str,
    proof: &Proof,
) -> Result<Scope, String> {
    let intent = serde_json::to_string(&(node, base, proof))
        .map_err(|_| "Could not identify the approved task request")?;
    Ok(Scope::Task {
        project_id,
        run_id,
        node_id: node.id.clone(),
        intent,
    })
}

pub(crate) fn approval_scope(
    node: &Node,
    project_id: Uuid,
    worktree: &OwnedWorktree,
    proof: &Proof,
) -> Result<Scope, String> {
    planned_scope(
        node,
        project_id,
        worktree.run_id(),
        worktree.start_commit(),
        proof,
    )
}

pub(crate) fn to_kind(kind: HarnessKind) -> Kind {
    match kind {
        HarnessKind::OpenCode => Kind::OpenCode,
        HarnessKind::ClaudeCode => Kind::ClaudeCode,
    }
}

fn persisted(outcome: Outcome) -> State {
    match outcome {
        Outcome::Launched | Outcome::Delivered => State::Delivered,
        Outcome::Verified | Outcome::Tested => State::Verified,
        Outcome::Failed => State::Failed,
        Outcome::Cancelled => State::Cancelled,
        Outcome::Unverified | Outcome::Skipped => State::Unverified,
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(all(test, unix))]
mod tests;

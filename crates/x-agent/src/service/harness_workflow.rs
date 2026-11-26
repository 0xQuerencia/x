use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::harness::Adapter;
use crate::projects::{Project, Registry};
use crate::workflows::dag::{Graph, Job};
use crate::workflows::desktop_exec::{self, Prepared};
use crate::workflows::harness_exec::{
    ApprovedHarnessExecutor, Assignment, artifact_commands, planned_scope, proof_for,
    queue_permission, to_kind,
};
use crate::workflows::recovery::{Claim, Journal, State};
use crate::workflows::scheduler::{self, JobStops, NodeUpdate, Outcome};
use crate::worktrees::Manager;

use super::corrections::Binding;
use super::edit::{step, task};
use super::{AuditKind, Cancellation, Phase, Review, Service, Task, audit_best_effort, busy, emit};

mod inventory;
mod mixed;
mod renewal;
pub use renewal::PermissionWait;
pub(super) use renewal::permission_waits;

const WORKTREE_FOLDER: &str = "X Worktrees";
const PARALLEL_WORKERS: usize = 2;

fn node_status(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Launched => "launched",
        Outcome::Delivered => "delivered",
        Outcome::Verified => "done",
        Outcome::Tested => "tested",
        Outcome::Failed => "failed",
        Outcome::Unverified => "unverified",
        Outcome::Cancelled => "cancelled",
        Outcome::Skipped => "skipped",
    }
}

pub(super) struct PendingGraph {
    graph: Graph,
    home: PathBuf,
    base: PathBuf,
    manager: Arc<Manager>,
    registry: Arc<Registry>,
    journal: Arc<Journal>,
    jobs: Vec<Option<PlannedJob>>,
    desktop: Vec<Prepared>,
    correction: Option<Binding>,
}

impl PendingGraph {
    pub(super) fn project_roots(&self) -> Vec<PathBuf> {
        let mut roots = self
            .jobs
            .iter()
            .flatten()
            .map(|job| job.project.root.clone())
            .collect::<Vec<_>>();
        roots.sort();
        roots.dedup();
        roots
    }

    pub(super) fn background_capable(&self) -> bool {
        self.graph
            .nodes
            .iter()
            .all(|node| matches!(node.job, Job::Harness { .. } | Job::Test { .. }))
    }

    pub(super) fn job_stops(&self, root: &Cancellation) -> JobStops {
        JobStops::new(self.graph.nodes.len(), root)
    }
}

struct PlannedJob {
    project: Project,
    commit: String,
    adapter: Option<Arc<dyn Adapter>>,
}

fn claim_status_preview(mut claims: Vec<Claim>) -> serde_json::Value {
    claims.sort_by(|a, b| {
        a.run_id
            .cmp(&b.run_id)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
    let jobs = claims
        .into_iter()
        .map(|claim| {
            let delivery_observed = claim.evidence_id.is_some();
            let status = match claim.state {
                State::Claimed if claim.desktop.is_some() => "possibly_executed",
                State::Claimed if delivery_observed => "delivery_observed",
                State::Claimed => "possibly_delivered",
                State::Delivered if claim.desktop.is_some() => "launched",
                State::Delivered => "delivered",
                State::Verified
                    if claim
                        .test_evidence
                        .as_ref()
                        .is_some_and(|test| test.passed == Some(true)) =>
                {
                    "tested"
                }
                State::Verified => "verified",
                State::Failed => "failed",
                State::Unverified => "unverified",
                State::Cancelled => "cancelled",
            };
            serde_json::json!({
                "run_id": claim.run_id,
                "node_id": claim.node_id,
                "status": status,
                "attempt": claim.attempt,
                "evidence_recorded": delivery_observed,
                "requires_provider_check_before_retry": !delivery_observed && claim.desktop.is_none(),
                "requires_new_desktop_review": claim.desktop.is_some(),
                "retry_blocked_by_delivery": delivery_observed,
                "artifact": claim.artifact,
                "test_evidence": claim.test_evidence,
                "desktop_outcome": claim.desktop.and_then(|desktop| desktop.outcome),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "count": jobs.len(),
        "jobs": jobs,
        "executed": false,
        "retried": false,
        "provider_checked": false,
        "note": "A provider acknowledgement confirms receipt, not success. Coding retries require proof of non-delivery and fresh approval. Interrupted desktop effects require observation and a new reviewed plan; they are never replayed from this history.",
    })
}

fn reconciliation_preview(mut claims: Vec<Claim>) -> serde_json::Value {
    claims.sort_by(|a, b| {
        a.run_id
            .cmp(&b.run_id)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
    let jobs = claims
        .into_iter()
        .map(|claim| {
            serde_json::json!({
                "run_id": claim.run_id,
                "node_id": claim.node_id,
                "status": "requires_reconciliation",
                "delivery_observed": claim.evidence_id.is_some(),
                "requires_new_desktop_review": claim.desktop.is_some(),
                "requires_provider_check_before_retry": claim.desktop.is_none() && claim.evidence_id.is_none(),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "count": jobs.len(),
        "jobs": jobs,
        "executed": false,
        "retried": false,
        "note": "An acknowledged coding job still needs outcome reconciliation; unknown delivery requires proof of non-delivery before retry. Interrupted desktop effects require observation and a new reviewed plan, never automatic replay.",
    })
}

impl Service {
    /// Read unresolved workflow claims without exposing prompts, request hashes
    /// or provider evidence IDs. This never retries, settles or starts any job.
    pub fn uncertain_coding_jobs(&self) -> Result<serde_json::Value, String> {
        let (dir, _) = self
            .workflow_dirs
            .lock()
            .map_err(|_| "Workflow storage is unavailable")?
            .clone()
            .ok_or("Workflow storage is not configured")?;
        let support = dir
            .parent()
            .ok_or("Could not locate X's local data directory")?;
        let claims = Journal::new(support.join("workflow-journal")).uncertain()?;
        Ok(reconciliation_preview(claims))
    }

    /// Read-only local claim history. Does not expose prompts, fingerprints,
    /// provider evidence identifiers or any token needed to forge approval.
    pub fn coding_claims_summary(&self) -> Result<serde_json::Value, String> {
        let (dir, _) = self
            .workflow_dirs
            .lock()
            .map_err(|_| "Workflow storage is unavailable")?
            .clone()
            .ok_or("Workflow storage is not configured")?;
        let support = dir
            .parent()
            .ok_or("Could not locate X's local data directory")?;
        let claims = Journal::new(support.join("workflow-journal")).snapshot()?;
        Ok(claim_status_preview(claims))
    }

    pub fn plan_graph_workflow(&self, id: &str) -> Result<Task, String> {
        self.plan_graph_workflow_pinned(id, None)
    }

    pub(super) fn plan_graph_workflow_pinned(
        &self,
        id: &str,
        pinned: Option<&Project>,
    ) -> Result<Task, String> {
        let graph = self.workflow_store().get_graph(id)?;
        if graph.id != id {
            return Err("The workflow file has a different ID".into());
        }
        self.plan_graph_review(graph, pinned, None)
    }

    pub(super) fn plan_graph_review(
        &self,
        graph: Graph,
        pinned: Option<&Project>,
        correction: Option<Binding>,
    ) -> Result<Task, String> {
        let phase = self.lock().phase;
        if phase.is_busy() {
            return Err(busy(phase));
        }
        let (dir, home) = self
            .workflow_dirs
            .lock()
            .map_err(|_| "Workflow storage is unavailable")?
            .clone()
            .ok_or("Workflow storage is not configured")?;
        let support = dir
            .parent()
            .ok_or("Could not locate X's local data directory")?;
        let base = home.join(WORKTREE_FOLDER);
        let manager = Arc::new(Manager::new(base.clone(), home.clone()));
        let registry = Arc::new(Registry::new(support.to_path_buf(), home.clone()));
        let journal = Arc::new(Journal::new(support.join("workflow-journal")));
        let desktop =
            desktop_exec::prepare(&graph, self.machine().as_ref(), &self.workflow_config())?;
        let mut projects = HashMap::<String, (Project, String)>::new();
        let mut jobs = Vec::with_capacity(graph.nodes.len());
        let mut steps = Vec::with_capacity(graph.nodes.len());
        for node in &graph.nodes {
            let (project_id, description, adapter) = match &node.job {
                Job::Harness {
                    harness,
                    project_id,
                    goal,
                } => {
                    let kind = to_kind(*harness);
                    let adapter = self
                        .work_adapter(kind)
                        .cloned()
                        .ok_or_else(|| format!("{} is unavailable", kind.label()))?;
                    (
                        project_id,
                        format!("{} · {goal}", kind.label()),
                        Some(adapter),
                    )
                }
                Job::Test { project_id, suite } => {
                    let (program, args) = suite.command();
                    (
                        project_id,
                        format!(
                            "Run {program} {} ({}) at the exact input commit (120 second limit)",
                            args.join(" "),
                            suite.label()
                        ),
                        None,
                    )
                }
                Job::Desktop { action } => {
                    let prepared = desktop
                        .iter()
                        .find(|prepared| prepared.id() == node.id)
                        .ok_or("The desktop review is missing its exact node")?;
                    steps.push(step(&prepared.description(), action.risk()));
                    jobs.push(None);
                    continue;
                }
            };
            if !projects.contains_key(project_id) {
                let project = registry.validated_resolve(project_id)?;
                if pinned.is_some_and(|expected| {
                    expected.id != project.id
                        || expected.root != project.root
                        || expected.git_common_dir != project.git_common_dir
                }) {
                    return Err("The selected project changed while preparing the workflow".into());
                }
                let commit = if let Some(binding) = &correction {
                    binding.validate_source(&manager, &project, &journal)?;
                    binding.commit.clone()
                } else {
                    manager.starting_commit(&project)?
                };
                super::roadmap::validate_source(&graph, &project, &commit, &home)?;
                projects.insert(project_id.clone(), (project, commit));
            }
            let (project, commit) = &projects[project_id];
            let proof_note = if matches!(node.job, Job::Test { .. }) {
                "required successful test process at the exact assembled commit".to_owned()
            } else if node.required_commit {
                format!(
                    "required committed artifact; permits exact commands: {}",
                    artifact_commands(node).join(" ; ")
                )
            } else {
                node.required_change.as_ref().map_or_else(
                    || "delivery only".to_owned(),
                    |path| format!("required file change: {path}"),
                )
            };
            let preview = step(
                &format!(
                    "{} · {description} · {} at base {commit} · {proof_note}{}",
                    node.id,
                    project.root.display(),
                    if node.inputs.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " · assemble approved input commits in order: {}",
                            node.inputs
                                .iter()
                                .map(|input| format!("{} ({:?})", input.node, input.proof))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                ),
                if matches!(node.job, Job::Test { .. }) {
                    "runs repository test code with your OS access in an isolated checkout; this is not a sandbox"
                } else if adapter
                    .as_ref()
                    .is_some_and(|adapter| adapter.descriptor().supports_workspace_write())
                {
                    "shares the goal and allows edits in a separate Git worktree"
                } else {
                    "shares the goal for tool-free analysis; no workspace edits"
                },
            );
            steps.push(preview);
            jobs.push(Some(PlannedJob {
                project: project.clone(),
                commit: commit.clone(),
                adapter,
            }));
        }

        let mut state = self.lock();
        if pinned.is_some_and(|expected| state.workspace.as_ref() != Some(&expected.root)) {
            return Err("The selected project changed before review".into());
        }
        if state.phase.is_busy() {
            return Err(busy(state.phase));
        }
        state.serial += 1;
        let source_note = graph.roadmap.as_ref().map_or_else(String::new, |source| {
            format!(" · Markdown source {} (SHA-256 {}), roadmap {} tasks {}. Completion markers are author-supplied; semantic code/PR matching is not performed.", source.path.display(), source.sha256, source.roadmap_id, source.tasks.iter().map(|task| task.task_id.as_str()).collect::<Vec<_>>().join(", "))
        });
        let correction_note = correction.as_ref().map_or_else(String::new, |binding| {
            format!(
                " · correction {}/2 of failure {}/{} at {}",
                binding.attempt, binding.source_run, binding.source_node, binding.commit
            )
        });
        let preview = format!(
            "Run reviewed workflow {} with coding checkouts in {} (queued jobs may start for 30 minutes after approval; desktop steps reacquire input and recheck exact targets){source_note}{correction_note}",
            graph.name,
            base.display()
        );
        let mut task = task(state.serial, &preview, steps, None);
        task.phase = Phase::AwaitingApproval;
        state.graph_workflow = Some(PendingGraph {
            graph,
            home,
            base,
            manager,
            registry,
            journal,
            jobs,
            desktop,
            correction,
        });
        state.phase = Phase::AwaitingApproval;
        state.review = Review::Required;
        state.actions.clear();
        state.grants.clear();
        state.guard = None;
        state.edit = None;
        state.workflow = None;
        state.create_workspace = false;
        state.task = Some(task.clone());
        audit_best_effort(&mut state, AuditKind::Planned, None);
        emit(&mut state, "plan", &preview);
        Ok(task)
    }

    /// Request Stop for one exact step in the currently approved coding graph.
    /// A stale panel, non-coding task, or already finished step has no authority.
    pub fn cancel_coding_job(&self, task_id: u64, index: usize) -> Result<(), String> {
        let mut state = self.lock();
        if state.phase != Phase::Running || state.stop.requested() {
            return Err("No running coding graph".into());
        }
        let active = state
            .task
            .as_ref()
            .filter(|task| task.id == task_id)
            .and_then(|task| task.steps.get(index))
            .is_some_and(|step| {
                matches!(
                    step.status.as_str(),
                    "pending" | "running" | "waiting_for_permission"
                )
            });
        if !active {
            return Err("No matching active coding job".into());
        }
        let stops = state
            .active_graph_stops
            .as_ref()
            .ok_or("No running coding graph")?;
        if !stops.request(index)? {
            return Err("Stop was already requested for this job".into());
        }
        if let Some(step) = state
            .task
            .as_mut()
            .and_then(|task| task.steps.get_mut(index))
        {
            step.status = "stopping".into();
        }
        audit_best_effort(&mut state, AuditKind::StopRequested, Some(index));
        emit(
            &mut state,
            "info",
            &format!("Stop requested for coding job {}", index + 1),
        );
        Ok(())
    }

    pub(super) fn run_graph_workflow(&self, workflow: PendingGraph, stop: &Cancellation) {
        let result = self.run_approved_graph(&workflow, stop);
        match result {
            Ok(report) => {
                if workflow.correction.is_none()
                    && let Err(error) = self
                        .workflow_store()
                        .record_graph_last(&workflow.graph.id, &report)
                {
                    self.emit(
                        "warning",
                        &format!("Could not save coding workflow outcome: {error}"),
                    );
                }
                let complete = report
                    .nodes
                    .iter()
                    .filter(|node| node.outcome.completed())
                    .count();
                for node in &report.nodes {
                    let status = node_status(node.outcome);
                    let detail = if node.reason.is_empty() {
                        format!("{}: {status}", node.id)
                    } else {
                        format!("{}: {status} ({})", node.id, node.reason)
                    };
                    self.emit("verification", &detail);
                }
                let delivered = report
                    .nodes
                    .iter()
                    .filter(|node| node.outcome == Outcome::Delivered)
                    .count();
                let launched = report
                    .nodes
                    .iter()
                    .filter(|node| node.outcome == Outcome::Launched)
                    .count();
                let verified = complete - delivered - launched;
                let summary = format!(
                    "{complete}/{} jobs completed: {launched} launched without effect confirmation, {delivered} delivery-only, {verified} with additional proof",
                    report.nodes.len()
                );
                let phase = if report.stopped || stop.requested() {
                    Phase::Cancelled
                } else if complete == report.nodes.len() {
                    Phase::Completed
                } else {
                    Phase::Failed
                };
                self.finish(phase, None, Some(summary), None);
            }
            Err(error) if stop.requested() => {
                self.finish(Phase::Cancelled, None, Some(error), None);
            }
            Err(error) => self.finish(Phase::Failed, Some(error), None, None),
        }
    }

    fn run_approved_graph(
        &self,
        workflow: &PendingGraph,
        stop: &Cancellation,
    ) -> Result<scheduler::Report, String> {
        stop.check()?;
        let stops = self
            .lock()
            .active_graph_stops
            .clone()
            .ok_or("Coding graph cancellation scopes are unavailable")?;
        let mut checked = HashSet::new();
        for job in workflow.jobs.iter().flatten() {
            if checked.insert(job.project.id) {
                stop.check()?;
                let current = workflow
                    .registry
                    .validated_resolve(&job.project.id.to_string())?;
                if current.root != job.project.root
                    || current.git_common_dir != job.project.git_common_dir
                {
                    return Err("The registered project changed after the workflow preview".into());
                }
                if workflow.graph.roadmap.is_some() {
                    let base = workflow.manager.starting_commit(&current)?;
                    super::roadmap::validate_source(
                        &workflow.graph,
                        &current,
                        &base,
                        &workflow.home,
                    )?;
                }
            }
        }
        stop.check()?;
        if let Some(binding) = &workflow.correction {
            let project = workflow
                .jobs
                .iter()
                .flatten()
                .find(|job| job.project.id == binding.project_id)
                .ok_or("The correction project is missing")?;
            binding.validate_source(&workflow.manager, &project.project, &workflow.journal)?;
            stop.check()?;
            self.corrections
                .lock()
                .map_err(|_| "Correction history is unavailable")?
                .consume(binding)?;
        }
        if workflow.jobs.iter().any(Option::is_some) {
            match std::fs::create_dir(&workflow.base) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!(
                        "Could not create the approved worktree directory: {error}"
                    ));
                }
            }
        }
        let run = Uuid::new_v4();
        let executor = Arc::new(self.approved_graph_executor(workflow, run, stop, &stops)?);
        let result = scheduler::run_observed_scoped(
            &workflow.graph,
            &workflow.home,
            executor.clone(),
            &stops,
            PARALLEL_WORKERS,
            |index, update| {
                let status = match update {
                    NodeUpdate::WaitingForPermission => "waiting_for_permission",
                    NodeUpdate::Running => "running",
                    NodeUpdate::Finished(outcome) => node_status(outcome),
                };
                self.set_step(index, status);
                if matches!(update, NodeUpdate::Finished(_))
                    && let Some(execution) =
                        executor.desktop.execution(&workflow.graph.nodes[index].id)
                {
                    self.record_execution(index, execution);
                    self.set_step(index, status);
                }
            },
        );
        let reversals = executor.desktop.reversals();
        if !reversals.is_empty() {
            self.lock().undo = reversals;
        }
        result
    }
}

#[cfg(test)]
mod reconciliation_tests {
    use super::*;
    use crate::workflows::recovery::State;

    #[test]
    fn interrupted_jobs_preview_has_no_request_fingerprint_or_provider_evidence() {
        let run_id = Uuid::new_v4();
        let claims = vec![
            Claim {
                run_id,
                node_id: "z_back".into(),
                request_fingerprint: "private-coding-task-fingerprint".into(),
                attempt: 1,
                state: State::Claimed,
                evidence_id: Some("ses_private_provider_id".into()),
                artifact: None,
                test_evidence: None,
                worktree: None,
                desktop: None,
            },
            Claim {
                run_id,
                node_id: "a_front".into(),
                request_fingerprint: "another-private-fingerprint".into(),
                attempt: 1,
                state: State::Claimed,
                evidence_id: None,
                artifact: None,
                test_evidence: None,
                worktree: None,
                desktop: None,
            },
        ];
        let preview = reconciliation_preview(claims);
        assert_eq!(preview["count"], 2);
        assert_eq!(preview["jobs"][0]["node_id"], "a_front");
        assert_eq!(preview["jobs"][1]["node_id"], "z_back");
        assert_eq!(preview["jobs"][0]["delivery_observed"], false);
        assert_eq!(preview["jobs"][1]["delivery_observed"], true);
        assert_eq!(preview["executed"], false);
        assert_eq!(preview["retried"], false);
        let rendered = preview.to_string();
        assert!(!rendered.contains("fingerprint"));
        assert!(!rendered.contains("ses_private_provider_id"));
        assert!(!rendered.contains("private-coding-task"));
    }

    #[test]
    fn full_claim_preview_is_sorted_and_does_not_expose_private_evidence() {
        let run = Uuid::new_v4();
        let claims = vec![
            Claim {
                run_id: run,
                node_id: "z_job".into(),
                request_fingerprint: "private-fingerprint".into(),
                attempt: 2,
                state: State::Failed,
                evidence_id: Some("ses_private".into()),
                artifact: None,
                test_evidence: None,
                worktree: None,
                desktop: None,
            },
            Claim {
                run_id: run,
                node_id: "a_job".into(),
                request_fingerprint: "even-more-private".into(),
                attempt: 1,
                state: State::Verified,
                evidence_id: Some("provider_secret".into()),
                artifact: None,
                test_evidence: None,
                worktree: None,
                desktop: None,
            },
        ];
        let report = claim_status_preview(claims);
        assert_eq!(report["count"], 2);
        assert_eq!(report["jobs"][0]["node_id"], "a_job");
        assert_eq!(report["jobs"][0]["status"], "verified");
        assert_eq!(report["jobs"][1]["status"], "failed");
        assert_eq!(report["jobs"][1]["attempt"], 2);
        assert_eq!(report["jobs"][1]["evidence_recorded"], true);
        assert_eq!(report["jobs"][1]["retry_blocked_by_delivery"], true);
        assert_eq!(
            report["jobs"][1]["requires_provider_check_before_retry"],
            false
        );
        assert_eq!(report["executed"], false);
        assert_eq!(report["provider_checked"], false);
        let text = report.to_string();
        for private in ["fingerprint", "private", "provider_secret", "ses_private"] {
            assert!(!text.contains(private));
        }
    }

    #[test]
    fn empty_reconciliation_preview_is_safe_and_nonexecuting() {
        let preview = reconciliation_preview(vec![]);
        assert_eq!(preview["count"], 0);
        assert_eq!(preview["jobs"].as_array().unwrap().len(), 0);
        assert_eq!(preview["retried"], false);
    }
}

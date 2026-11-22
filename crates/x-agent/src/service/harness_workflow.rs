use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::grants::Permission;
use crate::harness::Adapter;
use crate::projects::{Project, Registry};
use crate::workflows::dag::{Graph, Job};
use crate::workflows::harness_exec::{
    ApprovedHarnessExecutor, Assignment, Proof, approval_scope, to_kind,
};
use crate::workflows::recovery::{Claim, Journal, State};
use crate::workflows::scheduler::{self, Outcome};
use crate::worktrees::{Manager, OwnedWorktree};

use super::edit::{step, task};
use super::{AuditKind, Cancellation, Phase, Review, Service, Task, audit_best_effort, busy, emit};

const WORKTREE_FOLDER: &str = "X Worktrees";
const PARALLEL_WORKERS: usize = 2;

pub(super) struct PendingGraph {
    graph: Graph,
    home: PathBuf,
    base: PathBuf,
    manager: Arc<Manager>,
    registry: Registry,
    journal: Arc<Journal>,
    jobs: Vec<PlannedJob>,
}

struct PlannedJob {
    project: Project,
    commit: String,
    adapter: Arc<dyn Adapter>,
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
            let (status, needs_reconciliation) = match claim.state {
                State::Claimed => ("possibly_delivered", true),
                State::Verified => ("verified", false),
                State::Failed => ("failed", true),
                State::Unverified => ("unverified", true),
                State::Cancelled => ("cancelled", true),
            };
            serde_json::json!({
                "run_id": claim.run_id,
                "node_id": claim.node_id,
                "status": status,
                "attempt": claim.attempt,
                "evidence_recorded": claim.evidence_id.is_some(),
                "requires_provider_check_before_retry": needs_reconciliation,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "count": jobs.len(),
        "jobs": jobs,
        "executed": false,
        "retried": false,
        "provider_checked": false,
        "note": "A status record is not evidence of delivery. Never retry without an independent provider check and a new approval.",
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
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "count": jobs.len(),
        "jobs": jobs,
        "executed": false,
        "retried": false,
        "note": "Check provider delivery independently before authorizing a new attempt",
    })
}

impl Service {
    /// Read unresolved coding claims without exposing prompts, request hashes or
    /// provider evidence IDs. This never retries, settles or starts any job.
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
        let graph = self.workflow_store().get_graph(id)?;
        if graph.id != id {
            return Err("The workflow file has a different ID".into());
        }
        let base = home.join(WORKTREE_FOLDER);
        let manager = Arc::new(Manager::new(base.clone(), home.clone()));
        let registry = Registry::new(support.to_path_buf(), home.clone());
        let mut projects = HashMap::<String, (Project, String)>::new();
        let mut jobs = Vec::with_capacity(graph.nodes.len());
        let mut steps = Vec::with_capacity(graph.nodes.len());
        for node in &graph.nodes {
            let Job::Harness {
                harness,
                project_id,
                goal,
            } = &node.job
            else {
                return Err("Use the standard workflow runner for desktop actions. Coding graphs require only harness jobs.".into());
            };
            let kind = to_kind(*harness);
            let adapter = self
                .work_adapter(kind)
                .cloned()
                .ok_or_else(|| format!("{} is unavailable", kind.label()))?;
            if !projects.contains_key(project_id) {
                let project = registry.validated_resolve(project_id)?;
                let commit = manager.starting_commit(&project)?;
                projects.insert(project_id.clone(), (project, commit));
            }
            let (project, commit) = &projects[project_id];
            let preview = step(
                &format!(
                    "{} · {} · {} at {commit} · {goal}",
                    node.id,
                    kind.label(),
                    project.root.display()
                ),
                "shares the goal with the harness and uses a separate Git worktree",
            );
            steps.push(preview);
            jobs.push(PlannedJob {
                project: project.clone(),
                commit: commit.clone(),
                adapter,
            });
        }

        let mut state = self.lock();
        if state.phase.is_busy() {
            return Err(busy(state.phase));
        }
        state.serial += 1;
        let preview = format!("Run coding workflow {} in {}", graph.name, base.display());
        let mut task = task(state.serial, &preview, steps, None);
        task.phase = Phase::AwaitingApproval;
        state.graph_workflow = Some(PendingGraph {
            graph,
            home,
            base,
            manager,
            registry,
            journal: Arc::new(Journal::new(support.join("workflow-journal"))),
            jobs,
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

    pub(super) fn run_graph_workflow(&self, workflow: PendingGraph, stop: &Cancellation) {
        let result = self.run_approved_graph(&workflow, stop);
        match result {
            Ok(report) => {
                if let Err(error) = self
                    .workflow_store()
                    .record_graph_last(&workflow.graph.id, &report)
                {
                    self.emit(
                        "warning",
                        &format!("Could not save coding workflow outcome: {error}"),
                    );
                }
                let mut complete = 0;
                for (index, node) in report.nodes.iter().enumerate() {
                    let status = match node.outcome {
                        Outcome::Verified => {
                            complete += 1;
                            "done"
                        }
                        Outcome::Failed => "failed",
                        Outcome::Unverified => "unverified",
                        Outcome::Cancelled => "cancelled",
                        Outcome::Skipped => "skipped",
                    };
                    self.set_step(index, status);
                    let detail = if node.reason.is_empty() {
                        format!("{}: {status}", node.id)
                    } else {
                        format!("{}: {status} ({})", node.id, node.reason)
                    };
                    self.emit("verification", &detail);
                }
                let summary = format!(
                    "{complete}/{} jobs returned verified delivery",
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
        let mut checked = HashSet::new();
        for job in &workflow.jobs {
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
            }
        }
        stop.check()?;
        match std::fs::create_dir(&workflow.base) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!(
                    "Could not create the approved worktree directory: {error}"
                ));
            }
        }
        let run = Uuid::new_v4();
        let mut worktrees = Vec::with_capacity(workflow.jobs.len());
        for (node, job) in workflow.graph.nodes.iter().zip(&workflow.jobs) {
            stop.check()?;
            let tree = workflow
                .manager
                .allocate(&job.project, run, &node.id, &job.commit, true)?;
            self.emit(
                "workflow",
                &format!("{} worktree: {}", node.id, tree.path().display()),
            );
            worktrees.push(tree);
        }

        let executor = self.approved_graph_executor(workflow, &worktrees, run, stop)?;
        scheduler::run(
            &workflow.graph,
            &workflow.home,
            Arc::new(executor),
            stop,
            PARALLEL_WORKERS,
        )
    }

    fn approved_graph_executor(
        &self,
        workflow: &PendingGraph,
        worktrees: &[OwnedWorktree],
        run: Uuid,
        stop: &Cancellation,
    ) -> Result<ApprovedHarnessExecutor, String> {
        let grants = self.scoped_grants();
        let result = (|| {
            let state = self.lock();
            stop.check()?;
            let mut assignments = Vec::with_capacity(workflow.jobs.len());
            for ((node, job), tree) in workflow
                .graph
                .nodes
                .iter()
                .zip(&workflow.jobs)
                .zip(worktrees)
            {
                let proof = Proof::DeliveryOnly;
                let scope = approval_scope(node, job.project.id, tree, &proof)?;
                let receipt = grants.issue(
                    Permission::StartHarness,
                    scope,
                    Duration::from_secs(60),
                    1,
                    true,
                )?;
                assignments.push(Assignment::from_approved_scope(
                    node,
                    job.project.clone(),
                    tree.clone(),
                    Arc::clone(&job.adapter),
                    proof,
                    Arc::clone(&grants),
                    receipt,
                )?);
            }
            drop(state);
            stop.check()?;
            ApprovedHarnessExecutor::new(
                &workflow.graph,
                run,
                assignments,
                Arc::clone(&workflow.manager),
                Arc::clone(&workflow.journal),
            )
        })();
        if result.is_err() {
            for id in workflow
                .jobs
                .iter()
                .map(|job| job.project.id)
                .collect::<HashSet<_>>()
            {
                let _ = grants.revoke_run(id, run);
            }
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
            },
            Claim {
                run_id,
                node_id: "a_front".into(),
                request_fingerprint: "another-private-fingerprint".into(),
                attempt: 1,
                state: State::Claimed,
                evidence_id: None,
            },
        ];
        let preview = reconciliation_preview(claims);
        assert_eq!(preview["count"], 2);
        assert_eq!(preview["jobs"][0]["node_id"], "a_front");
        assert_eq!(preview["jobs"][1]["node_id"], "z_back");
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
            },
            Claim {
                run_id: run,
                node_id: "a_job".into(),
                request_fingerprint: "even-more-private".into(),
                attempt: 1,
                state: State::Verified,
                evidence_id: Some("provider_secret".into()),
            },
        ];
        let report = claim_status_preview(claims);
        assert_eq!(report["count"], 2);
        assert_eq!(report["jobs"][0]["node_id"], "a_job");
        assert_eq!(report["jobs"][0]["status"], "verified");
        assert_eq!(report["jobs"][1]["status"], "failed");
        assert_eq!(report["jobs"][1]["attempt"], 2);
        assert_eq!(
            report["jobs"][1]["requires_provider_check_before_retry"],
            true
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

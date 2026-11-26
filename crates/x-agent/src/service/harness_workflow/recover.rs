//! Exact local checkout recovery. Historic claims carry no execution authority.

use super::*;
use crate::grants::Scope;
use crate::workflows::{recovery::Dispatch, roadmap::digest};
use crate::worktrees::Inspection;

impl Service {
    pub fn plan_coding_retry(&self, workflow: &str, run: &str, node: &str) -> Result<Task, String> {
        self.with_recovery_checkout(run, node, |manager, project, claim, inspection| {
            claim.validate_undispatched_retry()?;
            let reservation = claim
                .worktree
                .as_ref()
                .ok_or("This job has no recorded checkout")?;
            let mut graph = self.workflow_store().get_graph(workflow)?;
            if graph.id != workflow || graph.roadmap.is_some() {
                return Err(
                    "Choose the original saved coding workflow with no linked roadmap source"
                        .into(),
                );
            }
            graph.nodes.retain(|candidate| candidate.id == node);
            let candidate = graph
                .nodes
                .first()
                .filter(|node| {
                    matches!(node.job, Job::Harness { .. })
                        && node.after.is_empty()
                        && node.requires.is_empty()
                        && node.requires_verified.is_empty()
                        && node.inputs.is_empty()
                })
                .ok_or(
                    "Retry requires an independent coding node from the original saved workflow",
                )?;
            let Scope::Task { intent, .. } = planned_scope(
                candidate,
                project.id,
                claim.run_id,
                reservation.approved_base(),
                &proof_for(candidate),
            )?
            else {
                return Err("The approved coding request has the wrong scope".into());
            };
            if digest(intent.as_bytes()) != claim.request_fingerprint {
                return Err("The saved coding request changed since the recorded attempt".into());
            }
            manager.retry_checkout(project, claim.run_id, node, reservation, inspection)?;
            self.plan_graph_review_with_retry(graph, None, None, Some(claim.clone()))
        })
    }

    pub fn open_coding_worktree(&self, run: &str, node: &str) -> Result<(), String> {
        self.with_desktop_control(|| {
            self.with_recovery_checkout(run, node, |_, _, _, inspection| match inspection {
                Inspection::Owned { path, .. } => self.launcher.open_project_folder(&path),
                Inspection::Absent { .. } => Err("The recorded checkout is absent".into()),
            })
        })
    }

    pub fn preview_coding_worktree_cleanup(
        &self,
        run: &str,
        node: &str,
    ) -> Result<serde_json::Value, String> {
        self.with_recovery_checkout(run, node, |_, _, claim, inspection| {
            let (path, head, token) = cleanup_scope(claim, inspection)?;
            Ok(serde_json::json!({
                "run_id": claim.run_id,
                "node_id": claim.node_id,
                "attempt": claim.attempt,
                "path": path,
                "commit": head,
                "review_token": token,
                "removed": false,
            }))
        })
    }

    pub fn cleanup_coding_worktree(
        &self,
        run: &str,
        node: &str,
        token: &str,
    ) -> Result<(), String> {
        self.with_recovery_checkout(run, node, |manager, project, claim, inspection| {
            let (_, head, expected) = cleanup_scope(claim, inspection)?;
            if token != expected {
                return Err("The cleanup review changed; preview the exact checkout again".into());
            }
            let owned = manager.reattach(project, claim.run_id, &claim.node_id, &head)?;
            manager.remove_if_clean(project, &owned, true)
        })
    }

    fn with_recovery_checkout<T>(
        &self,
        raw_run: &str,
        node: &str,
        action: impl FnOnce(
            &Manager,
            &crate::projects::Project,
            &Claim,
            Inspection,
        ) -> Result<T, String>,
    ) -> Result<T, String> {
        let run = Uuid::parse_str(raw_run)
            .ok()
            .filter(|run| !run.is_nil())
            .ok_or("Enter the exact recorded run UUID")?;
        let (dir, home) = self
            .workflow_dirs
            .lock()
            .map_err(|_| "Workflow storage is unavailable")?
            .clone()
            .ok_or("Workflow storage is not configured")?;
        let support = dir.parent().ok_or("Local data directory unavailable")?;
        let journal = Journal::new(support.join("workflow-journal"));
        let claim = journal.exact_claim(run, node)?;
        let reservation = claim
            .worktree
            .as_ref()
            .ok_or("This job has no recorded checkout")?;
        let registry = Registry::new(support.to_path_buf(), home.clone());
        let project = registry.validated_resolve(&reservation.project_id().to_string())?;
        let _lease = self.reserve_worktree_review(project.root.clone())?;
        let manager = Manager::new(home.join(WORKTREE_FOLDER), home);
        let inspection = manager.inspect_reserved(&project, run, node, reservation)?;
        if journal.exact_claim(run, node)? != claim
            || registry.validated_resolve(&project.id.to_string())? != project
        {
            return Err("The recorded checkout changed; refresh its inventory".into());
        }
        action(&manager, &project, &claim, inspection)
    }
}

fn cleanup_scope(
    claim: &Claim,
    inspection: Inspection,
) -> Result<(std::path::PathBuf, String, String), String> {
    if !matches!(claim.state, State::Failed | State::Cancelled)
        || claim.dispatch != Some(Dispatch::Pending)
        || claim.evidence_id.is_some()
        || claim.artifact.is_some()
        || claim.test_evidence.is_some()
    {
        return Err(
            "Cleanup requires a stopped or failed preparation with no delivered work or evidence"
                .into(),
        );
    }
    let Inspection::Owned {
        path,
        current_commit,
        clean_for_cleanup: true,
        prepared_commit_matches: Some(true),
    } = inspection
    else {
        return Err(
            "The exact checkout is missing, changed, or contains files; nothing was removed".into(),
        );
    };
    let scope = serde_json::to_vec(&("clean-owned-checkout-v1", claim, &path, &current_commit))
        .map_err(|_| "Could not bind the cleanup review to the checkout")?;
    let token = digest(&scope);
    Ok((path, current_commit, token))
}

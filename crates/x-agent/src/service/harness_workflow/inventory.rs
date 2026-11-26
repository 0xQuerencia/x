//! Read-only recovery inventory. Journal claims remain historical evidence;
//! filesystem observation never restores authority or invents a terminal state.

use super::*;

impl Service {
    /// List recorded preparations, independently inspecting at most one graph's
    /// worth of recent entries. No prompt, receipt or provider token is exposed.
    pub fn coding_worktree_inventory(&self) -> Result<serde_json::Value, String> {
        self.worktree_inventory(None)
    }

    /// Inspect an older exact run without scanning unrelated project contents.
    pub fn coding_run_worktree_inventory(&self, run: &str) -> Result<serde_json::Value, String> {
        let run = Uuid::parse_str(run)
            .ok()
            .filter(|run| !run.is_nil())
            .ok_or("Enter the exact nonempty run UUID from the recovery history")?;
        self.worktree_inventory(Some(run))
    }

    /// Read a diff only for one explicitly selected, recorded run/node.
    /// Raw patch text stays in this local operator response; this does not
    /// authorize a provider request, tests, retry or integration.
    pub fn coding_worktree_review(
        &self,
        run: &str,
        node: &str,
    ) -> Result<crate::worktrees::WorktreeReview, String> {
        self.read_coding_worktree_review(run, node, true)
            .map(|(review, _)| review)
    }

    pub(in crate::service) fn read_coding_worktree_review(
        &self,
        run: &str,
        node: &str,
        reserve: bool,
    ) -> Result<(crate::worktrees::WorktreeReview, Project), String> {
        let run = Uuid::parse_str(run)
            .ok()
            .filter(|run| !run.is_nil())
            .ok_or("Enter the exact recorded run UUID")?;
        let (dir, home) = self
            .workflow_dirs
            .lock()
            .map_err(|_| "Workflow storage is unavailable")?
            .clone()
            .ok_or("Workflow storage is not configured")?;
        let support = dir.parent().ok_or("Local data directory is unavailable")?;
        let journal = Journal::new(support.join("workflow-journal"));
        let exact_claim = || journal.exact_claim(run, node);
        let claim = exact_claim()?;
        if matches!(claim.state, State::Claimed | State::Unverified) {
            return Err("Review is unavailable while this claim's outcome is uncertain".into());
        }
        let reservation = claim.worktree.as_ref().ok_or("No recorded checkout")?;
        if reservation.resolved_commit().is_none() {
            return Err("The exact prepared checkout was never recorded".into());
        }
        let registry = Registry::new(support.to_path_buf(), home.clone());
        let project = registry.validated_resolve(&reservation.project_id().to_string())?;
        let _lease = reserve
            .then(|| self.reserve_worktree_review(project.root.clone()))
            .transpose()?;
        if exact_claim()? != claim {
            return Err("The recorded worktree changed; refresh the review".into());
        }
        let manager = Manager::new(home.join(WORKTREE_FOLDER), home);
        let mut review = manager.review_reserved(
            &project,
            run,
            node,
            reservation,
            claim.test_evidence.as_ref(),
        )?;
        if exact_claim()? != claim
            || registry.validated_resolve(&project.id.to_string())? != project
        {
            return Err("The recorded worktree changed; refresh the review".into());
        }
        review.recorded_artifact = claim
            .artifact
            .as_ref()
            .map(|proof| proof.commit().to_owned());
        Ok((review, project))
    }

    fn worktree_inventory(&self, run: Option<Uuid>) -> Result<serde_json::Value, String> {
        let (dir, home) = self
            .workflow_dirs
            .lock()
            .map_err(|_| "Workflow storage is unavailable")?
            .clone()
            .ok_or("Workflow storage is not configured")?;
        let support = dir
            .parent()
            .ok_or("Could not locate X's local data directory")?;
        let claims = Journal::new(support.join("workflow-journal")).snapshot()?;
        let registry = Registry::new(support.to_path_buf(), home.clone());
        let manager = Manager::new(home.join(WORKTREE_FOLDER), home);
        Ok(inventory(&registry, &manager, claims, run))
    }
}

fn inventory(
    registry: &Registry,
    manager: &Manager,
    claims: Vec<Claim>,
    run: Option<Uuid>,
) -> serde_json::Value {
    let limit = crate::workflows::dag::MAX_NODES;
    let mut inspected = 0;
    let mut jobs = claims
        .into_iter()
        .rev()
        .filter(|claim| run.is_none_or(|run| claim.run_id == run))
        .map(|claim| {
            let inspection = match &claim.worktree {
                None if claim.desktop.is_some() => serde_json::json!({
                    "state": "not_applicable",
                    "reason": "This desktop node has no coding checkout",
                }),
                None => serde_json::json!({
                    "state": "unrecorded",
                    "reason": "This older claim has no durable preparation identity; no path was assumed",
                }),
                Some(_) if inspected == limit => serde_json::json!({
                    "state": "not_inspected",
                    "reason": "Inspection limit reached; request this exact run to inspect its checkouts",
                }),
                Some(reservation) => {
                    inspected += 1;
                    match registry
                        .resolve(&reservation.project_id().to_string())
                        .and_then(|project| {
                            manager.inspect_reserved(
                                &project,
                                claim.run_id,
                                &claim.node_id,
                                reservation,
                            )
                        }) {
                        Ok(observation) => serde_json::json!(observation),
                        Err(reason) => serde_json::json!({
                            "state": "unverified",
                            "reason": reason,
                        }),
                    }
                }
            };
            serde_json::json!({
                "run_id": claim.run_id,
                "node_id": claim.node_id,
                "attempt": claim.attempt,
                "recorded_state": claim.state,
                "delivery_observed": claim.evidence_id.is_some(),
                "reservation": claim.worktree,
                "artifact": claim.artifact,
                "inspection": inspection,
            })
        })
        .collect::<Vec<_>>();
    jobs.sort_by(|a, b| {
        a["run_id"]
            .as_str()
            .cmp(&b["run_id"].as_str())
            .then_with(|| a["node_id"].as_str().cmp(&b["node_id"].as_str()))
    });
    serde_json::json!({
        "count": jobs.len(),
        "inspected": inspected,
        "inspection_limit": limit,
        "jobs": jobs,
        "executed": false,
        "retried": false,
        "removed": false,
        "note": "Recorded outcome and current filesystem state are separate. Interrupted claims remain unresolved. Clean status is an observation, never cleanup approval.",
    })
}

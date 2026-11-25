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

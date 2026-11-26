//! Read-only delivery reconciliation. A missing session never proves that
//! OpenCode did not receive or start processing the request.

use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::harness::{Kind, SessionId};
use crate::workflows::recovery::Dispatch;
use crate::worktrees::Inspection;

impl Service {
    pub fn reconcile_coding_delivery(
        &self,
        raw_run: &str,
        node: &str,
        lookup: impl FnOnce(&Path, &SessionId) -> Result<(), String>,
    ) -> Result<Value, String> {
        let run = Uuid::parse_str(raw_run)
            .ok()
            .filter(|run| !run.is_nil())
            .ok_or("Enter an exact recorded run UUID")?;
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
            .filter(|_| claim.desktop.is_none() && claim.test_evidence.is_none())
            .ok_or("This claim has no recorded provider worktree")?;
        let registry = Registry::new(support.to_path_buf(), home.clone());
        let project = registry.validated_resolve(&reservation.project_id().to_string())?;
        let _lease = self.reserve_worktree_review(project.root.clone())?;
        if journal.exact_claim(run, node)? != claim {
            return Err("The exact claim changed; refresh it before reconciliation".into());
        }
        let manager = Manager::new(home.join(WORKTREE_FOLDER), home);
        let inspection = manager.inspect_reserved(&project, run, node, reservation)?;
        let session = claim
            .evidence_id
            .clone()
            .map(SessionId::try_from)
            .transpose()?;
        let (delivery, provider_record) = match (&session, inspection) {
            (None, _)
                if claim.dispatch == Some(Dispatch::Pending)
                    || (claim.dispatch.is_none()
                        && reservation.resolved_commit().is_none()
                        && claim.attempt == 1) =>
            {
                // Older attempts only prove the boundary before preparation.
                ("not_dispatched", "not_applicable")
            }
            (None, _) => ("uncertain", "no_exact_session"),
            (Some(id), _) if id.kind() != Kind::OpenCode => ("acknowledged", "unsupported"),
            (Some(id), Inspection::Owned { path, .. }) => {
                if lookup(&path, id).is_ok() {
                    match manager.inspect_reserved(&project, run, node, reservation)? {
                        Inspection::Owned { path: after, .. } if after == path => {}
                        _ => {
                            return Err(
                                "The owned checkout changed during provider readback".into()
                            );
                        }
                    }
                    ("acknowledged", "exact_session_observed")
                } else {
                    ("acknowledged", "unavailable")
                }
            }
            (Some(_), Inspection::Absent { .. }) => ("acknowledged", "checkout_unavailable"),
        };
        if journal.exact_claim(run, node)? != claim
            || registry.validated_resolve(&project.id.to_string())? != project
        {
            return Err("The exact claim changed; refresh it before reconciliation".into());
        }
        Ok(json!({
            "run_id": run,
            "node_id": node,
            "attempt": claim.attempt,
            "recorded_state": claim.state,
            "delivery": delivery,
            "provider_record": provider_record,
            "retry_authorized": false,
        }))
    }
}

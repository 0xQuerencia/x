//! Operator-only renewed approval of a live queued coding node.
//! A review token identifies the already-reviewed exact task, never a new goal.

use super::*;
use crate::service::State as TaskState;

/// A native control for one exact live review.
#[derive(Clone, Debug)]
pub struct PermissionWait {
    pub step_index: usize,
    pub review_token: String,
}

pub(in crate::service) fn permission_waits(state: &TaskState) -> Vec<PermissionWait> {
    let (Some(task), Some(stops)) = (&state.task, &state.active_graph_stops) else {
        return Vec::new();
    };
    if state.phase != Phase::Running || state.stop.requested() {
        return Vec::new();
    }
    task.steps
        .iter()
        .enumerate()
        .filter_map(|(step_index, step)| {
            if step.status != "waiting_for_permission" {
                return None;
            }
            Some(PermissionWait {
                step_index,
                review_token: stops.permission_review_token(step_index)?.to_string(),
            })
        })
        .collect()
}

impl Service {
    /// Read the current expired-approval requests for an exact live task.
    /// The title is the original reviewed plan step; the token is not a receipt.
    pub fn coding_permission_reviews(&self, task_id: u64) -> Result<serde_json::Value, String> {
        let target = self.service_for_task(task_id)?;
        let state = target.lock();
        let task = state
            .task
            .as_ref()
            .filter(|task| task.id == task_id && state.phase == Phase::Running)
            .ok_or("The reviewed coding graph is no longer active")?;
        state.stop.check()?;
        let reviews = permission_waits(&state)
            .into_iter()
            .map(|review| {
                let step = &task.steps[review.step_index];
                serde_json::json!({
                    "task_id": task_id,
                    "step_index": review.step_index,
                    "title": step.title,
                    "risk": step.risk,
                    "review_token": review.review_token,
                    "note": "Renew only the unchanged queued job for up to five minutes, once.",
                })
            })
            .collect::<Vec<_>>();
        Ok(serde_json::json!({"task_id": task_id, "reviews": reviews}))
    }

    /// A trusted local CLI operator invokes this with the exact visible token.
    /// It does not accept a new task, scope, receipt, provider, or operation.
    pub fn renew_coding_permission(&self, task_id: u64, raw_token: &str) -> Result<(), String> {
        let token = Uuid::parse_str(raw_token)
            .ok()
            .filter(|token| !token.is_nil())
            .ok_or("Enter the exact permission review token")?;
        let target = self.service_for_task(task_id)?;
        let mut state = target.lock();
        state.stop.check()?;
        let task = state
            .task
            .as_ref()
            .filter(|task| task.id == task_id && state.phase == Phase::Running)
            .ok_or("The reviewed coding graph is no longer active")?;
        let stops = state
            .active_graph_stops
            .as_ref()
            .ok_or("No active coding graph")?;
        let index = task
            .steps
            .iter()
            .enumerate()
            .find(|(index, step)| {
                step.status == "waiting_for_permission"
                    && stops.permission_review_token(*index) == Some(token)
            })
            .map(|(index, _)| index)
            .ok_or("No matching live queued permission review")?;
        stops.renew_permission(index, token)?;
        if let Some(step) = state
            .task
            .as_mut()
            .and_then(|task| task.steps.get_mut(index))
        {
            step.status = "pending".into();
        }
        emit(
            &mut state,
            "info",
            &format!(
                "Owner renewed the exact queued approval for job {}",
                index + 1
            ),
        );
        Ok(())
    }
}

//! Owner-reviewed fast-forward integration; no model-triggered merge paths.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::grants::{Permission, Scope};
use crate::projects::{Project, Registry};
use crate::workflows::integration::{self as merge, Preview};
use crate::worktrees::Manager;

use super::edit::{step, task};
use super::{AuditKind, Cancellation, Phase, Review, Service, Task, audit_best_effort, busy, emit};

const WORKTREE_FOLDER: &str = "X Worktrees";

pub(super) struct PendingIntegration {
    project: Project,
    run_id: Uuid,
    task_id: String,
    start_commit: String,
    expected: Preview,
    manager: Arc<Manager>,
    registry: Registry,
}

fn scope(pending: &PendingIntegration) -> Scope {
    Scope::Task {
        project_id: pending.project.id,
        run_id: pending.run_id,
        node_id: pending.task_id.clone(),
        intent: format!(
            "merge-ff:{}:{}",
            pending.expected.primary_head, pending.expected.task_head
        ),
    }
}

impl PendingIntegration {
    pub(super) fn project_root(&self) -> std::path::PathBuf {
        self.project.root.clone()
    }
}

impl Service {
    /// The exact project/run/task/start commit are supplied through a local
    /// typed command, never interpreted from speech or model output.
    pub fn plan_fast_forward(
        &self,
        project_id: &str,
        run_id: &str,
        task_id: &str,
        start_commit: &str,
    ) -> Result<Task, String> {
        if self.lock().phase.is_busy() {
            return Err(busy(self.lock().phase));
        }
        let id = Uuid::parse_str(run_id).map_err(|_| "Enter an exact X worktree run ID")?;
        let (dir, home) = self
            .workflow_dirs
            .lock()
            .map_err(|_| "Workflow storage is unavailable")?
            .clone()
            .ok_or("Workflow storage is not configured")?;
        let support = dir
            .parent()
            .ok_or("Could not locate X's local data directory")?;
        let registry = Registry::new(support.to_path_buf(), home.clone());
        let project = registry.validated_resolve(project_id)?;
        let base: PathBuf = home.join(WORKTREE_FOLDER);
        let manager = Arc::new(Manager::new(base, home));
        let worktree = manager.reattach(&project, id, task_id, start_commit)?;
        let preview = merge::preview(&manager, &project, &worktree)?;
        if !merge::eligible_fast_forward(&project, &preview)? {
            return Err(
                "Only a clean, conflict-free fast-forward with no unrelated owner changes can be approved".into()
            );
        }
        let description = format!(
            "Fast-forward {} from {} to {}",
            project.name, preview.primary_head, preview.task_head,
        );
        let steps = vec![step(
            &description,
            "updates the primary Git checkout; irreversible by X; no automatic conflict handling",
        )];
        let mut state = self.lock();
        if state.phase.is_busy() {
            return Err(busy(state.phase));
        }
        state.serial += 1;
        let mut plan = task(state.serial, &description, steps, None);
        plan.changed_files = Some(preview.changed_files.clone());
        plan.phase = Phase::AwaitingApproval;
        state.phase = Phase::AwaitingApproval;
        state.review = Review::Required;
        state.actions.clear();
        state.guard = None;
        state.grants.clear();
        state.edit = None;
        state.workflow = None;
        state.graph_workflow = None;
        state.pending_tests = None;
        state.project_command = None;
        state.create_workspace = false;
        state.pending_integration = Some(PendingIntegration {
            project,
            run_id: id,
            task_id: task_id.into(),
            start_commit: start_commit.into(),
            expected: preview,
            manager,
            registry,
        });
        state.task = Some(plan.clone());
        audit_best_effort(&mut state, AuditKind::Planned, None);
        emit(&mut state, "plan", &description);
        Ok(plan)
    }

    pub(super) fn run_fast_forward(&self, pending: PendingIntegration, stop: &Cancellation) {
        self.set_step(0, "running");
        let result = (|| {
            stop.check()?;
            let current = pending
                .registry
                .validated_resolve(&pending.project.id.to_string())?;
            if current.root != pending.project.root
                || current.git_common_dir != pending.project.git_common_dir
            {
                return Err("The registered project identity changed".into());
            }
            let worktree = pending.manager.reattach(
                &current,
                pending.run_id,
                &pending.task_id,
                &pending.start_commit,
            )?;
            stop.check()?;
            let grants = self.scoped_grants();
            let exact_scope = scope(&pending);
            let receipt = grants.issue(
                Permission::IntegrateFastForward,
                exact_scope.clone(),
                Duration::from_secs(60),
                1,
                true,
            )?;
            if stop.requested() {
                let _ = grants.revoke(&receipt);
                return Err("Stopped before fast-forward".into());
            }
            grants.consume(&receipt, Permission::IntegrateFastForward, &exact_scope)?;
            stop.check()?;
            // A stale commit, dirty checkout or conflict refuses the write.
            // Git may take a moment to update the index; Stop cannot safely
            // interrupt an in-flight merge without risking a half-written tree.
            merge::apply_fast_forward(
                &pending.manager,
                &current,
                &worktree,
                &pending.expected,
                true,
            )
        })();
        match result {
            Ok(proof) => {
                self.set_step(0, "done");
                self.finish(
                    Phase::Completed,
                    None,
                    Some(format!(
                        "Primary checkout advanced to {} ({} changed paths). No push performed.",
                        proof.task_head,
                        proof.changed_files.len()
                    )),
                    Some(proof.changed_files),
                );
            }
            Err(_) if stop.requested() => {
                self.set_step(0, "cancelled");
                self.finish(
                    Phase::Cancelled,
                    None,
                    Some("Stopped before merge".into()),
                    None,
                );
            }
            Err(error) => {
                self.set_step(0, "failed");
                self.finish(Phase::Failed, Some(error), None, None);
            }
        }
    }
}

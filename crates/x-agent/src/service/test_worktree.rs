//! Native approval bridge for fixed, isolated project test presets.

use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::grants::Permission;
use crate::projects::{Project, Registry};
use crate::verification::Outcome;
use crate::verification::test_runner::{Suite, scope_for_suite, verify_approved};
use crate::worktrees::Manager;

use super::edit::{step, task};
use super::{AuditKind, Cancellation, Phase, Review, Service, Task, audit_best_effort, busy, emit};

const WORKTREE_FOLDER: &str = "X Worktrees";

pub(super) struct PendingTests {
    project: Project,
    commit: String,
    suite: Suite,
    manager: Arc<Manager>,
    base: std::path::PathBuf,
    registry: Registry,
}

impl PendingTests {
    pub(super) fn project_root(&self) -> std::path::PathBuf {
        self.project.root.clone()
    }
}

impl Service {
    /// Only the typed local control command can request this plan. Approval
    /// is always a separate explicit owner interaction, even in Autonomous.
    pub fn plan_worktree_tests(&self, project_id: &str, suite: Suite) -> Result<Task, String> {
        if self.lock().phase.is_busy() {
            return Err(busy(self.lock().phase));
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
        let registry = Registry::new(support.to_path_buf(), home.clone());
        let project = registry.validated_resolve(project_id)?;
        let base = home.join(WORKTREE_FOLDER);
        let manager = Arc::new(Manager::new(base.clone(), home));
        let commit = manager.starting_commit(&project)?;
        let (program, args) = suite.command();
        let command = format!("{program} {}", args.join(" "));
        let label = format!("Test {} at {}: {command}", project.name, commit);
        let steps = vec![step(
            &label,
            "runs project scripts with user permissions; not sandboxed; approval required",
        )];
        let mut state = self.lock();
        if state.phase.is_busy() {
            return Err(busy(state.phase));
        }
        state.serial += 1;
        let mut preview = task(state.serial, &label, steps, None);
        preview.phase = Phase::AwaitingApproval;
        state.phase = Phase::AwaitingApproval;
        state.review = Review::Required;
        state.actions.clear();
        state.guard = None;
        state.grants.clear();
        state.edit = None;
        state.workflow = None;
        state.graph_workflow = None;
        state.project_command = None;
        state.create_workspace = false;
        state.pending_tests = Some(PendingTests {
            project,
            commit,
            suite,
            manager,
            base,
            registry,
        });
        state.task = Some(preview.clone());
        audit_best_effort(&mut state, AuditKind::Planned, None);
        emit(&mut state, "plan", &label);
        Ok(preview)
    }

    pub(super) fn run_worktree_tests(&self, pending: PendingTests, stop: &Cancellation) {
        self.set_step(0, "running");
        let result = (|| {
            stop.check()?;
            let current = pending
                .registry
                .validated_resolve(&pending.project.id.to_string())?;
            if current.root != pending.project.root
                || current.git_common_dir != pending.project.git_common_dir
                || pending.manager.starting_commit(&current)? != pending.commit
            {
                return Err("Project identity or commit changed since test approval".into());
            }
            std::fs::create_dir_all(&pending.base)
                .map_err(|_| "Cannot create the isolated X worktree directory")?;
            stop.check()?;
            let tree = pending.manager.allocate(
                &pending.project,
                Uuid::new_v4(),
                "test-suite",
                &pending.commit,
                true,
            )?;
            self.emit(
                "workflow",
                &format!("Isolated test checkout: {}", tree.path().display()),
            );
            stop.check()?;
            let grants = self.scoped_grants();
            let receipt = grants.issue(
                Permission::RunTests,
                scope_for_suite(&pending.project, &tree, pending.suite),
                Duration::from_secs(60),
                1,
                true,
            )?;
            if stop.requested() {
                let _ = grants.revoke(&receipt);
                return Err("Stopped before starting tests".into());
            }
            let outcome = verify_approved(
                &pending.manager,
                &pending.project,
                &tree,
                pending.suite,
                &grants,
                &receipt,
                stop,
            )?;
            Ok((outcome.outcome, tree.path().display().to_string()))
        })();
        match result {
            Ok((Outcome::Verified, path)) if !stop.requested() => {
                self.set_step(0, "done");
                self.finish(
                    Phase::Completed,
                    None,
                    Some(format!(
                        "Test process passed; inspect isolated checkout at {path}"
                    )),
                    None,
                );
            }
            Ok((_, path)) if stop.requested() => {
                self.set_step(0, "cancelled");
                self.finish(
                    Phase::Cancelled,
                    None,
                    Some(format!("Stopped. The isolated checkout remains at {path}")),
                    None,
                );
            }
            Ok((outcome, path)) => {
                self.set_step(0, "failed");
                self.finish(
                    Phase::Failed,
                    Some(format!("Test result {outcome:?}; inspect {path}")),
                    None,
                    None,
                );
            }
            Err(_) if stop.requested() => {
                self.set_step(0, "cancelled");
                self.finish(Phase::Cancelled, None, Some("Stopped".into()), None);
            }
            Err(error) => {
                self.set_step(0, "failed");
                self.finish(Phase::Failed, Some(error), None, None);
            }
        }
    }
}

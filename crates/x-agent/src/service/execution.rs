//! Approval consumption and foreground action execution.

use super::*;

impl Service {
    // ---- execution ---------------------------------------------------------------------

    pub fn approve(&self, task_id: u64) -> Result<(), String> {
        self.approve_from(task_id, Approval::User)
    }

    pub(super) fn approve_if_automatic(&self, task_id: u64) -> Result<(), String> {
        self.approve_from(task_id, Approval::Automatic)
    }

    pub(super) fn approve_from(&self, task_id: u64, approval: Approval) -> Result<(), String> {
        self.approve_with_window(task_id, approval, None)
    }

    pub(super) fn approve_with_window(
        &self,
        task_id: u64,
        approval: Approval,
        question_id: Option<&str>,
    ) -> Result<(), String> {
        let (
            task,
            actions,
            is_edit,
            is_workflow,
            graph,
            tests,
            integration,
            project_command,
            artifact_review,
            stop,
            lease,
        ) = {
            let mut s = self.lock();
            // Check under the same lock as approval consumption. A delayed
            // initial-task click cannot race a new clarification into approval.
            if s.computer_dialogue.is_some() {
                return match approval {
                    Approval::User => Err(
                        "Review the answered computer question with its current question token"
                            .into(),
                    ),
                    Approval::Automatic => Ok(()),
                };
            }
            match (&s.window_dialogue, question_id) {
                (Some(pending), Some(question_id)) => pending.approved(&s, task_id, question_id)?,
                (Some(_), None) => {
                    return match approval {
                        Approval::User => Err(
                            "Choose and review the window with its current task and question token"
                                .into(),
                        ),
                        Approval::Automatic => Ok(()),
                    };
                }
                (None, Some(_)) => return Err("That window question is no longer current".into()),
                (None, None) => {}
            }
            let matches_task = s.task.as_ref().is_some_and(|t| t.id == task_id);
            if !matches_task || s.phase != Phase::AwaitingApproval {
                return match approval {
                    Approval::Automatic => Ok(()),
                    Approval::User => Err("No matching task awaiting approval".into()),
                };
            }
            if !s.review.allows(s.autonomy_mode, approval) {
                return Ok(());
            }
            let lease = self.reserve_work(&s, task_id)?;
            let kind = match approval {
                Approval::User => AuditKind::Approved,
                Approval::Automatic => AuditKind::Automatic,
            };
            if let Err(error) = audit(&s, kind, None) {
                emit(&mut s, "error", &error);
                return Err(error);
            }
            if approval == Approval::Automatic {
                if let Some(guard) = s.guard.as_mut() {
                    guard.strict_focus = true;
                }
                emit(&mut s, "approval", &format!("automatic task {task_id}"));
            } else {
                emit(&mut s, "info", "Approved");
                emit(&mut s, "approval", &format!("approved task {task_id}"));
            }
            set_phase(&mut s, Phase::Running);
            s.window_dialogue = None;
            s.stop = Cancellation::default();
            s.active_graph_stops = s
                .graph_workflow
                .as_ref()
                .map(|graph| graph.job_stops(&s.stop));
            (
                s.task.clone().ok_or("no task")?,
                s.actions.clone(),
                s.edit.is_some(),
                s.workflow.is_some(),
                s.graph_workflow.take(),
                s.pending_tests.take(),
                s.pending_integration.take(),
                s.project_command.take(),
                s.artifact_review.take(),
                s.stop.clone(),
                lease,
            )
        };
        self.spawn_owned(task_id, lease, move |me| {
            if let Some(graph) = graph {
                return me.run_graph_workflow(graph, &stop);
            }
            if let Some(tests) = tests {
                return me.run_worktree_tests(tests, &stop);
            }
            if let Some(integration) = integration {
                return me.run_fast_forward(integration, &stop);
            }
            if let Some(command) = project_command {
                return me.run_project_command(command, &stop);
            }
            let _desktop = if task.mode == Mode::Actions && !is_workflow {
                match me.acquire_desktop(task.id, &stop, std::time::Duration::from_secs(15)) {
                    Ok(lease) => Some(lease),
                    Err(why) => {
                        let phase = if stop.requested() {
                            Phase::Cancelled
                        } else {
                            Phase::Failed
                        };
                        return me.finish(phase, Some(why), None, None);
                    }
                }
            } else {
                None
            };
            match task.mode {
                Mode::Actions if is_edit => me.run_edit(&task),
                Mode::Actions if is_workflow => me.run_workflow(),
                Mode::Actions => me.run_actions(&actions, &stop),
                Mode::Work => me.run_work(&task, &stop, artifact_review),
            }
        })
    }

    /// Allows one command OpenCode was refused, for one rerun of the same goal. The rerun
    /// is planned like any other Work task: it still waits for approval.
    pub fn allow_once(&self, index: usize) -> Result<Task, String> {
        let (text, command, session) = {
            let s = self.lock();
            if s.phase.is_busy() {
                return Err(busy(s.phase));
            }
            let task = s
                .task
                .as_ref()
                .filter(|t| t.mode == Mode::Work)
                .ok_or("No Work task to rerun")?;
            if task.work_harness == Some(harness::Kind::ClaudeCode) {
                return Err("Claude Code Work in X does not accept command grants".into());
            }
            if task.work_access == harness::WorkAccess::SelectionOnly {
                return Err("Sharing a selection does not allow command grants".into());
            }
            let command = task
                .blocked
                .get(index)
                .ok_or("No such blocked command")?
                .clone();
            let workspace = s.workspace.as_ref().map(|p| p.display().to_string());
            if task.workspace != workspace {
                return Err("Select the original project before rerunning that task".into());
            }
            (task.text.clone(), command, task.session.clone())
        };
        if command.contains(['*', '?', '\n']) {
            return Err("That command contains wildcards and cannot be allowed".into());
        }
        let task = self.plan_task(
            &text,
            Mode::Work,
            None,
            PlanOptions {
                allow_command: Some(command.clone()),
                session,
                ..PlanOptions::default()
            },
        )?;
        self.emit("info", &format!("Allowing `{command}` for this rerun only"));
        Ok(task)
    }

    pub(super) fn claim_action(&self, id: ActionId) -> Result<bool, String> {
        let mut state = self.lock();
        if state.stop.requested() || state.phase != Phase::Running {
            return Ok(false);
        }
        let Some(step) = state
            .task
            .as_ref()
            .filter(|task| task.id == id.task)
            .and_then(|task| task.steps.get(id.step))
            .filter(|step| step.status == "pending")
        else {
            return Ok(false);
        };
        if step.request.as_ref().map(|request| request.idempotency_key) != Some(id) {
            return Ok(false);
        }
        if state
            .actions
            .get(id.step)
            .and_then(|request| request.reference_expires)
            .is_some_and(|expires| Instant::now() >= expires)
        {
            return Err(
                "The reviewed window reference expired before dispatch; repeat the request".into(),
            );
        }
        audit(&state, AuditKind::Claimed, Some(id.step))?;
        if let Some(task) = state.task.as_mut() {
            task.steps[id.step].status = "running".into();
        }
        Ok(true)
    }

    pub(super) fn recheck_guard(&self) -> Result<(), String> {
        let guard = self.lock().guard.take();
        if let Some(guard) = guard {
            if guard.browser_share {
                let window = guard
                    .ctx
                    .window_ref()
                    .ok_or("Browser window is unavailable")?;
                self.check_browser_share(window)?;
            }
            self.context_engine()
                .recheck(&guard.ctx, guard.strict_focus)
                .map_err(|why| why.to_string())?;
        }
        Ok(())
    }

    pub(super) fn run_actions(&self, actions: &[ActionRequest], stop: &Cancellation) {
        self.run_actions_from(actions, 0, stop)
    }

    pub(super) fn run_actions_from(
        &self,
        actions: &[ActionRequest],
        start: usize,
        stop: &Cancellation,
    ) {
        if let Err(why) = self.recheck_guard() {
            self.set_step(0, "failed");
            return self.finish(Phase::Failed, Some(format!("Stopped: {why}")), None, None);
        }
        for request in actions.iter().skip(start) {
            let id = request.metadata.idempotency_key;
            match self.claim_action(id) {
                Ok(true) => {}
                Ok(false) => {
                    if stop.requested() {
                        self.finish(Phase::Cancelled, None, Some("Stopped".into()), None);
                    }
                    return;
                }
                Err(error) => {
                    self.set_step(id.step, "failed");
                    self.finish(Phase::Failed, Some(error), None, None);
                    return;
                }
            }
            let i = id.step;
            let action = &request.action;
            self.emit("action", &action.describe());
            if let Action::Computer { app, goal } = action {
                match self.run_computer(app, goal, stop.child()) {
                    crate::computer::Outcome::Observed(note) => {
                        self.record_execution(
                            i,
                            tools::ExecutionOutcome::observed(
                                note.clone(),
                                tools::EffectEvidence::AccessibilityObservation,
                            ),
                        );
                        if actions.len() == 1 {
                            return self.finish(
                                Phase::Completed,
                                None,
                                Some(format!(
                                    "{note}. The requested goal was not independently verified."
                                )),
                                None,
                            );
                        }
                    }
                    crate::computer::Outcome::Waiting(continuation) => {
                        if let Err(why) =
                            self.pause_computer(request.metadata.idempotency_key, *continuation)
                        {
                            if stop.requested() {
                                self.set_step(i, "cancelled");
                                self.finish(Phase::Cancelled, None, None, None);
                            } else {
                                self.set_step(i, "failed");
                                self.finish(Phase::Failed, Some(why), None, None);
                            }
                        }
                        return;
                    }
                    crate::computer::Outcome::Cancelled => {
                        self.set_step(i, "cancelled");
                        return self.finish(Phase::Cancelled, None, Some("Stopped".into()), None);
                    }
                    crate::computer::Outcome::Failed(why) => {
                        self.record_execution(i, tools::ExecutionOutcome::failed(&why));
                        return self.finish(Phase::Failed, Some(why), None, None);
                    }
                }
                continue;
            }
            let execution_started = Instant::now();
            let result = match action {
                Action::Type { .. } => {
                    let waited = match actions.get(i.wrapping_sub(1)) {
                        Some(launched) if launched.action.launches_window() => {
                            self.launcher.await_launched_focus(stop)
                        }
                        _ => Ok(()),
                    };
                    waited.and_then(|()| self.launcher.execute_prepared(action, None, stop))
                }
                _ => self
                    .launcher
                    .execute_prepared(action, request.close_target.as_ref(), stop),
            };
            // Window Controller already waits for compositor verification;
            // ordinary launches are timed as execution, not verification.
            let phase = if matches!(action, Action::Window { .. }) {
                "act_verify"
            } else {
                "act"
            };
            self.emit(
                "timing",
                &format!(
                    "phase={phase} step={} elapsed_ms={}",
                    i + 1,
                    execution_started.elapsed().as_millis()
                ),
            );
            if stop.requested() && result.is_err() {
                return self.finish(Phase::Cancelled, None, Some("Stopped".into()), None);
            }
            match result {
                Ok(outcome) => {
                    let message = outcome.message.clone();
                    match self.record_execution(i, outcome) {
                        tools::EffectState::Unverified | tools::EffectState::Failed => {
                            return self.finish(
                                Phase::Failed,
                                Some(format!("Action effect was not confirmed: {message}")),
                                None,
                                None,
                            );
                        }
                        tools::EffectState::Launched
                        | tools::EffectState::Observed
                        | tools::EffectState::Confirmed => {}
                    }
                }
                Err(e) => {
                    self.record_execution(i, tools::ExecutionOutcome::failed(&e));
                    return self.finish(Phase::Failed, Some(format!("Stopped: {e}")), None, None);
                }
            }
        }
        let note = self.action_summary();
        self.finish(Phase::Completed, None, Some(note), None);
    }

    pub(super) fn run_computer(
        &self,
        app: &str,
        goal: &str,
        stop: Cancellation,
    ) -> crate::computer::Outcome {
        self.computer_session(app, goal, stop, None)
    }
}

//! In-memory, single-use answers for a paused Computer step. Nothing here turns
//! the answer into a new action plan or replays an already claimed action.

use std::sync::Arc;

use serde::Serialize;

use super::{
    Action, ActionId, AuditKind, Cancellation, Phase, Planner, Review, Service, Task, audit,
    audit_best_effort, emit, set_phase,
};
use crate::computer::{self, Continuation, Outcome};

/// The current question, and (after answering) the exact detail awaiting review.
/// Screen observations, previous actions and execution authority stay private.
#[derive(Clone, Debug, Serialize)]
pub struct ComputerQuestion {
    pub task_id: u64,
    pub step: usize,
    pub question_id: String,
    pub prompt: String,
    /// Monotonic time remaining in the original task's deadline, at this snapshot.
    pub expires_in_ms: u64,
    pub pending_answer: Option<String>,
}

pub(super) struct PendingComputer {
    id: ActionId,
    question_id: String,
    continuation: Continuation,
    planner: Arc<dyn Planner>,
    answer: Option<String>,
}

impl PendingComputer {
    pub(super) fn question_token(&self) -> String {
        self.question_id.clone()
    }

    pub(super) fn preview(&self) -> ComputerQuestion {
        ComputerQuestion {
            task_id: self.id.task,
            step: self.id.step,
            question_id: self.question_id.clone(),
            prompt: self.continuation.question().into(),
            expires_in_ms: self
                .continuation
                .remaining()
                .as_millis()
                .min(u64::MAX as u128) as u64,
            pending_answer: self.answer.clone(),
        }
    }
}

type Resume = (Continuation, String, Arc<dyn Planner>);

impl Service {
    pub(super) fn pause_computer(
        &self,
        id: ActionId,
        continuation: Continuation,
    ) -> Result<(), String> {
        continuation.check_live()?;
        let planner = self
            .planner
            .lock()
            .ok()
            .and_then(|p| p.clone())
            .ok_or("No model is set")?;
        let mut state = self.lock();
        if state.stop.requested() {
            return Err("Stopped".into());
        }
        let request = state
            .actions
            .get(id.step)
            .ok_or("The computer step is no longer current")?;
        if state.phase != Phase::Running
            || request.metadata.idempotency_key != id
            || !matches!(&request.action, Action::Computer { app, goal }
                if app == continuation.app() && goal == continuation.goal())
            || !state.task.as_ref().is_some_and(|task| {
                task.id == id.task
                    && task
                        .steps
                        .get(id.step)
                        .is_some_and(|step| step.status == "running")
            })
            || state.computer_dialogue.is_some()
        {
            return Err("The computer step is no longer current".into());
        }
        state.computer_dialogue = Some(PendingComputer {
            id,
            question_id: uuid::Uuid::new_v4().to_string(),
            continuation,
            planner,
            answer: None,
        });
        let task = state.task.as_mut().ok_or("No computer task")?;
        task.steps[id.step].status = "waiting_input".into();
        task.steps[id.step].detail =
            "Answer the pending question, then review before resuming this step".into();
        state.review = Review::Required;
        set_phase(&mut state, Phase::WaitingForInput);
        emit(&mut state, "info", "Computer task paused for a user answer");
        Ok(())
    }

    /// Answer exactly one live question. The answer changes no grants and does
    /// not run a decision: it creates a mandatory review of the remaining step.
    pub fn answer_computer(
        &self,
        task_id: u64,
        question_id: &str,
        answer: &str,
    ) -> Result<Task, String> {
        let answer = computer::check_answer(answer)?;
        let mut state = self.lock();
        if state.phase != Phase::WaitingForInput || state.stop.requested() {
            return Err("No computer question is waiting for an answer".into());
        }
        let pending = state
            .computer_dialogue
            .as_mut()
            .ok_or("No computer question is waiting")?;
        if pending.id.task != task_id || pending.question_id != question_id {
            return Err("That answer does not match the current task and question".into());
        }
        pending.continuation.check_live()?;
        if pending.answer.is_some() {
            return Err("This question already has an answer awaiting review".into());
        }
        let step = pending.id.step;
        let question = pending.continuation.question().to_string();
        pending.answer = Some(answer.clone());
        let task = state
            .task
            .as_mut()
            .filter(|task| task.id == task_id)
            .ok_or("No matching task")?;
        task.steps[step].status = "waiting_review".into();
        task.steps[step].detail = format!(
            "Question: {question}\nYour answer: {answer}\nReview to continue this same app and step with the remaining budget."
        );
        state.review = Review::Required;
        set_phase(&mut state, Phase::AwaitingApproval);
        emit(
            &mut state,
            "info",
            "Answer received; review is required before the computer task resumes",
        );
        state.task.clone().ok_or_else(|| "No matching task".into())
    }

    /// Explicit approval of this exact answered question, never automatic
    /// policy or a delayed approval of the initial task. Retain
    /// the original cancellation token and acquire the task's execution lease
    /// before taking its continuation, so overlapping workers cannot resume it.
    pub fn approve_computer_answer(&self, task_id: u64, question_id: &str) -> Result<(), String> {
        let (pending, actions, stop, lease) = {
            let mut state = self.lock();
            if state.phase != Phase::AwaitingApproval || state.stop.requested() {
                return Err("No answered computer question is awaiting review".into());
            }
            let pending = state
                .computer_dialogue
                .as_ref()
                .ok_or("No computer question is waiting")?;
            if pending.id.task != task_id
                || pending.question_id != question_id
                || state.task.as_ref().is_none_or(|task| task.id != task_id)
                || pending.answer.is_none()
            {
                return Err(
                    "No matching answered computer question; review the current question token"
                        .into(),
                );
            }
            pending.continuation.check_live()?;
            let lease = self.reserve_work(&state, task_id)?;
            audit(&state, AuditKind::Approved, Some(pending.id.step))?;
            let pending = state
                .computer_dialogue
                .take()
                .ok_or("No computer continuation")?;
            if let Some(task) = state.task.as_mut() {
                task.steps[pending.id.step].status = "running".into();
                // The raw answer is intentionally absent from event/audit history.
                task.steps[pending.id.step].detail = "Resuming after the reviewed answer".into();
            }
            set_phase(&mut state, Phase::Running);
            emit(
                &mut state,
                "approval",
                &format!("approved continuation for task {task_id}"),
            );
            (pending, state.actions.clone(), state.stop.clone(), lease)
        };
        self.spawn_owned(task_id, lease, move |service| {
            let timeout = pending
                .continuation
                .remaining()
                .min(std::time::Duration::from_secs(15));
            let _desktop = match service.acquire_desktop(task_id, &stop, timeout) {
                Ok(lease) => lease,
                Err(why) => return service.computer_resume_failed(pending.id.step, why, &stop),
            };
            let PendingComputer {
                id,
                continuation,
                planner,
                answer,
                ..
            } = pending;
            let app = continuation.app().to_string();
            let goal = continuation.goal().to_string();
            let outcome = service.computer_session(
                &app,
                &goal,
                stop.child(),
                Some((continuation, answer.expect("reviewed answer"), planner)),
            );
            match outcome {
                Outcome::Observed(note) => {
                    service.record_execution(
                        id.step,
                        crate::tools::ExecutionOutcome::observed(
                            note,
                            crate::tools::EffectEvidence::AccessibilityObservation,
                        ),
                    );
                    service.run_actions_from(&actions, id.step + 1, &stop);
                }
                Outcome::Waiting(continuation) => {
                    if let Err(why) = service.pause_computer(id, *continuation) {
                        service.computer_resume_failed(id.step, why, &stop);
                    }
                }
                Outcome::Failed(why) => service.computer_resume_failed(id.step, why, &stop),
                Outcome::Cancelled => {
                    service.set_step(id.step, "cancelled");
                    service.finish(Phase::Cancelled, None, None, None);
                }
            }
        })
    }

    fn computer_resume_failed(&self, step: usize, why: String, stop: &Cancellation) {
        if stop.requested() {
            self.set_step(step, "cancelled");
            self.finish(Phase::Cancelled, None, None, None);
        } else {
            self.record_execution(step, crate::tools::ExecutionOutcome::failed(&why));
            self.finish(Phase::Failed, Some(why), None, None);
        }
    }

    /// Snapshots can retire an expired paused task without a background timer.
    /// The monotonic check at answer, approval and execution also fails closed
    /// when no snapshot has been requested since the deadline elapsed.
    pub(super) fn expire_computer_question(&self) {
        let mut state = self.lock();
        if !matches!(
            state.phase,
            Phase::WaitingForInput | Phase::AwaitingApproval
        ) {
            return;
        }
        let Some((id, why)) = state.computer_dialogue.as_ref().and_then(|pending| {
            pending
                .continuation
                .check_live()
                .err()
                .map(|why| (pending.id, why))
        }) else {
            return;
        };
        state.computer_dialogue = None;
        if let Some(task) = state.task.as_mut().filter(|task| task.id == id.task) {
            task.steps[id.step].status = "failed".into();
            task.steps[id.step].detail = why.clone();
            task.error = Some(why.clone());
        }
        set_phase(&mut state, Phase::Failed);
        audit_best_effort(&mut state, AuditKind::StepFailed, Some(id.step));
        audit_best_effort(&mut state, AuditKind::Finished, None);
        emit(&mut state, "error", &why);
    }

    pub(super) fn computer_session(
        &self,
        app: &str,
        goal: &str,
        stop: Cancellation,
        resume: Option<Resume>,
    ) -> Outcome {
        struct Link<'a>(&'a Service, Cancellation);
        impl computer::Host for Link<'_> {
            fn emit(&self, kind: &str, text: &str) {
                self.0.emit(kind, text);
            }
            fn stopped(&self) -> bool {
                self.1.requested()
            }
            fn cancellation(&self) -> Option<&Cancellation> {
                Some(&self.1)
            }
            fn open_app(&self, app: &str) -> Result<(), String> {
                self.0
                    .launcher
                    .launch_prepared(&Action::App { target: app.into() }, None, &self.1)
                    .map(drop)
            }
        }
        if stop.requested() {
            return Outcome::Cancelled;
        }
        let planner = resume
            .as_ref()
            .map(|(_, _, planner)| Arc::clone(planner))
            .or_else(|| self.planner.lock().ok().and_then(|p| p.clone()));
        let Some(planner) = planner else {
            return Outcome::Failed("No model is set".into());
        };
        if let Err(why) = self.screen_decision_permission(&planner, app) {
            return Outcome::Failed(why);
        }
        let desktop = self
            .desktop
            .lock()
            .map(|d| d.clone())
            .unwrap_or_else(|p| p.into_inner().clone());
        let decide = |goal: &str,
                      app: &str,
                      observation: &crate::desktop::Observation,
                      history: &[String]| {
            let vision_allowed = self.screen_decision_permission(&planner, app)?;
            let result = if let Some(decision) =
                planner.decide_computer(goal, app, observation, history, vision_allowed)
            {
                decision
            } else {
                self.screen_decision_permission(&planner, app)?;
                planner
                    .complete(&computer::prompt(goal, app, observation, history))
                    .and_then(|response| computer::parse(&response))
            };
            // A model or consent change while the request was in flight also
            // invalidates its returned action, evidence or clarification.
            self.screen_decision_permission(&planner, app)?;
            result
        };
        let host = Link(self, stop);
        match resume {
            Some((continuation, answer, _)) => {
                if continuation.app() != app || continuation.goal() != goal {
                    return Outcome::Failed(
                        "The computer continuation no longer matches its reviewed action".into(),
                    );
                }
                computer::resume_with(continuation, &answer, desktop.as_ref(), &decide, &host)
            }
            None => computer::run_with(
                app,
                goal,
                desktop.as_ref(),
                &decide,
                &host,
                &computer::Limits::default(),
            ),
        }
    }
}

#[cfg(test)]
mod tests;

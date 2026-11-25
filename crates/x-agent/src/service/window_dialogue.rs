//! Deterministic, task-scoped choices for an unchanged native window operation.
//! Answers select only an identity from the displayed list and always produce
//! a new review token. No model sees the answer and no mutation happens here.

use super::*;
pub use crate::context::references::WindowChoice;
use crate::context::references::WindowReferences;

#[derive(Clone, Debug, Serialize)]
pub struct WindowQuestion {
    pub task_id: u64,
    pub step: usize,
    pub question_id: String,
    pub operation: String,
    pub expires_in_ms: u64,
    pub choices: Vec<WindowChoice>,
    pub selected: Option<usize>,
    pub previous: Option<usize>,
}

pub(super) struct PendingWindow {
    id: ActionId,
    question_id: String,
    action: Action,
    references: WindowReferences,
}

impl PendingWindow {
    pub(super) fn question_token(&self) -> String {
        self.question_id.clone()
    }

    pub(super) fn from_requests(requests: &[ActionRequest]) -> Result<Option<Self>, String> {
        let [request] = requests else {
            return Ok(None);
        };
        let Some(crate::wm::WindowTarget::Choose(choices)) = &request.close_target else {
            return Ok(None);
        };
        Ok(Some(Self {
            id: request.metadata.idempotency_key,
            question_id: uuid::Uuid::new_v4().to_string(),
            action: request.action.clone(),
            references: WindowReferences::new(request.metadata.idempotency_key.task, choices)?,
        }))
    }

    fn check(&self, state: &State, task_id: u64, question_id: &str) -> Result<(), String> {
        if self.id.task != task_id
            || self.question_id != question_id
            || !matches!(
                state.phase,
                Phase::WaitingForInput | Phase::AwaitingApproval
            )
            || state.task.as_ref().is_none_or(|task| task.id != task_id)
            || state.actions.get(self.id.step).is_none_or(|request| {
                request.metadata.idempotency_key != self.id || request.action != self.action
            })
        {
            return Err("That answer does not match the current task and window list".into());
        }
        self.references.check(task_id)
    }

    pub(super) fn approved(
        &self,
        state: &State,
        task_id: u64,
        question_id: &str,
    ) -> Result<(), String> {
        self.check(state, task_id, question_id)?;
        if state.phase != Phase::AwaitingApproval || self.references.selected().is_none() {
            return Err("Select one displayed window before reviewing the operation".into());
        }
        Ok(())
    }

    pub(super) fn preview(&self) -> WindowQuestion {
        WindowQuestion {
            task_id: self.id.task,
            step: self.id.step,
            question_id: self.question_id.clone(),
            operation: self.action.describe(),
            expires_in_ms: self
                .references
                .remaining()
                .as_millis()
                .min(u64::MAX as u128) as u64,
            choices: self.references.choices().to_vec(),
            selected: self.references.selected(),
            previous: self.references.previous(),
        }
    }
}

impl Service {
    /// Interpret only the bounded reference grammar against the current
    /// displayed foreground list. This creates a review, never authority.
    pub fn answer_current_window_reference(&self, text: &str) -> Option<Result<Task, String>> {
        let text = tools::strip_wake_word(text);
        let command = text
            .trim()
            .trim_end_matches(['.', '!', '?'])
            .to_ascii_lowercase();
        let cancel = matches!(command.as_str(), "cancel this one" | "cancel this choice");
        if !cancel && !crate::context::references::is_window_reference(&text) {
            return None;
        }
        let (question, mut task) = {
            let state = self.lock();
            (
                state.window_dialogue.as_ref()?.preview(),
                state.task.clone()?,
            )
        };
        Some(if cancel {
            self.cancel_window_question(question.task_id, &question.question_id)
                .map(|()| {
                    task.phase = Phase::Cancelled;
                    task
                })
        } else {
            self.answer_window(question.task_id, &question.question_id, &text)
        })
    }

    /// A bounded deterministic selector, not a new instruction or implicit approval.
    /// Current/previous/other refer only to explicit selections in this same list.
    pub fn answer_window(
        &self,
        task_id: u64,
        question_id: &str,
        answer: &str,
    ) -> Result<Task, String> {
        let (index, reference, step) = {
            let state = self.lock();
            let pending = state
                .window_dialogue
                .as_ref()
                .ok_or("No task window choice is waiting")?;
            pending.check(&state, task_id, question_id)?;
            let (index, reference) = pending.references.resolve(task_id, answer)?;
            (index, reference, pending.id.step)
        };
        // Read outside State; Stop/task replacement can proceed while a native
        // observation is blocked. Recheck the exact token after it returns.
        let mut target = self.launcher.prepare_window_choice(&reference)?;
        if target.identity().as_ref() != Some(&reference) {
            return Err("The desktop returned a different window identity".into());
        }
        let mut state = self.lock();
        let pending = state
            .window_dialogue
            .as_ref()
            .ok_or("The window question is no longer current")?;
        pending.check(&state, task_id, question_id)?;
        let pending = state.window_dialogue.as_mut().ok_or("No window question")?;
        pending.references.select(task_id, index)?;
        pending.question_id = uuid::Uuid::new_v4().to_string();
        let expires = pending.references.expires_at();
        target.bound_until(expires);
        let request = state
            .actions
            .get_mut(step)
            .ok_or("The window step disappeared")?;
        request.close_target = Some(target);
        request.reference_expires = Some(expires);
        let title = request.title();
        let task = state.task.as_mut().ok_or("The window task disappeared")?;
        task.steps[step].title = title;
        task.steps[step].status = "pending".into();
        task.steps[step].detail =
            "Review this exact window before the unchanged operation runs".into();
        state.review = Review::Required;
        set_phase(&mut state, Phase::AwaitingApproval);
        emit(
            &mut state,
            "info",
            "Window selected; review the exact target before execution",
        );
        state
            .task
            .clone()
            .ok_or_else(|| "The window task disappeared".into())
    }

    pub fn approve_window_answer(&self, task_id: u64, question_id: &str) -> Result<(), String> {
        self.approve_with_window(task_id, Approval::User, Some(question_id))
    }

    /// Cancel this pending question atomically. It cannot stop a replacement
    /// foreground task or any independent background run.
    pub fn cancel_window_question(&self, task_id: u64, question_id: &str) -> Result<(), String> {
        let mut state = self.lock();
        let pending = state
            .window_dialogue
            .as_ref()
            .ok_or("No task window choice is waiting")?;
        pending.check(&state, task_id, question_id)?;
        state.stop.request();
        state.window_dialogue = None;
        state.actions.clear();
        state.guard = None;
        state.grants.clear();
        set_phase(&mut state, Phase::Cancelled);
        audit_best_effort(&mut state, AuditKind::Discarded, None);
        emit(&mut state, "info", "This window task was cancelled");
        Ok(())
    }

    pub(super) fn expire_window_question(&self) {
        let mut state = self.lock();
        let Some((id, why)) = state.window_dialogue.as_ref().and_then(|pending| {
            pending
                .references
                .check(pending.id.task)
                .err()
                .map(|why| (pending.id, why))
        }) else {
            return;
        };
        state.window_dialogue = None;
        if !matches!(
            state.phase,
            Phase::WaitingForInput | Phase::AwaitingApproval
        ) || state.task.as_ref().is_none_or(|task| task.id != id.task)
        {
            return;
        }
        state.actions.clear();
        if let Some(task) = state.task.as_mut() {
            task.steps[id.step].status = "failed".into();
            task.steps[id.step].detail = why.clone();
            task.error = Some(why.clone());
        }
        set_phase(&mut state, Phase::Failed);
        audit_best_effort(&mut state, AuditKind::Finished, None);
        emit(&mut state, "error", &why);
    }
}

#[cfg(test)]
mod tests;

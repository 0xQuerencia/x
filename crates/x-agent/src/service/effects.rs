//! Projection of native execution evidence into the compatibility service view.

use serde::Serialize;

use super::{AuditKind, Mode, Phase, Service, Task, audit_best_effort, emit};
use crate::tools::{EffectState, ExecutionOutcome};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionCompletion {
    Confirmed,
    Launched,
    Observed,
    Partial,
    Unverified,
    Failed,
    Cancelled,
}

impl Task {
    /// A terminal projection computed from evidence, never a stale success flag.
    /// Legacy `phase=completed` means the execution ended; this reports which
    /// effects, if any, actually have proof. Unmigrated workflows stay absent.
    pub fn action_outcome(&self) -> Option<ActionCompletion> {
        if self.mode != Mode::Actions || self.phase.is_busy() {
            return None;
        }
        match self.phase {
            Phase::Failed => return Some(ActionCompletion::Failed),
            Phase::Cancelled => return Some(ActionCompletion::Cancelled),
            Phase::Completed => {}
            _ => return None,
        }
        if !self.steps.iter().any(|step| step.execution.is_some()) {
            return None;
        }
        let (mut confirmed, mut observed, mut launched, mut unknown) = (0, 0, 0, 0);
        for step in &self.steps {
            match step
                .execution
                .as_ref()
                .map(|result| result.clone().checked().state)
            {
                Some(EffectState::Confirmed) => confirmed += 1,
                Some(EffectState::Observed) => observed += 1,
                Some(EffectState::Launched) => launched += 1,
                Some(EffectState::Failed) => return Some(ActionCompletion::Failed),
                Some(EffectState::Unverified) | None => unknown += 1,
            }
        }
        Some(if confirmed == self.steps.len() {
            ActionCompletion::Confirmed
        } else if observed == self.steps.len() {
            ActionCompletion::Observed
        } else if launched == self.steps.len() {
            ActionCompletion::Launched
        } else if confirmed > 0 || (observed > 0 && launched > 0) {
            ActionCompletion::Partial
        } else if unknown > 0 {
            ActionCompletion::Unverified
        } else {
            ActionCompletion::Partial
        })
    }
}

impl Service {
    /// Keep old status values readable while exposing the actual typed proof.
    /// Observed maps to legacy "launched", never legacy confirmed/done.
    pub(super) fn record_execution(&self, index: usize, outcome: ExecutionOutcome) -> EffectState {
        let outcome = outcome.checked();
        let effect = outcome.state;
        let (status, audit_kind, label) = match effect {
            EffectState::Confirmed => (
                "done",
                AuditKind::StepDone,
                "confirmed by the native adapter",
            ),
            EffectState::Observed => (
                "launched",
                AuditKind::Observed,
                "observed; requested effect not independently verified",
            ),
            EffectState::Launched => (
                "launched",
                AuditKind::Launched,
                "launched; outcome not verified",
            ),
            EffectState::Unverified => ("failed", AuditKind::Unverified, "effect not verified"),
            EffectState::Failed => ("failed", AuditKind::StepFailed, "failed"),
        };
        let mut state = self.lock();
        if let Some(step) = state
            .task
            .as_mut()
            .and_then(|task| task.steps.get_mut(index))
        {
            step.status = status.into();
            step.detail = outcome.message.clone();
            step.execution = Some(outcome.clone());
        }
        audit_best_effort(&mut state, audit_kind, Some(index));
        emit(
            &mut state,
            "result",
            &format!("{} ({label})", outcome.message),
        );
        effect
    }

    pub(super) fn action_summary(&self) -> String {
        let state = self.lock();
        let Some(task) = &state.task else {
            return "Actions ended without a retained result".into();
        };
        let count = |effect| {
            task.steps
                .iter()
                .filter(|step| {
                    step.execution
                        .as_ref()
                        .is_some_and(|result| result.state == effect)
                })
                .count()
        };
        let confirmed = count(EffectState::Confirmed);
        let observed = count(EffectState::Observed);
        let launched = count(EffectState::Launched);
        format!(
            "{confirmed} action(s) confirmed, {observed} observed, {launched} launched. Observations and launches do not independently verify the requested effect."
        )
    }
}

#[cfg(test)]
mod tests;

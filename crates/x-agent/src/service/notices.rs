//! Content-free completion notices. The desktop daemon never receives task
//! prompts, titles, paths, provider output, errors, selections or answers.

use std::collections::VecDeque;

use serde::Serialize;

use super::{ActionCompletion, Mode, Phase, Service, Task};

const MAX_PENDING: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    Confirmed,
    Launched,
    Observed,
    Partial,
    Unverified,
    Delivered,
    Finished,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct TaskNotice {
    pub task_id: u64,
    pub kind: NoticeKind,
}

/// Only identity and phase leave the service for an attention notification.
/// The token distinguishes successive questions without exposing their text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskAttention {
    pub task_id: u64,
    pub phase: Phase,
    pub question_token: Option<String>,
}

impl TaskNotice {
    pub(super) fn from_task(task: &Task) -> Option<Self> {
        let kind = match task.phase {
            Phase::Failed => NoticeKind::Failed,
            Phase::Cancelled => NoticeKind::Cancelled,
            Phase::Completed => match task.action_outcome() {
                Some(ActionCompletion::Confirmed) => NoticeKind::Confirmed,
                Some(ActionCompletion::Launched) => NoticeKind::Launched,
                Some(ActionCompletion::Observed) => NoticeKind::Observed,
                Some(ActionCompletion::Partial) => NoticeKind::Partial,
                Some(ActionCompletion::Unverified) => NoticeKind::Unverified,
                Some(ActionCompletion::Failed) => NoticeKind::Failed,
                Some(ActionCompletion::Cancelled) => NoticeKind::Cancelled,
                None if task.mode == Mode::Work && task.accepted_session.is_some() => {
                    NoticeKind::Delivered
                }
                None => NoticeKind::Finished,
            },
            _ => return None,
        };
        Some(Self {
            task_id: task.id,
            kind,
        })
    }

    /// Static wording plus an opaque local number is safe for desktop notices,
    /// even when the task itself contains secrets.
    pub fn message(self) -> String {
        let outcome = match self.kind {
            NoticeKind::Confirmed => "effects confirmed",
            NoticeKind::Launched => "launched; requested effect is not verified",
            NoticeKind::Observed => "observation available; requested effect is not verified",
            NoticeKind::Partial => "finished with mixed effect evidence",
            NoticeKind::Unverified => "finished without effect confirmation",
            NoticeKind::Delivered => "delivered to the provider; review changes and tests",
            NoticeKind::Finished => "finished; review its recorded evidence",
            NoticeKind::Failed => "failed; review the error in X",
            NoticeKind::Cancelled => "stopped; review any effects already made",
        };
        format!("Task {}: {outcome}. Open X to review.", self.task_id)
    }
}

#[derive(Default)]
pub(super) struct Notices {
    pending: VecDeque<TaskNotice>,
    // Deduplication survives draining. Retaining recent opaque IDs bounds
    // memory without persistent activity collection.
    emitted: VecDeque<u64>,
}

impl Notices {
    pub(super) fn push(&mut self, notice: TaskNotice) {
        if self.emitted.contains(&notice.task_id) {
            return;
        }
        self.emitted.push_back(notice.task_id);
        self.pending.push_back(notice);
        while self.emitted.len() > MAX_PENDING {
            self.emitted.pop_front();
        }
        while self.pending.len() > MAX_PENDING {
            self.pending.pop_front();
        }
    }
}

impl Service {
    pub fn task_attention(&self) -> Option<TaskAttention> {
        let state = self.lock();
        if !matches!(
            state.phase,
            Phase::AwaitingApproval | Phase::WaitingForInput
        ) {
            return None;
        }
        Some(TaskAttention {
            task_id: state.task.as_ref()?.id,
            phase: state.phase,
            question_token: state
                .computer_dialogue
                .as_ref()
                .map(|pending| pending.question_token())
                .or_else(|| {
                    state
                        .window_dialogue
                        .as_ref()
                        .map(|pending| pending.question_token())
                }),
        })
    }

    /// One native watcher consumes this bounded queue. Reading a service
    /// snapshot never consumes notices or triggers desktop side effects.
    pub fn take_task_notices(&self) -> Vec<TaskNotice> {
        let notices = self.lock().notices.clone();
        let mut notices = notices.lock().unwrap_or_else(|p| p.into_inner());
        notices.pending.drain(..).collect()
    }

    /// Clear only finished in-memory activity. Owned work keeps its state until
    /// the worker releases its lease. Durable recovery evidence is not removed.
    pub fn clear_completed_activity(&self) -> usize {
        let mut foreground = self.lock();
        let mut coordinator = self.coordinator.lock().unwrap_or_else(|p| p.into_inner());
        let removed = coordinator.clear_finished();
        let foreground_done = foreground.task.as_ref().is_some_and(|task| {
            matches!(
                task.phase,
                Phase::Completed | Phase::Failed | Phase::Cancelled
            ) && !coordinator.owns(task.id)
        });
        if foreground_done {
            foreground.task = None;
            foreground.phase = Phase::Idle;
            foreground.events.clear();
        }
        removed + usize::from(foreground_done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_preserve_dispatch_limits_without_content() {
        let mut task = super::super::edit::task(42, "PRIVATE PROMPT", vec![], None);
        task.phase = Phase::Completed;
        task.summary = Some("TOKEN=secret; verified done".into());
        task.error = Some("private/path/secret".into());
        task.changed_files = Some(vec!["PRIVATE FILE".into()]);
        let notice = TaskNotice::from_task(&task).unwrap();
        assert_eq!(notice.kind, NoticeKind::Finished);
        assert_eq!(
            serde_json::to_value(notice).unwrap(),
            serde_json::json!({
                "task_id": 42, "kind": "finished"
            })
        );
        assert!(!notice.message().contains("PRIVATE"));
        assert!(!notice.message().contains("secret"));
    }

    #[test]
    fn queue_draining_does_not_repeat_results_and_overflow_is_bounded() {
        let mut notices = Notices::default();
        for id in 1..=100 {
            notices.push(TaskNotice {
                task_id: id,
                kind: NoticeKind::Delivered,
            });
        }
        assert_eq!(notices.pending.len(), MAX_PENDING);
        assert_eq!(notices.pending.front().unwrap().task_id, 37);
        notices.pending.clear();
        notices.push(TaskNotice {
            task_id: 100,
            kind: NoticeKind::Confirmed,
        });
        assert!(notices.pending.is_empty());
    }
}

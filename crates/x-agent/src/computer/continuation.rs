//! Private retained state for a single-use, bounded desktop continuation.

use std::time::{Duration, Instant};

use super::{Limits, Progress};
use crate::desktop::Win;

/// A paused loop owns its exact app, window, evidence and remaining budget.
/// It is deliberately neither clonable nor serializable: resumption consumes it,
/// and restarting X never resurrects an old screen mutation.
#[derive(Eq, PartialEq)]
pub struct Continuation {
    pub(super) app: String,
    pub(super) goal: String,
    pub(super) window: Win,
    pub(super) progress: Progress,
    pub(super) started: Instant,
    pub(super) limits: Limits,
    pub(super) next_turn: usize,
    pub(super) stale_retry_used: bool,
    pub(super) question: String,
    pub(super) answers: Vec<String>,
}

impl std::fmt::Debug for Continuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Continuation")
            .field("app", &self.app)
            .field("next_turn", &self.next_turn)
            .field("remaining", &self.remaining())
            .finish_non_exhaustive()
    }
}

impl Continuation {
    pub(crate) fn app(&self) -> &str {
        &self.app
    }
    pub(crate) fn goal(&self) -> &str {
        &self.goal
    }
    pub(crate) fn question(&self) -> &str {
        &self.question
    }

    pub(crate) fn remaining(&self) -> Duration {
        self.limits.deadline.saturating_sub(self.started.elapsed())
    }

    pub(crate) fn check_live(&self) -> Result<(), String> {
        if self.remaining().is_zero() {
            return Err("The computer question expired with the original task deadline".into());
        }
        if self.next_turn >= self.limits.steps {
            return Err("The computer task has no decision steps remaining".into());
        }
        Ok(())
    }
}

pub(super) const MAX_CLARIFICATIONS: usize = 3;
pub(super) const MAX_ANSWER: usize = 300;

pub(crate) fn check_answer(answer: &str) -> Result<String, String> {
    let answer = answer.trim();
    if answer.is_empty()
        || answer.chars().count() > MAX_ANSWER
        || answer.chars().any(char::is_control)
    {
        return Err("Answer the pending question with 1–300 characters of plain text".into());
    }
    Ok(answer.into())
}

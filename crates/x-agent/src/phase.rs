//! Task phases and the only legal transitions between them.

use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Listening,
    Transcribing,
    Planning,
    AwaitingApproval,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl Phase {
    /// A new task may not start while one is in flight.
    pub fn is_busy(self) -> bool {
        matches!(
            self,
            Self::Listening
                | Self::Transcribing
                | Self::Planning
                | Self::AwaitingApproval
                | Self::Running
        )
    }

    pub fn can_move_to(self, next: Self) -> bool {
        use Phase::*;
        matches!(
            (self, next),
            (Idle | Completed | Failed | Cancelled, Listening | Planning)
                | (Listening, Transcribing | Idle)
                | (Transcribing, Planning | Idle)
                | (
                    Planning,
                    AwaitingApproval | Idle | Completed | Failed | Cancelled
                )
                | (AwaitingApproval, Running | Cancelled)
                | (Running, Completed | Failed | Cancelled)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::Phase::*;

    #[test]
    fn a_plan_must_be_approved_before_it_runs() {
        assert!(!Planning.can_move_to(Running));
        assert!(!Idle.can_move_to(Running));
        assert!(AwaitingApproval.can_move_to(Running));
    }

    #[test]
    fn finished_tasks_never_resume() {
        for done in [Completed, Failed, Cancelled] {
            assert!(!done.is_busy());
            assert!(!done.can_move_to(Running));
            assert!(!done.can_move_to(AwaitingApproval));
        }
    }

    #[test]
    fn busy_phases_block_new_work() {
        for busy in [Listening, Transcribing, Planning, AwaitingApproval, Running] {
            assert!(busy.is_busy());
            assert!(!busy.can_move_to(Listening) || busy == Listening || busy == Planning);
        }
    }
}

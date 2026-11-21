use super::{InputSource, Origin};
use crate::tools::{Action, Requirement, Risk};
use crate::wm::CloseTarget;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ActionId {
    pub task: u64,
    pub step: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RequestMetadata {
    pub idempotency_key: ActionId,
    pub origin: Origin,
    pub input_source: InputSource,
    pub risk: Risk,
    pub requirements: &'static [Requirement],
}

/// Constructed inside the service from a validated action and trusted task metadata.
#[derive(Clone)]
pub(super) struct ActionRequest {
    pub(super) metadata: RequestMetadata,
    pub(super) action: Action,
    pub(super) close_target: Option<CloseTarget>,
}

impl ActionRequest {
    pub(super) fn new(
        idempotency_key: ActionId,
        origin: Origin,
        input_source: InputSource,
        action: Action,
        close_target: Option<CloseTarget>,
    ) -> Self {
        let metadata = RequestMetadata {
            idempotency_key,
            origin,
            input_source,
            risk: action.risk(),
            requirements: action.requirements(),
        };
        Self {
            metadata,
            action,
            close_target,
        }
    }

    pub(super) fn title(&self) -> String {
        if let Some(label) = self.close_target.as_ref().and_then(CloseTarget::label) {
            format!("Close {label}")
        } else {
            self.action.describe()
        }
    }
}

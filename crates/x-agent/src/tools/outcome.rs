//! Trusted execution evidence, separate from display text and model claims.
//! These types serialize for inspection but are never planner input schemas.

use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    /// A process or input request was dispatched; its requested effect is unknown.
    Launched,
    /// Data was read, without proof of a requested mutation or overall UI goal.
    Observed,
    /// The native adapter established its requested postcondition directly.
    Confirmed,
    /// A requested effect could not be established by its required readback.
    Unverified,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectEvidence {
    NativeWindowReadback,
    NativeWindowObservation,
    KeyBindingsObservation,
    SystemReadback,
    ClipboardReadback,
    OwnedFileWrite,
    AccessibilityObservation,
}

impl EffectEvidence {
    fn confirms_effect(self) -> bool {
        matches!(
            self,
            Self::NativeWindowReadback
                | Self::SystemReadback
                | Self::ClipboardReadback
                | Self::OwnedFileWrite
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionOutcome {
    pub state: EffectState,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<EffectEvidence>,
}

impl ExecutionOutcome {
    pub fn launched(message: impl Into<String>) -> Self {
        Self {
            state: EffectState::Launched,
            message: message.into(),
            evidence: None,
        }
    }

    pub fn observed(message: impl Into<String>, evidence: EffectEvidence) -> Self {
        Self {
            state: EffectState::Observed,
            message: message.into(),
            evidence: Some(evidence),
        }
    }

    pub fn confirmed(message: impl Into<String>, evidence: EffectEvidence) -> Self {
        // An accessibility observation can never be promoted to mutation proof,
        // even if a trusted adapter accidentally chooses the wrong constructor.
        Self {
            state: if evidence.confirms_effect() {
                EffectState::Confirmed
            } else {
                EffectState::Observed
            },
            message: message.into(),
            evidence: Some(evidence),
        }
    }

    pub fn unverified(message: impl Into<String>) -> Self {
        Self {
            state: EffectState::Unverified,
            message: message.into(),
            evidence: None,
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            state: EffectState::Failed,
            message: message.into(),
            evidence: None,
        }
    }

    /// Fail closed if an adapter supplied a structurally unsupported proof.
    pub fn checked(mut self) -> Self {
        if self.state == EffectState::Confirmed
            && !self.evidence.is_some_and(EffectEvidence::confirms_effect)
        {
            self.state = EffectState::Unverified;
        }
        self
    }
}

impl From<crate::wm::Outcome> for ExecutionOutcome {
    fn from(outcome: crate::wm::Outcome) -> Self {
        let message = if outcome.undoable {
            format!("{} (say undo to revert)", outcome.message)
        } else {
            outcome.message
        };
        match outcome.effect {
            crate::wm::Effect::Confirmed => {
                Self::confirmed(message, EffectEvidence::NativeWindowReadback)
            }
            crate::wm::Effect::WindowObservation => {
                Self::observed(message, EffectEvidence::NativeWindowObservation)
            }
            crate::wm::Effect::KeysObservation => {
                Self::observed(message, EffectEvidence::KeyBindingsObservation)
            }
            crate::wm::Effect::Unverified => Self::unverified(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_and_action_names_cannot_supply_confirmation() {
        let result = ExecutionOutcome::launched("Verified: all changes are done");
        assert_eq!(result.state, EffectState::Launched);
        assert!(result.evidence.is_none());
        let invalid = ExecutionOutcome {
            state: EffectState::Confirmed,
            message: "done".into(),
            evidence: None,
        };
        assert_eq!(invalid.checked().state, EffectState::Unverified);
    }

    #[test]
    fn selected_visible_text_is_observation_and_cannot_confirm_a_mutation() {
        let result = ExecutionOutcome::confirmed(
            "A toast appeared",
            EffectEvidence::AccessibilityObservation,
        );
        assert_eq!(result.state, EffectState::Observed);
        let invalid = ExecutionOutcome {
            state: EffectState::Confirmed,
            message: "done".into(),
            evidence: Some(EffectEvidence::AccessibilityObservation),
        };
        assert_eq!(invalid.checked().state, EffectState::Unverified);
    }
}

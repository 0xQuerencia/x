use serde::Serialize;

/// Execution policy is independent of speech routing and the task's mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AutonomyMode {
    #[default]
    Default,
    Autonomous,
}

impl AutonomyMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "default" => Ok(Self::Default),
            "autonomous" => Ok(Self::Autonomous),
            _ => Err("Mode must be default or autonomous".into()),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Autonomous => "autonomous",
        }
    }

    /// The old trust bit grants automatic execution only to low-risk desktop tasks.
    pub fn from_trust(trusted: bool) -> Self {
        if trusted {
            Self::Autonomous
        } else {
            Self::Default
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Review {
    #[default]
    Required,
    LowRisk,
    ShowResult,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Approval {
    User,
    Automatic,
}

impl Review {
    pub(super) fn allows(self, mode: AutonomyMode, approval: Approval) -> bool {
        match (approval, self) {
            (Approval::User, _) | (Approval::Automatic, Self::ShowResult) => true,
            (Approval::Automatic, Self::LowRisk) => mode == AutonomyMode::Autonomous,
            (Approval::Automatic, Self::Required) => false,
        }
    }
}

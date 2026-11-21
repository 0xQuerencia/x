//! An owned coding run and its structured events, shared by Work and workflows.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;

use crate::cancellation::Cancellation;

pub mod probe;
mod session;
pub use session::{Session, SessionId};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    OpenCode,
    ClaudeCode,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenCode => "OpenCode",
            Self::ClaudeCode => "Claude Code",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    OwnedRun,
    StreamEvents,
    StopOwnedRun,
}

pub(crate) const OPENCODE_OWNED_CAPABILITIES: &[Capability] = &[
    Capability::OwnedRun,
    Capability::StreamEvents,
    Capability::StopOwnedRun,
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Descriptor {
    pub kind: Kind,
    pub event_schema: u16,
    /// An executable version must be probed before it is reported.
    pub version: Option<String>,
    /// Operations the adapter implements. Runtime readiness is probed separately.
    pub capabilities: &'static [Capability],
}

pub struct Request {
    pub workspace: PathBuf,
    pub prompt: String,
    pub allow_commands: Vec<String>,
}

pub trait Adapter: Send + Sync {
    fn descriptor(&self) -> Descriptor;

    fn start(
        &self,
        request: Request,
        events: EventSink,
        stop: &Cancellation,
    ) -> Result<Box<dyn OwnedRun>, String>;
}

pub trait OwnedRun: Send {
    fn join(self: Box<Self>) -> RunResult;
}

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Text,
    Tool,
    Permission,
    Error,
    StepStart,
    StepFinish,
    Activity,
    Stall,
    Raw,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Tool => "tool",
            Self::Permission => "permission",
            Self::Error => "error",
            Self::StepStart => "step_start",
            Self::StepFinish => "step_finish",
            Self::Activity => "event",
            Self::Stall => "stall",
            Self::Raw => "raw",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Event {
    pub kind: EventKind,
    pub text: String,
    /// A specific command reported by a structured bash permission refusal.
    /// The owner still has to select it for a newly approved run.
    pub blocked_command: Option<String>,
}

impl Event {
    pub fn new(kind: EventKind, text: impl AsRef<str>) -> Self {
        let text = text.as_ref().chars().take(450).collect();
        Self {
            kind,
            text,
            blocked_command: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
pub struct RunResult {
    pub status: RunStatus,
    pub exit_code: Option<i32>,
    pub errors: Vec<String>,
    pub stderr: Vec<String>,
    /// Files whose git status changed. Evidence may be unavailable outside a Git repository.
    pub changed_files: Option<Vec<String>>,
}

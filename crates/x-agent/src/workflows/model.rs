//! The typed workflow format. A workflow is a named list of steps; each step is one of a
//! closed set of actions. Nothing here is a shell command, and unknown keys, actions or
//! arguments are rejected rather than ignored.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::paths;
use crate::tools;

pub const MAX_STEPS: usize = 20;
pub const MAX_TRIGGERS: usize = 8;
pub const MAX_NAME: usize = 60;
pub const DEFAULT_TIMEOUT: u64 = 15;
pub const MAX_TIMEOUT: u64 = 120;
const MAX_TEXT: usize = 300;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    #[serde(rename = "do")]
    pub action: StepAction,
    /// Earlier steps that must have succeeded before this one runs.
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub on_failure: OnFailure,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    /// Stop the workflow here and report what did and did not happen.
    #[default]
    Stop,
    Continue,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum StepAction {
    /// Opens an approved app, unless one is already open (then that window is reused), and
    /// optionally moves its window to a workspace.
    OpenApp {
        app: String,
        #[serde(default)]
        workspace: Option<u8>,
    },
    OpenDirectory {
        directory: String,
    },
    /// A terminal starting in a folder.
    OpenTerminal {
        directory: String,
        #[serde(default)]
        workspace: Option<u8>,
    },
    OpenUrl {
        url: String,
    },
    FocusWindow {
        app: String,
        #[serde(default)]
        pick: Option<u8>,
    },
    SwitchWorkspace {
        workspace: u8,
    },
    MoveWindow {
        app: String,
        workspace: u8,
        #[serde(default)]
        pick: Option<u8>,
    },
    WaitForWindow {
        app: String,
    },
    /// An existing X action, written as the phrase you would say, such as
    /// `search github for rust lifetimes`. It is parsed and checked like spoken input.
    RunAction {
        phrase: String,
    },
    /// An OpenCode run in a folder, under OpenCode's own permission gates.
    OpencodeTask {
        goal: String,
        directory: String,
    },
}

impl StepAction {
    pub fn describe(&self) -> String {
        let at = |w: &Option<u8>| w.map_or(String::new(), |n| format!(" on workspace {n}"));
        match self {
            Self::OpenApp { app, workspace } => format!("Open {app}{}", at(workspace)),
            Self::OpenDirectory { directory } => format!("Open folder {directory}"),
            Self::OpenTerminal {
                directory,
                workspace,
            } => {
                format!("Open a terminal in {directory}{}", at(workspace))
            }
            Self::OpenUrl { url } => format!("Open {url}"),
            Self::FocusWindow { app, .. } => format!("Focus {app}"),
            Self::SwitchWorkspace { workspace } => format!("Switch to workspace {workspace}"),
            Self::MoveWindow { app, workspace, .. } => {
                format!("Move {app} to workspace {workspace}")
            }
            Self::WaitForWindow { app } => format!("Wait for {app} to appear"),
            Self::RunAction { phrase } => format!("Run: {phrase}"),
            Self::OpencodeTask { goal, directory } => {
                format!("OpenCode in {directory}: {goal}")
            }
        }
    }

    /// What approving this step allows, in plain words.
    pub fn risk(&self) -> &'static str {
        match self {
            Self::OpencodeTask { .. } => "can modify files in that folder",
            Self::OpenApp { .. } | Self::OpenTerminal { .. } | Self::OpenDirectory { .. } => {
                "launches an app"
            }
            Self::OpenUrl { .. } => "opens a website",
            Self::RunAction { .. } => "runs an X action",
            _ => "",
        }
    }
}

/// One thing wrong with a workflow, tied to a step when it has one.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Problem {
    pub step: Option<String>,
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.step {
            Some(s) => write!(f, "step {s:?}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

fn slug_ok(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && s.as_bytes()[0].is_ascii_alphanumeric()
}

/// The words of a phrase, lowercase, punctuation stripped: how triggers are compared.
pub fn normalize(phrase: &str) -> String {
    phrase
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a phrase already means something built in, so a workflow may not claim it.
pub fn is_builtin_phrase(phrase: &str) -> bool {
    let phrase = normalize(phrase);
    tools::plan_rules(&phrase).is_some()
        || crate::context::commands::parse(&phrase).is_some()
        || crate::edit::commands::parse(&phrase).is_some()
}

impl Workflow {
    pub fn needs_approval(&self) -> bool {
        self.steps
            .iter()
            .any(|s| matches!(s.action, StepAction::OpencodeTask { .. }))
    }

    /// Strict validation, with every problem found (not just the first).
    pub fn validate(&self, home: &Path) -> Result<(), Vec<Problem>> {
        let mut bad = Vec::new();
        let mut note = |step: Option<&str>, m: String| {
            bad.push(Problem {
                step: step.map(String::from),
                message: m,
            });
        };
        if !slug_ok(&self.id) {
            note(
                None,
                "The id must be 1-40 characters of a-z, 0-9, - or _".into(),
            );
        }
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control)
        {
            note(None, format!("The name must be 1-{MAX_NAME} characters"));
        }
        if self.steps.is_empty() || self.steps.len() > MAX_STEPS {
            note(None, format!("A workflow needs 1-{MAX_STEPS} steps"));
        }
        if self.triggers.len() > MAX_TRIGGERS {
            note(None, format!("At most {MAX_TRIGGERS} trigger phrases"));
        }
        let mut seen_triggers = HashSet::new();
        for t in &self.triggers {
            let n = normalize(t);
            let words = n.split(' ').count();
            if n.is_empty() || words > 6 || n.chars().count() > 60 {
                note(None, format!("Trigger {t:?} must be 1-6 words"));
            } else if is_builtin_phrase(&n) {
                note(None, format!("Trigger {t:?} is already a built-in command"));
            } else if !seen_triggers.insert(n) {
                note(None, format!("Trigger {t:?} is listed twice"));
            }
        }
        let mut ids: Vec<&str> = Vec::new();
        for step in &self.steps {
            let sid = Some(step.id.as_str());
            if !slug_ok(&step.id) {
                note(
                    sid,
                    "The step id must be 1-40 characters of a-z, 0-9, - or _".into(),
                );
            }
            if ids.contains(&step.id.as_str()) {
                note(sid, "Step ids must be unique".into());
            }
            for r in &step.requires {
                if !ids.contains(&r.as_str()) {
                    note(sid, format!("requires {r:?}, which is not an earlier step"));
                }
            }
            if step.timeout_secs.is_some_and(|t| t == 0 || t > MAX_TIMEOUT) {
                note(sid, format!("The timeout must be 1-{MAX_TIMEOUT} seconds"));
            }
            if let Err(why) = check_action(&step.action, home) {
                note(sid, why);
            }
            ids.push(&step.id);
        }
        if bad.is_empty() { Ok(()) } else { Err(bad) }
    }
}

fn check_workspace(n: u8) -> Result<(), String> {
    if (1..=10).contains(&n) {
        Ok(())
    } else {
        Err("A workspace must be 1-10".into())
    }
}

fn check_app(app: &str) -> Result<(), String> {
    tools::APPS
        .iter()
        .any(|(name, _)| *name == app)
        .then_some(())
        .ok_or_else(|| format!("{app:?} is not an approved app"))
}

fn check_text(label: &str, text: &str) -> Result<(), String> {
    if text.trim().is_empty()
        || text.chars().count() > MAX_TEXT
        || text.chars().any(char::is_control)
    {
        return Err(format!(
            "{label} must be 1-{MAX_TEXT} characters on one line"
        ));
    }
    Ok(())
}

fn check_action(action: &StepAction, home: &Path) -> Result<(), String> {
    match action {
        StepAction::OpenApp { app, workspace } => {
            check_app(app)?;
            workspace.map_or(Ok(()), check_workspace)
        }
        StepAction::OpenDirectory { directory } => paths::check(directory, home).map(drop),
        StepAction::OpenTerminal {
            directory,
            workspace,
        } => {
            paths::check(directory, home)?;
            workspace.map_or(Ok(()), check_workspace)
        }
        StepAction::OpenUrl { url } => tools::safe_url(url).map(drop),
        StepAction::FocusWindow { app, pick } => {
            check_app(app)?;
            pick.map_or(Ok(()), |p| {
                if p >= 1 {
                    Ok(())
                } else {
                    Err("pick starts at 1".into())
                }
            })
        }
        StepAction::SwitchWorkspace { workspace } => check_workspace(*workspace),
        StepAction::MoveWindow { app, workspace, .. } => {
            check_app(app)?;
            check_workspace(*workspace)
        }
        StepAction::WaitForWindow { app } => check_app(app),
        StepAction::RunAction { phrase } => {
            check_text("The action phrase", phrase)?;
            let actions = tools::plan_rules(phrase)
                .ok_or("X does not understand that phrase as a built-in action")?;
            if !tools::is_low_risk(&actions) {
                return Err(
                    "That action needs approval each time, so it cannot be a workflow step".into(),
                );
            }
            if actions
                .iter()
                .any(|a| matches!(a, tools::Action::Type { .. }))
            {
                return Err("Typing into windows is not allowed in workflows".into());
            }
            if actions.iter().any(|a| {
                matches!(
                    a,
                    tools::Action::Session { .. } | tools::Action::Computer { .. }
                )
            }) {
                return Err(
                    "Sessions and screen control need their own approval; use an OpenCode task step instead"
                        .into(),
                );
            }
            Ok(())
        }
        StepAction::OpencodeTask { goal, directory } => {
            check_text("The goal", goal)?;
            paths::check(directory, home).map(drop)
        }
    }
}

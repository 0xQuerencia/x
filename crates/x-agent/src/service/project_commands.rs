use std::time::Instant;

use crate::harness::{Kind, Session, SessionId};
use crate::projects::{Project, Registry};

use super::edit::{step, task};
use super::{
    AuditKind, Cancellation, Phase, Review, Service, Task, audit_best_effort, busy, emit,
    update_workspace,
};

pub(super) enum Command {
    Projects,
    Find(String),
    Select(String),
    Open(String),
    SavedSessions,
    Resume(String),
}

pub(super) enum Pending {
    Select(Project),
    Open(Project),
    Resume(Session),
}

pub(super) fn parse(input: &str) -> Option<Command> {
    let spoken = crate::tools::strip_wake_word(input);
    let normalized = spoken.trim_end_matches(['.', '?']).trim_end();
    let lower = normalized.to_lowercase();
    match lower.as_str() {
        "list projects"
        | "list registered projects"
        | "show my projects"
        | "show registered projects"
        | "what projects do i have" => return Some(Command::Projects),
        "list saved sessions" => return Some(Command::SavedSessions),
        _ => {}
    }
    for (prefix, command) in [
        ("find project ", Command::Find as fn(String) -> Command),
        ("find my project ", Command::Find),
        ("use project ", Command::Select),
        ("select project ", Command::Select),
        ("work on project ", Command::Select),
        ("let's work on project ", Command::Select),
        ("switch to project ", Command::Select),
        ("switch back to project ", Command::Select),
        ("set project to ", Command::Select),
        ("open project ", Command::Open),
        ("resume opencode session ", Command::Resume),
        ("resume session ", Command::Resume),
    ] {
        if lower.starts_with(prefix) {
            return Some(command(normalized[prefix.len()..].trim().into()));
        }
    }
    if lower.starts_with("open my ") && lower.ends_with(" project") && normalized.len() > 16 {
        return Some(Command::Open(
            normalized[8..normalized.len() - 8].trim().into(),
        ));
    }
    None
}

impl Service {
    fn project_registry(&self) -> Result<Registry, String> {
        self.session_store()
            .map(|(registry, _)| registry)
            .ok_or_else(|| "Project storage is not configured".into())
    }

    pub fn select_registered_project(&self, query: &str) -> Result<(Project, bool), String> {
        let registry = self.project_registry()?;
        let mut state = self.lock();
        if state.phase.is_busy() {
            return Err(busy(state.phase));
        }
        let (project, changed) = registry.select(query)?;
        let switched = state.workspace.as_ref() != Some(&project.root);
        update_workspace(&mut state, project.root.clone());
        Ok((project, changed || switched))
    }

    pub(super) fn plan_project_command(&self, command: Command) -> Result<Task, String> {
        let registry = self.project_registry()?;
        let pending = match command {
            Command::Projects => {
                self.begin_instant("List registered projects")?;
                return self.end_instant(registry.list().map(|projects| {
                    if projects.is_empty() {
                        return "No registered projects".into();
                    }
                    projects
                        .iter()
                        .map(|project| format!("{} ({})", project.name, project.id))
                        .collect::<Vec<_>>()
                        .join("\n")
                }));
            }
            Command::Find(query) => {
                self.begin_instant("Find registered project")?;
                return self.end_instant(registry.validated_resolve(&query).map(|project| {
                    format!(
                        "{} · {} · {}",
                        project.name,
                        project.id,
                        project.root.display()
                    )
                }));
            }
            Command::Select(query) => Pending::Select(registry.validated_resolve(&query)?),
            Command::Open(query) => Pending::Open(registry.validated_resolve(&query)?),
            Command::SavedSessions => {
                let workspace = self.workspace_path().ok_or("Choose a project first")?;
                self.begin_instant("List saved sessions")?;
                return self.end_instant(self.remembered_sessions(&workspace).map(|sessions| {
                    if sessions.is_empty() {
                        return "No saved sessions in this project".into();
                    }
                    let ids = sessions
                        .iter()
                        .map(|link| {
                            format!(
                                "{} · {}",
                                link.session_id.kind().label(),
                                link.session_id.as_str()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!("Saved sessions with unverified availability\n{ids}")
                }));
            }
            Command::Resume(query) => Pending::Resume(self.plan_resume_target(&query)?),
        };
        let (title, risk) = match &pending {
            Pending::Select(project) => (
                format!(
                    "Select {} for Work in {}",
                    project.name,
                    project.root.display()
                ),
                "changes the workspace for future Work",
            ),
            Pending::Open(project) => (
                format!(
                    "Open {} in the file manager at {}",
                    project.name,
                    project.root.display()
                ),
                "launches the system file manager",
            ),
            Pending::Resume(session) => (
                format!(
                    "Verify and select OpenCode session {} in {}",
                    session.id.as_str(),
                    session.workspace.display()
                ),
                "changes the session for future Work",
            ),
        };
        let mut state = self.lock();
        if state.phase.is_busy() {
            return Err(busy(state.phase));
        }
        if let Pending::Resume(session) = &pending
            && state.workspace.as_ref() != Some(&session.workspace)
        {
            return Err("The selected project changed".into());
        }
        state.serial += 1;
        let mut task = task(state.serial, &title, vec![step(&title, risk)], None);
        task.phase = Phase::AwaitingApproval;
        state.project_command = Some(pending);
        state.phase = Phase::AwaitingApproval;
        state.review = Review::Required;
        state.actions.clear();
        state.grants.clear();
        state.guard = None;
        state.edit = None;
        state.workflow = None;
        state.create_workspace = false;
        state.task = Some(task.clone());
        audit_best_effort(&mut state, AuditKind::Planned, None);
        emit(&mut state, "plan", &title);
        Ok(task)
    }

    fn plan_resume_target(&self, query: &str) -> Result<Session, String> {
        let state = self.lock();
        let workspace = state.workspace.as_ref().ok_or("Choose a project first")?;
        if let Ok(id) = SessionId::try_from(query.to_string()) {
            if id.kind() != Kind::OpenCode {
                return Err("Enter an exact OpenCode session ID beginning with ses_".into());
            }
            return Ok(Session {
                title: id.as_str().into(),
                id,
                workspace: workspace.clone(),
                updated_at: 0,
            });
        }
        state
            .session_list
            .as_ref()
            .ok_or("Display the project's OpenCode sessions before choosing by title or number")?
            .resolve(workspace, query, Instant::now())
    }

    pub(super) fn run_project_command(&self, pending: Pending, stop: &Cancellation) {
        let result = self.execute_project_command(pending, stop);
        match result {
            Ok((status, message)) => {
                self.set_step(0, status);
                self.finish(Phase::Completed, None, Some(message), None);
            }
            Err(_) if stop.requested() => {
                self.set_step(0, "cancelled");
                self.finish(Phase::Cancelled, None, Some("Stopped".into()), None);
            }
            Err(error) => {
                self.set_step(0, "failed");
                self.finish(Phase::Failed, Some(error), None, None);
            }
        }
    }

    fn execute_project_command(
        &self,
        pending: Pending,
        stop: &Cancellation,
    ) -> Result<(&'static str, String), String> {
        stop.check()?;
        let registry = self.project_registry()?;
        match pending {
            Pending::Select(project) => {
                let mut state = self.lock();
                stop.check()?;
                let (selected, _) = registry.select_pinned(&project)?;
                update_workspace(&mut state, selected.root.clone());
                Ok(("done", format!("Selected {}", selected.name)))
            }
            Pending::Open(project) => {
                let current = registry.validated_resolve(&project.id.to_string())?;
                if current.root != project.root {
                    return Err("The approved project directory changed".into());
                }
                stop.check()?;
                self.launcher.open_project_folder(&current.root)?;
                Ok((
                    "launched",
                    format!(
                        "Requested {} in the file manager. Opening is unverified.",
                        current.name
                    ),
                ))
            }
            Pending::Resume(session) => {
                let verify = self
                    .session_verifier
                    .as_ref()
                    .ok_or("OpenCode session verification is unavailable")?;
                verify(&session)?;
                let mut state = self.lock();
                stop.check()?;
                if state.workspace.as_ref() != Some(&session.workspace) {
                    return Err("The selected project changed".into());
                }
                let id = session.id.as_str().to_string();
                state.selected_session = Some(session.clone());
                drop(state);
                self.record_provider_session(&session);
                Ok((
                    "done",
                    format!("Verified and selected OpenCode session {id}"),
                ))
            }
        }
    }
}

#[cfg(test)]
mod spoken_project_command_tests {
    use super::{Command, parse};

    #[test]
    fn common_project_phrases_have_explicit_deterministic_targets() {
        for phrase in [
            "Friday, work on project X",
            "Friday, let's work on project X.",
            "switch back to project X",
            "set project to X",
            "select project X",
        ] {
            assert!(
                matches!(parse(phrase), Some(Command::Select(ref query)) if query == "X"),
                "{phrase}"
            );
        }
        for phrase in ["show my projects", "what projects do I have?"] {
            assert!(matches!(parse(phrase), Some(Command::Projects)), "{phrase}");
        }
        assert!(
            matches!(parse("find my project X"), Some(Command::Find(ref query)) if query == "X")
        );
    }

    #[test]
    fn unrelated_or_incomplete_phrases_never_become_project_actions() {
        for phrase in [
            "work on the project later",
            "open a browser",
            "find project files",
            "switch to workspace 3",
        ] {
            // A partially matching phrase may produce a project lookup, but
            // it cannot execute without resolving a registered exact alias.
            if let Some(Command::Open(_)) | Some(Command::Select(_)) = parse(phrase) {
                panic!("An unrelated phrase became a project mutation: {phrase}");
            }
        }
    }
}

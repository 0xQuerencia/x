//! Native Work drafts. Project and session selection stay in the existing
//! pickers; this form forwards one typed proposal to the real service planner.

use std::sync::mpsc::{Receiver, TryRecvError, channel};

use ::x_agent::harness::{Descriptor, Kind, Session};
use ::x_agent::projects::Project;
use ::x_agent::service::Snapshot;
use ::x_agent::service::composer::{Dependency, Draft, Request, Role, SessionChoice};
use rune::{El, TextEdit, col, row, text};
use rune_kit::{TextArea, TextInput};

use crate::agent_host;
use crate::rune_ui::Ui;
use crate::rune_ui::controls::{ButtonKind, DESCRIPTION, Paint, button, segmented};
use crate::rune_ui::lists::section_label;

#[cfg(test)]
mod tests;

struct TaskFields {
    title: TextEdit,
    goal: TextEdit,
    acceptance: TextEdit,
    provider: Kind,
}

impl TaskFields {
    fn new(title: &str) -> Self {
        Self {
            title: TextEdit::new(title),
            goal: TextEdit::default(),
            acceptance: TextEdit::default(),
            provider: Kind::OpenCode,
        }
    }

    fn draft(&self, snapshot: &Snapshot) -> Draft {
        Draft {
            title: self.title.text.clone(),
            goal: self.goal.text.clone(),
            acceptance: self.acceptance.text.clone(),
            provider: self.provider,
            role: provider(snapshot, self.provider).map_or(Role::WorkspaceWrite, provider_role),
        }
    }
}

pub(super) struct Composer {
    expanded: bool,
    two_tasks: bool,
    first: TaskFields,
    second: TaskFields,
    dependency: Dependency,
    worker_limit: usize,
    test_suite: Option<usize>,
    use_selected_session: bool,
    project: Option<Project>,
    seen_workspace: Option<Option<String>>,
    project_error: Option<String>,
    notice: Option<String>,
    preview: Option<Receiver<Result<(), String>>>,
}

impl Default for Composer {
    fn default() -> Self {
        Self {
            expanded: false,
            two_tasks: true,
            first: TaskFields::new("Interface"),
            second: TaskFields::new("Backend"),
            dependency: Dependency::Independent,
            worker_limit: 2,
            test_suite: None,
            use_selected_session: false,
            project: None,
            seen_workspace: None,
            project_error: None,
            notice: None,
            preview: None,
        }
    }
}

impl Composer {
    /// Preserve the exact identity chosen by the existing project picker,
    /// including a re-registered project whose directory text is unchanged.
    pub(super) fn select_project(&mut self, project: Project) {
        self.seen_workspace = Some(Some(project.root.display().to_string()));
        self.project = Some(project);
        self.project_error = None;
        self.use_selected_session = false;
        self.notice = None;
    }

    /// Cheap on unchanged snapshots. Project discovery uses the same public
    /// registry API as the existing picker, only when its workspace changes.
    pub(super) fn poll(&mut self, snapshot: &Snapshot) -> bool {
        let mut changed = false;
        if self.seen_workspace.as_ref() != Some(&snapshot.workspace) {
            self.seen_workspace = Some(snapshot.workspace.clone());
            self.project = None;
            self.project_error = None;
            self.use_selected_session = false;
            if let Some(workspace) = &snapshot.workspace {
                match agent_host::service().list_registered_projects() {
                    Ok(projects) => {
                        self.project = projects
                            .into_iter()
                            .find(|project| project.root.to_str() == Some(workspace.as_str()));
                        if self.project.is_none() {
                            self.project_error =
                                Some("Choose a registered project above to compose Work.".into());
                        }
                    }
                    Err(error) => self.project_error = Some(error),
                }
            }
            changed = true;
        }
        let result = match self.preview.as_ref().map(Receiver::try_recv) {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => {
                Some(Err("Preview stopped before returning a result".into()))
            }
            Some(Err(TryRecvError::Empty)) | None => None,
        };
        if let Some(result) = result {
            self.preview = None;
            self.notice = Some(match result {
                Ok(()) => "Plan ready above. Review the project, permissions and exact steps before approving.".into(),
                Err(error) => error,
            });
            changed = true;
        }
        changed
    }

    fn request(&self, snapshot: &Snapshot) -> Result<Request, String> {
        let project = self
            .project
            .clone()
            .ok_or("Choose a registered project above first")?;
        let session = if !self.two_tasks && self.use_selected_session {
            SessionChoice::Selected(
                selected_session(snapshot, self.first.provider)
                    .cloned()
                    .ok_or("Select a session for this provider above first")?,
            )
        } else {
            SessionChoice::Fresh
        };
        Ok(Request {
            project,
            first: self.first.draft(snapshot),
            second: self.two_tasks.then(|| self.second.draft(snapshot)),
            dependency: if self.two_tasks {
                self.dependency
            } else {
                Dependency::Independent
            },
            worker_limit: if self.two_tasks { self.worker_limit } else { 1 },
            test_suite: self
                .two_tasks
                .then_some(self.test_suite)
                .flatten()
                .map(|index| super::TEST_SUITES[index % super::TEST_SUITES.len()].1),
            session,
        })
    }
}

fn provider(snapshot: &Snapshot, kind: Kind) -> Option<&Descriptor> {
    match kind {
        Kind::OpenCode => snapshot.harness.as_ref(),
        Kind::ClaudeCode => snapshot.claude.as_ref(),
    }
}

fn provider_role(descriptor: &Descriptor) -> Role {
    if descriptor.supports_workspace_write() {
        Role::WorkspaceWrite
    } else {
        Role::Analysis
    }
}

fn selected_session(snapshot: &Snapshot, kind: Kind) -> Option<&Session> {
    match kind {
        Kind::OpenCode => snapshot.selected_session.as_ref(),
        Kind::ClaudeCode => snapshot.claude_session.as_ref(),
    }
}

fn edit_task(ui: &mut Ui, index: usize) -> &mut TaskFields {
    if index == 0 {
        &mut ui.agent.composer.first
    } else {
        &mut ui.agent.composer.second
    }
}

fn start_preview(ui: &mut Ui) {
    if ui.agent.is_busy() || ui.agent.composer.preview.is_some() {
        return;
    }
    let request = ui
        .agent
        .snapshot
        .as_ref()
        .ok_or_else(|| "Agent state is unavailable".to_owned())
        .and_then(|snapshot| ui.agent.composer.request(snapshot));
    let request = match request {
        Ok(request) => request,
        Err(error) => {
            ui.agent.composer.notice = Some(error.to_string());
            return;
        }
    };
    let (sender, receiver) = channel();
    let waker = ui.modes.waker.clone();
    match std::thread::Builder::new()
        .name("x-work-preview".into())
        .spawn(move || {
            let result = agent_host::service()
                .plan_work_composer(&request)
                .map(|_| ());
            let _ = sender.send(result);
            if let Some(waker) = waker {
                waker();
            }
        }) {
        Ok(_) => {
            ui.agent.composer.preview = Some(receiver);
            ui.agent.composer.notice = Some("Preparing the exact plan…".into());
        }
        Err(error) => {
            ui.agent.composer.notice = Some(format!("Could not prepare the plan: {error}"))
        }
    }
}

pub(super) fn view(p: Paint, ui: &Ui) -> El {
    let composer = &ui.agent.composer;
    let mut form = col().key("work-composer").gap(12.0).child(
        row()
            .key("composer-heading")
            .center_y()
            .gap(8.0)
            .child(section_label(p, "composer-title", "Work task composer"))
            .child(button(
                p,
                "composer-expand",
                if composer.expanded {
                    "Hide composer"
                } else {
                    "Compose tasks"
                },
                ButtonKind::Secondary,
                |ui: &mut Ui, _| {
                    ui.agent.composer.expanded = !ui.agent.composer.expanded;
                    if ui.agent.composer.expanded {
                        ui.agent.composer.seen_workspace = None;
                    }
                },
            )),
    );
    if !composer.expanded {
        return form;
    }
    if ui.agent.snapshot.is_none() {
        return form;
    }
    let project_label = composer.project.as_ref().map_or_else(
        || "Select a registered project above".into(),
        |project| format!("{} · {}", project.name, project.root.display()),
    );
    form = form.child(
        text(project_label)
            .key("composer-project")
            .font(DESCRIPTION)
            .color(p.text())
            .wrap(),
    );
    if let Some(error) = &composer.project_error {
        form = form.child(
            text(error.clone())
                .key("composer-project-error")
                .font(DESCRIPTION)
                .color(p.warning())
                .wrap(),
        );
    }
    form = form.child(segmented(
        p,
        "composer-size",
        &["Single task", "Two tasks"],
        usize::from(composer.two_tasks),
        |ui: &mut Ui, index| {
            ui.agent.composer.two_tasks = index == 1;
            ui.agent.composer.use_selected_session = false;
            ui.agent.composer.notice = None;
        },
    ));
    form = form.child(task_fields(p, ui, 0));
    if composer.two_tasks {
        form = form
            .child(task_fields(p, ui, 1))
            .child(dependencies(p, ui))
            .child(test_choice(p, ui));
        form = form.child(text("Each task starts a fresh provider session in an owned Git worktree and must leave a clean commit. The preview lists the exact commit commands. Integration into the original project needs separate approval.")
            .key("composer-isolation").font(DESCRIPTION).color(p.muted()).wrap());
    } else {
        form = form.child(session_choice(p, ui));
    }
    if let Some(notice) = &composer.notice {
        form = form.child(
            text(notice.clone())
                .key("composer-notice")
                .font(DESCRIPTION)
                .color(p.text())
                .wrap(),
        );
    }
    if ui.agent.is_busy() {
        form = form.child(
            text("Finish or stop the current foreground task before previewing another plan.")
                .font(DESCRIPTION)
                .color(p.muted())
                .wrap(),
        );
    } else if composer.preview.is_none() {
        form = form.child(button(
            p,
            "composer-preview",
            "Preview Work plan",
            ButtonKind::Secondary,
            |ui: &mut Ui, _| start_preview(ui),
        ));
    }
    form
}

fn task_fields(p: Paint, ui: &Ui, index: usize) -> El {
    let composer = &ui.agent.composer;
    let fields = if index == 0 {
        &composer.first
    } else {
        &composer.second
    };
    let snapshot = ui
        .agent
        .snapshot
        .as_ref()
        .expect("composer requires a snapshot");
    let mut providers = row()
        .key(format!("composer-providers-{index}"))
        .gap(6.0)
        .center_y();
    let mut count = 0;
    for descriptor in [snapshot.harness.as_ref(), snapshot.claude.as_ref()]
        .into_iter()
        .flatten()
    {
        let role = provider_role(descriptor);
        if !role.supported_by(descriptor) || composer.two_tasks && role != Role::WorkspaceWrite {
            continue;
        }
        count += 1;
        let kind = descriptor.kind;
        let label = format!(
            "{}{} · {}",
            if fields.provider == kind { "✓ " } else { "" },
            kind.label(),
            role.label()
        );
        providers = providers.child(button(
            p,
            &format!("composer-provider-{index}-{kind:?}"),
            &label,
            ButtonKind::Secondary,
            move |ui: &mut Ui, _| {
                edit_task(ui, index).provider = kind;
                ui.agent.composer.use_selected_session = false;
                ui.agent.composer.notice = None;
            },
        ));
    }
    let mut content = col()
        .key(format!("composer-task-{index}"))
        .gap(6.0)
        .child(section_label(
            p,
            &format!("composer-task-heading-{index}"),
            &format!("Task {}", index + 1),
        ))
        .child(providers);
    if count == 0 {
        content = content.child(text(if composer.two_tasks {
            "No workspace writer is available. Install and configure OpenCode before previewing two coding tasks."
        } else { "No supported provider is available. Install and configure OpenCode or Claude Code first." })
            .font(DESCRIPTION).color(p.warning()).wrap());
    }
    if composer.two_tasks
        && snapshot
            .claude
            .as_ref()
            .is_some_and(|descriptor| !descriptor.supports_workspace_write())
    {
        content = content.child(text("Claude Code currently supports tool-free analysis. It cannot write or supply code artifacts for these tasks.")
            .key(format!("composer-capabilities-{index}")).font(DESCRIPTION).color(p.muted()).wrap());
    }
    if !composer.two_tasks && fields.provider == Kind::ClaudeCode {
        content = content.child(text("Supply the text to analyze in the goal. This provider does not read or edit project files.")
            .font(DESCRIPTION).color(p.muted()).wrap());
    }
    content
        .child(text("Task name").font(DESCRIPTION).color(p.text()))
        .child(
            TextInput::new(&fields.title)
                .placeholder("Task name")
                .width(None)
                .on_edit(move |ui: &mut Ui| &mut edit_task(ui, index).title)
                .build(p.k)
                .key(format!("composer-name-{index}"))
                .label(format!("Task {} name", index + 1)),
        )
        .child(text("Goal").font(DESCRIPTION).color(p.text()))
        .child(
            TextArea::new(&fields.goal)
                .placeholder(if index == 0 {
                    "What should the first task achieve?"
                } else {
                    "What should the second task achieve?"
                })
                .rows(3)
                .width(None)
                .on_edit(move |ui: &mut Ui| &mut edit_task(ui, index).goal)
                .build(p.k)
                .key(format!("composer-goal-{index}"))
                .label(format!("Task {} goal", index + 1)),
        )
        .child(
            text("Acceptance criteria (optional)")
                .font(DESCRIPTION)
                .color(p.text()),
        )
        .child(
            TextArea::new(&fields.acceptance)
                .placeholder("What will you check before accepting this result?")
                .rows(2)
                .width(None)
                .on_edit(move |ui: &mut Ui| &mut edit_task(ui, index).acceptance)
                .build(p.k)
                .key(format!("composer-acceptance-{index}"))
                .label(format!("Task {} acceptance criteria", index + 1)),
        )
}

fn dependencies(p: Paint, ui: &Ui) -> El {
    let composer = &ui.agent.composer;
    let chained = composer.dependency == Dependency::FirstArtifact;
    let mut content = col().key("composer-dependencies").gap(6.0)
        .child(text("Task relationship").font(DESCRIPTION).color(p.text()))
        .child(segmented(p, "composer-dependency-choice", &["Independent", "Task 2 uses task 1"], usize::from(chained),
            |ui: &mut Ui, index| {
                ui.agent.composer.dependency = if index == 0 { Dependency::Independent } else { Dependency::FirstArtifact };
            }))
        .child(text(if chained {
            "Task 2 starts from task 1's exact committed output. A failed or missing artifact blocks task 2."
        } else {
            "Both tasks start from the same reviewed project commit in separate worktrees. Tests combine task 1's commit, then task 2's commit; conflicts stop the test node."
        }).font(DESCRIPTION).color(p.muted()).wrap());
    if chained {
        content = content.child(
            text("Worker limit: 1 at a time because task 2 needs task 1's result.")
                .font(DESCRIPTION)
                .color(p.muted())
                .wrap(),
        );
    } else {
        content = content
            .child(text("Worker limit").font(DESCRIPTION).color(p.text()))
            .child(segmented(
                p,
                "composer-worker-limit",
                &["1 at a time", "2 at a time"],
                composer.worker_limit.saturating_sub(1),
                |ui: &mut Ui, index| ui.agent.composer.worker_limit = index + 1,
            ));
    }
    content
}

fn test_choice(p: Paint, ui: &Ui) -> El {
    let composer = &ui.agent.composer;
    let mut content = col()
        .key("composer-verification")
        .gap(6.0)
        .child(
            text("Verify committed output")
                .font(DESCRIPTION)
                .color(p.text()),
        )
        .child(segmented(
            p,
            "composer-verification-choice",
            &["Review commits", "Run project tests"],
            usize::from(composer.test_suite.is_some()),
            |ui: &mut Ui, index| ui.agent.composer.test_suite = (index == 1).then_some(0),
        ));
    if let Some(index) = composer.test_suite {
        // Two short rows keep every fixed suite directly keyboard/pointer
        // selectable without a shell-command field or an unlabelled cycle.
        for (group, suites) in super::TEST_SUITES.chunks(3).enumerate() {
            let mut choices = row().key(format!("composer-suites-{group}")).gap(6.0);
            for (offset, (label, _)) in suites.iter().enumerate() {
                let suite = group * 3 + offset;
                choices = choices.child(button(
                    p,
                    &format!("composer-suite-{suite}"),
                    &format!("{}{}", if index == suite { "✓ " } else { "" }, label),
                    ButtonKind::Secondary,
                    move |ui: &mut Ui, _| ui.agent.composer.test_suite = Some(suite),
                ));
            }
            content = content.child(choices);
        }
        let suite = super::TEST_SUITES[index % super::TEST_SUITES.len()].1;
        let (program, args) = suite.command();
        content = content.child(text(format!("{} {} · runs project scripts in a new owned worktree at the combined committed input. Test failures remain visible and block dependent work.", program, args.join(" ")))
            .key("composer-test-command").font(DESCRIPTION).color(p.muted()).wrap());
    } else {
        content = content.child(
            text(
                "A committed result records the code change. It does not prove that tests passed.",
            )
            .font(DESCRIPTION)
            .color(p.muted())
            .wrap(),
        );
    }
    content
}

fn session_choice(p: Paint, ui: &Ui) -> El {
    let composer = &ui.agent.composer;
    let snapshot = ui
        .agent
        .snapshot
        .as_ref()
        .expect("composer requires a snapshot");
    let mut content = col()
        .key("composer-session")
        .gap(6.0)
        .child(text("Provider session").font(DESCRIPTION).color(p.text()));
    if let Some(session) = selected_session(snapshot, composer.first.provider) {
        content = content
            .child(segmented(
                p,
                "composer-session-choice",
                &["Fresh session", "Selected session"],
                usize::from(composer.use_selected_session),
                |ui: &mut Ui, index| ui.agent.composer.use_selected_session = index == 1,
            ))
            .child(
                text(format!("Selected: {}", session.title))
                    .font(DESCRIPTION)
                    .color(p.muted())
                    .wrap(),
            );
    } else {
        content = content.child(
            text("Starts a fresh session. Choose an existing session above to continue it.")
                .font(DESCRIPTION)
                .color(p.muted())
                .wrap(),
        );
    }
    content.child(text("Single-task Work uses the selected project directory. Review the provider's access before approving.")
        .font(DESCRIPTION).color(p.muted()).wrap())
}

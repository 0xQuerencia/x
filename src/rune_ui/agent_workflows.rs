//! The workflow manager inside the Agent pane: list, create, edit, test, duplicate and
//! delete saved workflows without touching a file. Everything saved goes through the same
//! store and validation as `x agent workflow import`, so the editor cannot make a workflow
//! the engine would refuse.

use ::x_agent::workflows::model::{OnFailure, Step, StepAction, Workflow};
use ::x_agent::workflows::store::Entry;
use rune::{El, TextEdit, col, row, text};
use rune_kit::TextInput;

use super::Ui;
use super::controls::{ButtonKind, DESCRIPTION, Paint, button};
use super::lists::section_label;
use crate::agent_host;
use crate::desktop::i18n::tr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Kind {
    OpenApp,
    OpenTerminal,
    OpenDirectory,
    OpenUrl,
    FocusWindow,
    SwitchWorkspace,
    MoveWindow,
    WaitForWindow,
    RunAction,
    Opencode,
}

const KINDS: [Kind; 10] = [
    Kind::OpenApp,
    Kind::OpenTerminal,
    Kind::OpenDirectory,
    Kind::OpenUrl,
    Kind::FocusWindow,
    Kind::SwitchWorkspace,
    Kind::MoveWindow,
    Kind::WaitForWindow,
    Kind::RunAction,
    Kind::Opencode,
];

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::OpenApp => "Open app",
            Self::OpenTerminal => "Open terminal in folder",
            Self::OpenDirectory => "Open folder",
            Self::OpenUrl => "Open web page",
            Self::FocusWindow => "Focus window",
            Self::SwitchWorkspace => "Switch workspace",
            Self::MoveWindow => "Move window",
            Self::WaitForWindow => "Wait for window",
            Self::RunAction => "Run X action",
            Self::Opencode => "OpenCode task",
        }
    }

    fn next(self) -> Self {
        let at = KINDS.iter().position(|k| *k == self).unwrap_or(0);
        KINDS[(at + 1) % KINDS.len()]
    }

    fn uses_app(self) -> bool {
        matches!(
            self,
            Self::OpenApp | Self::FocusWindow | Self::MoveWindow | Self::WaitForWindow
        )
    }

    fn uses_workspace(self) -> bool {
        matches!(
            self,
            Self::OpenApp | Self::OpenTerminal | Self::SwitchWorkspace | Self::MoveWindow
        )
    }

    /// Whether the workspace may be left unset (a placement is optional there).
    fn workspace_optional(self) -> bool {
        matches!(self, Self::OpenApp | Self::OpenTerminal)
    }

    fn text_hint(self) -> Option<&'static str> {
        match self {
            Self::OpenTerminal | Self::OpenDirectory => Some("~/projects/my-project"),
            Self::OpenUrl => Some("https://github.com"),
            Self::RunAction => Some("search github for rust lifetimes"),
            Self::Opencode => Some("What OpenCode should do"),
            _ => None,
        }
    }
}

pub(super) struct StepEd {
    id: String,
    kind: Kind,
    app: usize,
    /// 0 means "no workspace".
    workspace: u8,
    text: TextEdit,
    /// OpenCode's folder (its goal is `text`).
    folder: TextEdit,
}

impl StepEd {
    fn new(id: String) -> Self {
        Self {
            id,
            kind: Kind::OpenApp,
            app: 0,
            workspace: 0,
            text: TextEdit::default(),
            folder: TextEdit::default(),
        }
    }

    fn from(step: &Step) -> Self {
        let mut ed = Self::new(step.id.clone());
        let app_index = |app: &str| {
            ::x_agent::tools::APPS
                .iter()
                .position(|(n, _)| *n == app)
                .unwrap_or(0)
        };
        match &step.action {
            StepAction::OpenApp { app, workspace } => {
                ed.app = app_index(app);
                ed.workspace = workspace.unwrap_or(0);
            }
            StepAction::OpenTerminal {
                directory,
                workspace,
            } => {
                ed.kind = Kind::OpenTerminal;
                ed.text.text = directory.clone();
                ed.workspace = workspace.unwrap_or(0);
            }
            StepAction::OpenDirectory { directory } => {
                ed.kind = Kind::OpenDirectory;
                ed.text.text = directory.clone();
            }
            StepAction::OpenUrl { url } => {
                ed.kind = Kind::OpenUrl;
                ed.text.text = url.clone();
            }
            StepAction::FocusWindow { app, .. } => {
                ed.kind = Kind::FocusWindow;
                ed.app = app_index(app);
            }
            StepAction::SwitchWorkspace { workspace } => {
                ed.kind = Kind::SwitchWorkspace;
                ed.workspace = *workspace;
            }
            StepAction::MoveWindow { app, workspace, .. } => {
                ed.kind = Kind::MoveWindow;
                ed.app = app_index(app);
                ed.workspace = *workspace;
            }
            StepAction::WaitForWindow { app } => {
                ed.kind = Kind::WaitForWindow;
                ed.app = app_index(app);
            }
            StepAction::RunAction { phrase } => {
                ed.kind = Kind::RunAction;
                ed.text.text = phrase.clone();
            }
            StepAction::OpencodeTask { goal, directory } => {
                ed.kind = Kind::Opencode;
                ed.text.text = goal.clone();
                ed.folder.text = directory.clone();
            }
        }
        ed
    }

    fn app_name(&self) -> String {
        ::x_agent::tools::APPS
            .get(self.app)
            .map_or("vscode", |(n, _)| *n)
            .to_string()
    }

    fn to_step(&self, old: Option<&Step>) -> Step {
        let ws = (self.workspace != 0).then_some(self.workspace);
        let action = match self.kind {
            Kind::OpenApp => StepAction::OpenApp {
                app: self.app_name(),
                workspace: ws,
            },
            Kind::OpenTerminal => StepAction::OpenTerminal {
                directory: self.text.text.trim().into(),
                workspace: ws,
            },
            Kind::OpenDirectory => StepAction::OpenDirectory {
                directory: self.text.text.trim().into(),
            },
            Kind::OpenUrl => StepAction::OpenUrl {
                url: self.text.text.trim().into(),
            },
            Kind::FocusWindow => StepAction::FocusWindow {
                app: self.app_name(),
                pick: None,
            },
            Kind::SwitchWorkspace => StepAction::SwitchWorkspace {
                workspace: self.workspace.max(1),
            },
            Kind::MoveWindow => StepAction::MoveWindow {
                app: self.app_name(),
                workspace: self.workspace.max(1),
                pick: None,
            },
            Kind::WaitForWindow => StepAction::WaitForWindow {
                app: self.app_name(),
            },
            Kind::RunAction => StepAction::RunAction {
                phrase: self.text.text.trim().into(),
            },
            Kind::Opencode => StepAction::OpencodeTask {
                goal: self.text.text.trim().into(),
                directory: self.folder.text.trim().into(),
            },
        };
        // Settings the editor has no control for (timeouts, requirements) survive a save.
        let (requires, timeout_secs, on_failure) = old
            .map(|o| (o.requires.clone(), o.timeout_secs, o.on_failure))
            .unwrap_or((vec![], None, OnFailure::Stop));
        Step {
            id: self.id.clone(),
            action,
            requires,
            timeout_secs,
            on_failure,
        }
    }
}

pub(super) struct WfEditor {
    id: String,
    is_new: bool,
    name: TextEdit,
    triggers: TextEdit,
    steps: Vec<StepEd>,
    /// The saved steps, so settings the editor does not show are kept.
    saved: Vec<Step>,
    problems: Vec<String>,
}

fn fresh_id(taken: &[String], name: &str) -> String {
    let base: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(30)
        .collect();
    let base = if base.is_empty() {
        "workflow".to_string()
    } else {
        base
    };
    (1..)
        .map(|n| {
            if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            }
        })
        .find(|c| !taken.contains(c))
        .unwrap_or(base)
}

impl WfEditor {
    fn blank(taken: &[String]) -> Self {
        let mut name = TextEdit::default();
        name.text = "My workflow".into();
        Self {
            id: fresh_id(taken, "workflow"),
            is_new: true,
            name,
            triggers: TextEdit::default(),
            steps: vec![StepEd::new("step-1".into())],
            saved: vec![],
            problems: vec![],
        }
    }

    fn open(wf: &Workflow) -> Self {
        let mut name = TextEdit::default();
        name.text = wf.name.clone();
        let mut triggers = TextEdit::default();
        triggers.text = wf.triggers.join(", ");
        Self {
            id: wf.id.clone(),
            is_new: false,
            name,
            triggers,
            steps: wf.steps.iter().map(StepEd::from).collect(),
            saved: wf.steps.clone(),
            problems: vec![],
        }
    }

    fn build(&self) -> Workflow {
        Workflow {
            id: self.id.clone(),
            name: self.name.text.trim().to_string(),
            triggers: self
                .triggers
                .text
                .split([',', ';'])
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(String::from)
                .collect(),
            steps: self
                .steps
                .iter()
                .map(|s| s.to_step(self.saved.iter().find(|o| o.id == s.id)))
                .collect(),
        }
    }

    fn next_step_id(&self) -> String {
        let taken: Vec<String> = self.steps.iter().map(|s| s.id.clone()).collect();
        (1..)
            .map(|n| format!("step-{n}"))
            .find(|c| !taken.contains(c))
            .unwrap_or_default()
    }
}

/// A workflow as listed, with its last result, read from disk by [`Manager::refresh`] and
/// not on every redraw.
pub(super) struct Row {
    entry: Entry,
    last: Option<::x_agent::workflows::Report>,
}

#[derive(Default)]
pub(super) struct Manager {
    rows: Vec<Row>,
    loaded: Option<std::time::Instant>,
    /// Set by anything that changes the files, so the list reloads at once.
    dirty: bool,
    shown: String,
    editor: Option<WfEditor>,
    /// Shown under the list: the outcome of the last Save, Delete or Duplicate.
    note: Option<(bool, String)>,
    /// A TextEdit to hand out when a closure outlives the editor it indexes.
    spare: TextEdit,
}

impl Manager {
    /// Reloads the list when something changed it or a few seconds passed (a run may have
    /// finished). True when what is shown differs.
    pub(super) fn refresh(&mut self) -> bool {
        let stale = self
            .loaded
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(3));
        if !self.dirty && !stale {
            return false;
        }
        let store = store();
        let rows: Vec<Row> = store
            .list()
            .into_iter()
            .map(|entry| Row {
                last: store.last(&entry.workflow.id),
                entry,
            })
            .collect();
        let shown = serde_json::to_string(
            &rows
                .iter()
                .map(|r| {
                    (
                        &r.entry.workflow,
                        r.entry.problems.len(),
                        &r.entry.conflicts,
                        r.last.as_ref().map(|l| &l.summary),
                    )
                })
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        self.dirty = false;
        self.loaded = Some(std::time::Instant::now());
        self.rows = rows;
        let changed = shown != self.shown;
        self.shown = shown;
        changed
    }
}

fn store() -> ::x_agent::workflows::Store {
    agent_host::service().workflow_store()
}

fn note(ui: &mut Ui, ok: bool, message: String) {
    ui.agent.workflows.note = Some((ok, message));
    ui.agent.workflows.dirty = true;
}

fn save(ui: &mut Ui) -> Option<String> {
    let wf = ui.agent.workflows.editor.as_ref()?.build();
    let saved = store().save(&wf);
    ui.agent.workflows.dirty = true;
    let editor = ui.agent.workflows.editor.as_mut()?;
    match saved {
        Ok(()) => {
            editor.problems.clear();
            editor.is_new = false;
            Some(wf.id)
        }
        Err(problems) => {
            editor.problems = problems.iter().map(ToString::to_string).collect();
            None
        }
    }
}

fn input<'a>(
    p: Paint,
    edit: &'a TextEdit,
    key: String,
    hint: &str,
    pick: impl for<'b> Fn(&'b mut Ui) -> &'b mut TextEdit + 'static,
) -> El {
    TextInput::new(edit)
        .placeholder(hint.to_string())
        .width(None)
        .on_edit(pick)
        .build(p.k)
        .key(key)
}

fn small(key: String, label: String, color: rune::Color) -> El {
    text(label).key(key).font(DESCRIPTION).color(color).wrap()
}

pub(super) fn view(p: Paint, ui: &Ui) -> El {
    let mgr = &ui.agent.workflows;
    let mut body = col().key("wf-body").gap(16.0);
    match &mgr.editor {
        Some(editor) => body = body.child(editor_card(p, editor)),
        None => body = body.child(list(p, mgr)),
    }
    body
}

fn list(p: Paint, mgr: &Manager) -> El {
    let taken: Vec<String> = mgr
        .rows
        .iter()
        .map(|r| r.entry.workflow.id.clone())
        .collect();
    let mut list = col().key("wf-list").gap(8.0).child(
        row()
            .key("wf-head")
            .gap(8.0)
            .center_y()
            .child(section_label(p, "wf-title", tr("Saved workflows")))
            .child(button(
                p,
                "wf-new",
                tr("New workflow"),
                ButtonKind::Primary,
                move |ui: &mut Ui, _| {
                    ui.agent.workflows.note = None;
                    ui.agent.workflows.editor = Some(WfEditor::blank(&taken));
                },
            )),
    );
    if mgr.rows.is_empty() {
        list = list.child(small("wf-empty".into(),
            tr("No workflows yet. Create one, then say its trigger phrase, such as \"X, start coding\".").into(),
            p.muted(),
        ));
    }
    for item in &mgr.rows {
        list = list.child(entry_card(p, item));
    }
    if let Some((ok, message)) = &mgr.note {
        list = list.child(small(
            "wf-note".into(),
            message.clone(),
            if *ok { p.success() } else { p.danger() },
        ));
    }
    list
}

fn entry_card(p: Paint, item: &Row) -> El {
    let entry = &item.entry;
    let wf = &entry.workflow;
    let id = wf.id.clone();
    let key = |s: &str| format!("wf-{id}-{s}");
    let triggers = if wf.triggers.is_empty() {
        tr("No trigger phrase").to_string()
    } else {
        wf.triggers
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut card = col()
        .key(key("card"))
        .gap(6.0)
        .px(16.0)
        .py(12.0)
        .radius(p.k.radius.md)
        .fill(p.k.c.layer_01)
        .child(
            text(wf.name.clone())
                .key(key("name"))
                .font(p.k.ty.small_medium)
                .color(p.text()),
        )
        .child(small(
            key("triggers"),
            format!("{triggers}  ·  {} {}", wf.steps.len(), tr("steps")),
            p.muted(),
        ));
    for problem in &entry.problems {
        card = card.child(small(key("problem"), problem.to_string(), p.danger()));
    }
    if !entry.conflicts.is_empty() {
        card = card.child(small(
            key("conflict"),
            format!(
                "{} {}",
                tr("Shares a trigger phrase with"),
                entry.conflicts.join(", ")
            ),
            p.warning(),
        ));
    }
    if let Some(last) = &item.last {
        use ::x_agent::workflows::RunState;
        let color = match last.state {
            RunState::Completed => p.success(),
            RunState::Failed => p.danger(),
            _ => p.warning(),
        };
        card = card.child(small(
            key("last"),
            format!("{} {}", tr("Last run:"), last.summary),
            color,
        ));
        if last.state != RunState::Completed {
            for step in last.steps.iter().filter(|s| !s.detail.is_empty()) {
                card = card.child(small(
                    format!("wf-{id}-log-{}", step.id),
                    format!("   {:?}  {}: {}", step.status, step.id, step.detail),
                    p.muted(),
                ));
            }
        }
    }
    let runnable = entry.runnable();
    let (edit_id, run_id, dup_id, del_id) = (id.clone(), id.clone(), id.clone(), id.clone());
    card.child(
        row()
            .key(key("actions"))
            .gap(8.0)
            .child(button(
                p,
                &key("edit"),
                tr("Edit"),
                ButtonKind::Secondary,
                move |ui: &mut Ui, _| {
                    ui.agent.workflows.note = None;
                    if let Ok(wf) = store().get(&edit_id) {
                        ui.agent.workflows.editor = Some(WfEditor::open(&wf));
                    }
                },
            ))
            .child(button(
                p,
                &key("run"),
                tr("Test"),
                if runnable {
                    ButtonKind::Primary
                } else {
                    ButtonKind::Secondary
                },
                move |ui: &mut Ui, _| {
                    let id = run_id.clone();
                    let outcome = agent_host::service().plan_workflow_by_id(&id);
                    note(
                        ui,
                        outcome.is_ok(),
                        outcome.map_or_else(
                            |e| e,
                            |_| tr("Planned: review it above and approve").to_string(),
                        ),
                    );
                },
            ))
            .child(button(
                p,
                &key("dup"),
                tr("Duplicate"),
                ButtonKind::Secondary,
                move |ui: &mut Ui, _| {
                    let result = store().duplicate(&dup_id);
                    note(
                        ui,
                        result.is_ok(),
                        result.map_or_else(|e| e, |c| format!("{} {}", tr("Created"), c.name)),
                    );
                },
            ))
            .child(button(
                p,
                &key("del"),
                tr("Delete"),
                ButtonKind::Danger,
                move |ui: &mut Ui, _| {
                    let result = store().delete(&del_id);
                    note(
                        ui,
                        result.is_ok(),
                        result.map_or_else(|e| e, |()| tr("Deleted").to_string()),
                    );
                },
            )),
    )
}

fn editor_card(p: Paint, ed: &WfEditor) -> El {
    let mut card = col()
        .key("wf-editor")
        .gap(10.0)
        .px(16.0)
        .py(16.0)
        .radius(p.k.radius.md)
        .fill(p.k.c.layer_01)
        .child(section_label(
            p,
            "wf-editor-title",
            if ed.is_new {
                tr("New workflow")
            } else {
                tr("Edit workflow")
            },
        ))
        .child(input(
            p,
            &ed.name,
            "wf-name".into(),
            tr("Name, such as Start coding"),
            |ui: &mut Ui| match ui.agent.workflows.editor.as_mut() {
                Some(e) => &mut e.name,
                None => &mut ui.agent.workflows.spare,
            },
        ))
        .child(input(
            p,
            &ed.triggers,
            "wf-triggers".into(),
            tr("Trigger phrases, separated by commas: start coding, coding setup"),
            |ui: &mut Ui| match ui.agent.workflows.editor.as_mut() {
                Some(e) => &mut e.triggers,
                None => &mut ui.agent.workflows.spare,
            },
        ));
    let count = ed.steps.len();
    for (i, step) in ed.steps.iter().enumerate() {
        card = card.child(step_row(p, step, i, count));
    }
    card = card.child(button(
        p,
        "wf-add-step",
        tr("Add step"),
        ButtonKind::Secondary,
        |ui: &mut Ui, _| {
            if let Some(e) = ui.agent.workflows.editor.as_mut() {
                let id = e.next_step_id();
                e.steps.push(StepEd::new(id));
            }
        },
    ));
    for (n, problem) in ed.problems.iter().enumerate() {
        card = card.child(small(
            format!("wf-problem-{n}"),
            problem.clone(),
            p.danger(),
        ));
    }
    card.child(
        row()
            .key("wf-editor-actions")
            .gap(8.0)
            .child(button(
                p,
                "wf-save",
                tr("Save"),
                ButtonKind::Primary,
                |ui: &mut Ui, _| {
                    if save(ui).is_some() {
                        ui.agent.workflows.editor = None;
                        note(ui, true, tr("Saved").to_string());
                    }
                },
            ))
            .child(button(
                p,
                "wf-test",
                tr("Save and test"),
                ButtonKind::Secondary,
                |ui: &mut Ui, _| {
                    if let Some(id) = save(ui) {
                        let outcome = agent_host::service().plan_workflow_by_id(&id);
                        if let Err(error) = outcome {
                            if let Some(e) = ui.agent.workflows.editor.as_mut() {
                                e.problems = vec![error];
                            }
                        } else {
                            ui.agent.workflows.editor = None;
                            note(
                                ui,
                                true,
                                tr("Planned: review it above and approve").to_string(),
                            );
                        }
                    }
                },
            ))
            .child(button(
                p,
                "wf-cancel",
                tr("Close"),
                ButtonKind::Secondary,
                |ui: &mut Ui, _| {
                    ui.agent.workflows.editor = None;
                },
            )),
    )
}

fn with_step(ui: &mut Ui, i: usize, f: impl FnOnce(&mut StepEd)) {
    if let Some(step) = ui
        .agent
        .workflows
        .editor
        .as_mut()
        .and_then(|e| e.steps.get_mut(i))
    {
        f(step);
    }
}

fn step_row(p: Paint, step: &StepEd, i: usize, count: usize) -> El {
    let key = |s: &str| format!("wf-step{i}-{s}");
    let kind = step.kind;
    let mut line = row().key(key("row")).gap(8.0).center_y().child(
        text(format!("{}.", i + 1))
            .key(key("n"))
            .font(DESCRIPTION)
            .color(p.muted()),
    );
    line = line.child(button(
        p,
        &key("kind"),
        tr(kind.label()),
        ButtonKind::Secondary,
        move |ui: &mut Ui, _| {
            with_step(ui, i, |s| s.kind = s.kind.next());
        },
    ));
    if kind.uses_app() {
        line = line.child(button(
            p,
            &key("app"),
            &step.app_name(),
            ButtonKind::Secondary,
            move |ui: &mut Ui, _| {
                with_step(ui, i, |s| {
                    s.app = (s.app + 1) % ::x_agent::tools::APPS.len()
                });
            },
        ));
    }
    if kind.uses_workspace() {
        let optional = kind.workspace_optional();
        let label = if step.workspace == 0 {
            tr("no workspace").to_string()
        } else {
            format!("{} {}", tr("workspace"), step.workspace)
        };
        line = line
            .child(button(
                p,
                &key("ws-"),
                "−",
                ButtonKind::Secondary,
                move |ui: &mut Ui, _| {
                    with_step(ui, i, |s| {
                        let floor = if optional { 0 } else { 1 };
                        s.workspace = s.workspace.saturating_sub(1).max(floor);
                    });
                },
            ))
            .child(text(label).key(key("ws")).font(DESCRIPTION).color(p.text()))
            .child(button(
                p,
                &key("ws+"),
                "+",
                ButtonKind::Secondary,
                move |ui: &mut Ui, _| {
                    with_step(ui, i, |s| s.workspace = (s.workspace + 1).min(10));
                },
            ));
    }
    if let Some(hint) = kind.text_hint() {
        line = line.child(input(
            p,
            &step.text,
            key("text"),
            tr(hint),
            move |ui: &mut Ui| match ui
                .agent
                .workflows
                .editor
                .as_mut()
                .and_then(|e| e.steps.get_mut(i))
            {
                Some(s) => &mut s.text,
                None => &mut ui.agent.workflows.spare,
            },
        ));
    }
    if kind == Kind::Opencode {
        line = line.child(input(
            p,
            &step.folder,
            key("folder"),
            tr("~/projects/my-project"),
            move |ui: &mut Ui| match ui
                .agent
                .workflows
                .editor
                .as_mut()
                .and_then(|e| e.steps.get_mut(i))
            {
                Some(s) => &mut s.folder,
                None => &mut ui.agent.workflows.spare,
            },
        ));
    }
    line.child(button(
        p,
        &key("up"),
        "↑",
        ButtonKind::Secondary,
        move |ui: &mut Ui, _| {
            if i > 0
                && let Some(e) = ui.agent.workflows.editor.as_mut()
            {
                e.steps.swap(i, i - 1);
            }
        },
    ))
    .child(button(
        p,
        &key("down"),
        "↓",
        ButtonKind::Secondary,
        move |ui: &mut Ui, _| {
            if i + 1 < count
                && let Some(e) = ui.agent.workflows.editor.as_mut()
            {
                e.steps.swap(i, i + 1);
            }
        },
    ))
    .child(button(
        p,
        &key("rm"),
        "✕",
        ButtonKind::Danger,
        move |ui: &mut Ui, _| {
            if let Some(e) = ui.agent.workflows.editor.as_mut() {
                if i < e.steps.len() && e.steps.len() > 1 {
                    e.steps.remove(i);
                }
            }
        },
    ))
}

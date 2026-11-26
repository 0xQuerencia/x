//! The Agent pane (Labs › X agent): speak or type a goal, read the plan, approve it, watch
//! it run, and Stop it. Everything real lives in `crates/x-agent`; this file draws its
//! snapshot and forwards presses to the same `hex agent` handler the CLI uses.

mod composer;
mod dialogue;
mod edit;
mod quick;
mod results;
mod runs;
mod workflows;

#[cfg(test)]
mod visual_tests;

use ::x_agent::phase::Phase;
use ::x_agent::service::{ActionCompletion, Mode, Snapshot, Task};
use rune::{El, TextEdit, col, row, text};
use rune_kit::TextInput;

use super::Ui;
use super::controls::{ButtonKind, DESCRIPTION, Paint, button, status_dot};
use super::lists::{message, section_label};
use crate::agent_host;
use crate::control::AgentAction;
use crate::desktop::i18n::tr;

const SHOWN_EVENTS: usize = 40;

pub(super) struct AgentPane {
    composer: composer::Composer,
    dialogue: dialogue::Dialogue,
    input: TextEdit,
    quick_query: TextEdit,
    snippet_trigger: TextEdit,
    snippet_value: TextEdit,
    snippet_note: Option<String>,
    quick_notice: Option<String>,
    quick_file_preview: Option<String>,
    pending_process: Option<::x_agent::quick::processes::ProcessInfo>,
    workspace: TextEdit,
    browser_url: TextEdit,
    browser_title: TextEdit,
    session_choices: Vec<(String, String)>,
    session_message: Option<String>,
    project_choices: Vec<(String, String)>,
    project_page: usize,
    project_message: Option<String>,
    test_project: TextEdit,
    test_suite_index: usize,
    recovery_summary: Option<String>,
    results: results::Results,
    mode: Mode,
    snapshot: Option<Snapshot>,
    /// Serialized form of the last snapshot, so polling redraws only on change.
    shown: String,
    /// The eight-second window Undo affordance is driven by monotonic time.
    undo_visible: bool,
    /// Whether the workflow manager is showing instead of the command view.
    pub(super) show_workflows: bool,
    pub(super) show_quick: bool,
    pub(super) workflows: workflows::Manager,
}

impl Default for AgentPane {
    fn default() -> Self {
        Self {
            composer: Default::default(),
            dialogue: Default::default(),
            input: TextEdit::default(),
            quick_query: TextEdit::default(),
            snippet_trigger: TextEdit::default(),
            snippet_value: TextEdit::default(),
            snippet_note: None,
            quick_notice: None,
            quick_file_preview: None,
            pending_process: None,
            workspace: TextEdit::default(),
            browser_url: TextEdit::default(),
            browser_title: TextEdit::default(),
            session_choices: Vec::new(),
            session_message: None,
            project_choices: Vec::new(),
            project_page: 0,
            project_message: None,
            test_project: TextEdit::default(),
            test_suite_index: 0,
            recovery_summary: None,
            results: Default::default(),
            mode: Mode::Actions,
            snapshot: None,
            shown: String::new(),
            undo_visible: false,
            show_workflows: false,
            show_quick: false,
            workflows: Default::default(),
        }
    }
}

impl AgentPane {
    pub(super) fn is_busy(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.phase.is_busy())
    }

    /// Reads X's state while the pane shows; true when it changed.
    pub(super) fn poll(&mut self, showing: bool) -> bool {
        if !showing || !crate::labs::x_agent() {
            return false;
        }
        let snapshot = agent_host::service().snapshot(0);
        let composed = self.composer.poll(&snapshot);
        let answered = self.dialogue.poll(&snapshot);
        let reviewed = self.results.poll();
        let serialized = serde_json::to_string(&snapshot).unwrap_or_default();
        let listed = (self.show_workflows || self.show_quick) && self.workflows.refresh();
        let can_undo = snapshot.phase == Phase::Completed
            && (::x_agent::wm::recent_undo_available() || ::x_agent::sys::recent_undo_available());
        let undo_changed = can_undo != self.undo_visible;
        self.undo_visible = can_undo;
        let native_review_changed = self.snapshot.as_ref().is_some_and(|previous| {
            previous.permission_waits != snapshot.permission_waits
                || previous
                    .background_runs
                    .iter()
                    .zip(&snapshot.background_runs)
                    .any(|(old, current)| old.permission_waits != current.permission_waits)
        });
        if serialized == self.shown && !native_review_changed {
            return listed || undo_changed || composed || answered || reviewed;
        }
        if self.workspace.text.is_empty()
            && let Some(path) = &snapshot.workspace
        {
            self.workspace.text = path.clone();
        }
        self.shown = serialized;
        self.snapshot = Some(snapshot);
        true
    }
}

fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::Idle => tr("Ready"),
        Phase::Listening => tr("Listening"),
        Phase::Transcribing => tr("Transcribing"),
        Phase::Planning => tr("Planning"),
        Phase::AwaitingApproval => tr("Needs your approval"),
        Phase::WaitingForInput => tr("Needs your answer"),
        Phase::Running => tr("Running"),
        Phase::Completed => tr("Finished"),
        Phase::Failed => tr("Failed"),
        Phase::Cancelled => tr("Cancelled"),
    }
}

fn outcome_label(outcome: ActionCompletion) -> &'static str {
    match outcome {
        ActionCompletion::Confirmed => "Confirmed",
        ActionCompletion::Launched => "Launched · effect unverified",
        ActionCompletion::Observed => "Observed · goal unverified",
        ActionCompletion::Partial => "Finished · some effects unverified",
        ActionCompletion::Unverified => "Effect unverified",
        ActionCompletion::Failed => "Failed",
        ActionCompletion::Cancelled => "Cancelled",
    }
}

fn task_label(task: &Task) -> &'static str {
    task.action_outcome()
        .map_or_else(|| phase_label(task.phase), outcome_label)
}

fn step_glyph(status: &str) -> &'static str {
    match status {
        "running" => "▸",
        "stopping" => "…",
        "delivered" | "launched" => "→",
        "done" | "tested" => "✓",
        "failed" => "✕",
        _ => "○",
    }
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Actions => "actions",
        Mode::Work => "work",
    }
}

pub(super) fn pane(ui: &Ui, p: Paint) -> (Vec<El>, El) {
    let pane = &ui.agent;
    if !crate::labs::x_agent() {
        return (
            Vec::new(),
            col().key("agent-pane").grow(1.0).child(message(
                p,
                "agent-off",
                tr("X is off"),
                Some(tr(
                    "Turn it on in Settings › Labs › X agent. Ordinary dictation is unaffected.",
                )),
            )),
        );
    }
    let Some(snapshot) = &pane.snapshot else {
        return (Vec::new(), col().key("agent-pane").grow(1.0));
    };
    let busy = snapshot.phase.is_busy();
    let tone = match snapshot.action_outcome {
        Some(ActionCompletion::Confirmed) => p.success(),
        Some(ActionCompletion::Failed) => p.danger(),
        Some(
            ActionCompletion::Launched
            | ActionCompletion::Observed
            | ActionCompletion::Partial
            | ActionCompletion::Unverified,
        ) => p.warning(),
        Some(ActionCompletion::Cancelled) => p.muted(),
        None => match snapshot.phase {
            Phase::Failed => p.danger(),
            Phase::AwaitingApproval | Phase::WaitingForInput => p.warning(),
            Phase::Running | Phase::Listening | Phase::Transcribing | Phase::Planning => p.accent(),
            Phase::Completed | Phase::Idle | Phase::Cancelled => p.muted(),
        },
    };
    let mut status = row()
        .key("agent-status")
        .gap(8.0)
        .center_y()
        .child(status_dot(tone))
        .child(
            text(
                snapshot
                    .action_outcome
                    .map_or_else(|| phase_label(snapshot.phase), outcome_label),
            )
            .key("phase")
            .shrink()
            .font(DESCRIPTION)
            .color(p.text()),
        );
    match snapshot.pending_job_approvals {
        Some(count) if count > 0 => {
            status = status.child(
                text(format!("{count} unspent job grants"))
                    .key("agent-job-grants")
                    .font(DESCRIPTION)
                    .color(p.warning()),
            );
        }
        None => {
            status = status.child(
                text("Job approval status unavailable")
                    .key("agent-grants-error")
                    .font(DESCRIPTION)
                    .color(p.danger()),
            );
        }
        _ => {}
    }

    let mode_button = |key: &'static str, label: &str, mode: Mode| {
        let kind = if pane.mode == mode {
            ButtonKind::Primary
        } else {
            ButtonKind::Secondary
        };
        button(p, key, label, kind, move |ui: &mut Ui, _| {
            if !ui
                .agent
                .snapshot
                .as_ref()
                .is_some_and(|s| s.phase.is_busy())
            {
                ui.agent.mode = mode;
            }
        })
    };
    let view_button = |key: &'static str, label: &str, workflows: bool, quick: bool| {
        let kind = if pane.show_workflows == workflows && pane.show_quick == quick {
            ButtonKind::Primary
        } else {
            ButtonKind::Secondary
        };
        button(p, key, label, kind, move |ui: &mut Ui, _| {
            ui.agent.show_workflows = workflows;
            ui.agent.show_quick = quick;
        })
    };
    let actions = vec![
        view_button("view-commands", tr("Commands"), false, false),
        view_button("view-quick", tr("Quick"), false, true),
        view_button("view-workflows", tr("Workflows"), true, false),
    ];
    let voice = agent_host::voice_mode();
    let voice_modes = [
        agent_host::VoiceMode::Dictate,
        agent_host::VoiceMode::Actions,
        agent_host::VoiceMode::Work,
        agent_host::VoiceMode::Auto,
        agent_host::VoiceMode::Smart,
    ];
    let mut voice_row = row()
        .key("agent-voice-mode")
        .gap(6.0)
        .center_y()
        .child(text(tr("Shortcut mode")).font(DESCRIPTION).color(p.muted()));
    for mode in voice_modes {
        let selected = mode == voice;
        let name = mode.name();
        voice_row = voice_row.child(button(
            p,
            &format!("agent-voice-{name}"),
            &format!("{}{}", name[..1].to_uppercase(), &name[1..]),
            if selected {
                ButtonKind::Primary
            } else {
                ButtonKind::Secondary
            },
            move |_: &mut Ui, _| {
                agent_host::handle(AgentAction::Voice { mode: name.into() });
            },
        ));
    }
    // Execution policy is separate from the shortcut's voice-routing Auto
    // mode. Default asks; Autonomous skips prompts only for permitted safe
    // desktop Actions. The existing trust bit is the one source of truth.
    let autonomous = snapshot.trusted_actions;
    let autonomy_control = row()
        .key("agent-autonomy-row")
        .gap(8.0)
        .center_y()
        .child(text(tr("Execution")).font(DESCRIPTION).color(p.muted()))
        .child(button(
            p,
            "agent-mode-default",
            "Default",
            if autonomous {
                ButtonKind::Secondary
            } else {
                ButtonKind::Primary
            },
            move |_: &mut Ui, _| {
                agent_host::handle(AgentAction::Autonomy {
                    mode: "default".into(),
                });
            },
        ))
        .child(button(
            p,
            "agent-mode-autonomous",
            "Autonomous",
            if autonomous {
                ButtonKind::Primary
            } else {
                ButtonKind::Secondary
            },
            move |_: &mut Ui, _| {
                agent_host::handle(AgentAction::Autonomy {
                    mode: "autonomous".into(),
                });
            },
        ));

    if pane.show_quick {
        let mut body = col().key("agent-body").gap(16.0).child(status);
        if let Some(task) = &snapshot.task {
            body = body.child(task_card(p, pane, snapshot, task));
        }
        body = body
            .child(runs::panel(p, snapshot))
            .child(quick::view(p, ui));
        return (
            actions,
            col()
                .key("agent-pane")
                .grow(1.0)
                .shrink()
                .px(24.0)
                .py(16.0)
                .child(body),
        );
    }
    if pane.show_workflows {
        let mut body = col().key("agent-body").gap(16.0).child(status);
        if let Some(task) = &snapshot.task {
            body = body.child(task_card(p, pane, snapshot, task));
        }
        body = body
            .child(runs::panel(p, snapshot))
            .child(workflows::view(p, ui));
        return (
            actions,
            col()
                .key("agent-pane")
                .grow(1.0)
                .shrink()
                .px(24.0)
                .py(16.0)
                .child(body),
        );
    }
    let input = TextInput::new(&pane.input)
        .placeholder(if pane.mode == Mode::Actions {
            tr("Open Firefox and search YouTube for Mars rover")
        } else {
            tr("Describe what OpenCode should do in the workspace")
        })
        .width(None)
        .on_edit(|ui: &mut Ui| &mut ui.agent.input)
        .build(p.k)
        .key("agent-input")
        .w(0.0)
        .grow(1.0)
        .shrink();
    let listening = snapshot.phase == Phase::Listening;
    let capture = row()
        .key("agent-capture")
        .gap(8.0)
        .center_y()
        .child(input)
        .child(button(
            p,
            "agent-plan",
            tr("Plan"),
            ButtonKind::Primary,
            |ui: &mut Ui, _| {
                let (text, mode) = (ui.agent.input.text.clone(), ui.agent.mode);
                std::thread::spawn(move || {
                    if let Err(error) = agent_host::service().plan(&text, mode) {
                        agent_host::service().emit("error", &error);
                    }
                });
            },
        ))
        .child(button(
            p,
            "agent-listen",
            if listening {
                tr("Stop listening")
            } else {
                tr("Speak")
            },
            ButtonKind::Secondary,
            |ui: &mut Ui, _| {
                let mode = mode_name(ui.agent.mode).to_string();
                agent_host::handle(AgentAction::Toggle { mode });
            },
        ));

    let capture = if matches!(snapshot.phase, Phase::Listening | Phase::Transcribing) {
        capture.child(button(
            p,
            "agent-cancel",
            tr("Cancel"),
            ButtonKind::Secondary,
            |_: &mut Ui, _| {
                agent_host::handle(AgentAction::StopForeground);
            },
        ))
    } else {
        capture
    };
    let mode_row = row()
        .key("agent-command-mode")
        .gap(8.0)
        .center_y()
        .child(mode_button("mode-actions", tr("Actions"), Mode::Actions))
        .child(mode_button("mode-work", tr("Work"), Mode::Work))
        .child(status);
    let mut body = col()
        .key("agent-body")
        .gap(16.0)
        .child(mode_row)
        .child(capture);
    if let Some(task) = &snapshot.task {
        body = body.child(task_card(p, pane, snapshot, task));
    }
    body = body.child(runs::panel(p, snapshot));
    body = body
        .child(voice_row)
        .child(autonomy_control)
        .child(text(tr("Autonomous: safe Actions and granted apps run without repeated approval. Work and protected actions still ask."))
            .font(DESCRIPTION)
            .color(p.muted())
            .wrap());
    body = body.child(access_row(p, snapshot));
    if pane.mode == Mode::Work {
        body = body.child(workspace_row(p, pane, busy));
        body = body.child(registered_projects_row(p, pane, busy));
        body = body.child(session_picker_row(p, pane, busy));
        body = body.child(composer::view(p, ui));
        body = body.child(browser_page_handoff_row(p, pane, busy));
        body = body.child(project_tests_row(p, pane, busy));
        body = body.child(recovery_review_row(p, pane));
        body = body.child(results::panel(p, &pane.results));
    }
    if snapshot.task.is_none() {
        body = body.child(message(
            p,
            "agent-empty",
            tr("Nothing planned"),
            Some(tr(if autonomous {
                "Type a goal or press Speak. Safe Actions run automatically; protected tasks still ask."
            } else {
                "Type a goal or press Speak. You review the plan before anything runs."
            })),
        ));
    }
    body = body.child(event_stream(p, snapshot));
    (
        actions,
        col()
            .key("agent-pane")
            .grow(1.0)
            .shrink()
            .px(24.0)
            .py(16.0)
            .child(super::list_detail::card_scroll(
                "agent-command-scroll",
                body,
            )),
    )
}

fn access_row(p: Paint, snapshot: &Snapshot) -> El {
    let mut access = row().key("agent-access").gap(8.0).center_y().child(
        text(format!(
            "{}{}",
            tr("Screen access"),
            if snapshot.computer_remote {
                " (remote)"
            } else {
                ""
            }
        ))
        .key("access-label")
        .font(DESCRIPTION)
        .color(p.muted()),
    );
    for app in &snapshot.computer_apps {
        let name = app.clone();
        access = access.child(button(
            p,
            &format!("agent-revoke-{app}"),
            &format!("{app} · {}", tr("Revoke")),
            ButtonKind::Secondary,
            move |_: &mut Ui, _| {
                agent_host::handle(AgentAction::Computer {
                    command: "revoke".into(),
                    value: name.clone(),
                });
            },
        ));
    }
    for app in ["chrome", "firefox"] {
        if !snapshot.computer_apps.iter().any(|granted| granted == app) {
            let name = app.to_string();
            access = access.child(button(
                p,
                &format!("agent-grant-{app}"),
                &format!("{app} · Grant"),
                ButtonKind::Secondary,
                move |_: &mut Ui, _| {
                    agent_host::handle(AgentAction::Computer {
                        command: "grant".into(),
                        value: name.clone(),
                    });
                },
            ));
        }
    }
    let remote = snapshot.computer_remote;
    access = access.child(button(
        p,
        "agent-computer-remote",
        if remote {
            "Remote models on"
        } else {
            "Remote models off"
        },
        ButtonKind::Secondary,
        move |_: &mut Ui, _| {
            agent_host::handle(AgentAction::Computer {
                command: "remote".into(),
                value: if remote { "off" } else { "on" }.into(),
            });
        },
    ));
    access
}

fn workspace_row(p: Paint, pane: &AgentPane, busy: bool) -> El {
    let field = TextInput::new(&pane.workspace)
        .placeholder(tr("Project folder OpenCode may change"))
        .width(None)
        .on_edit(|ui: &mut Ui| &mut ui.agent.workspace)
        .build(p.k)
        .key("agent-workspace")
        .w(0.0)
        .grow(1.0)
        .shrink();
    row()
        .key("agent-workspace-row")
        .gap(8.0)
        .center_y()
        .child(field)
        .child(button(
            p,
            "agent-set-workspace",
            tr("Set folder"),
            if busy {
                ButtonKind::Secondary
            } else {
                ButtonKind::Primary
            },
            |ui: &mut Ui, _| {
                let path = ui.agent.workspace.text.clone();
                if let Err(error) = agent_host::service().set_workspace(&path) {
                    agent_host::service().emit("error", &error);
                }
            },
        ))
}

fn browser_page_handoff_row(p: Paint, pane: &AgentPane, busy: bool) -> El {
    let url = TextInput::new(&pane.browser_url)
        .placeholder(tr("https://example.com/page"))
        .width(None)
        .on_edit(|ui: &mut Ui| &mut ui.agent.browser_url)
        .build(p.k)
        .key("agent-browser-url");
    let title = TextInput::new(&pane.browser_title)
        .placeholder(tr("Optional page title"))
        .width(None)
        .on_edit(|ui: &mut Ui| &mut ui.agent.browser_title)
        .build(p.k)
        .key("agent-browser-title");
    col()
        .key("agent-browser-handoff")
        .gap(6.0)
        .child(text(tr("Share browser page with Work")).font(DESCRIPTION).color(p.text()))
        .child(
            row()
                .gap(8.0)
                .center_y()
                .child(url)
                .child(title),
        )
        .child(button(
            p,
            "agent-browser-handoff-review",
            tr("Review page handoff"),
            if busy {
                ButtonKind::Secondary
            } else {
                ButtonKind::Primary
            },
            |ui: &mut Ui, _| {
                if ui.agent.is_busy() {
                    return;
                }
                let url = ui.agent.browser_url.text.trim().to_owned();
                let title = ui.agent.browser_title.text.trim().to_owned();
                if let Err(error) =
                    agent_host::service().plan_browser_page_to_session(&url, &title)
                {
                    agent_host::service().emit("error", &error);
                }
            },
        ))
        .child(
            text(tr("Only the URL and title you type are shared with the selected session after approval. X does not read the browser or open the page."))
                .font(DESCRIPTION)
                .color(p.muted()),
        )
}

fn session_picker_row(p: Paint, pane: &AgentPane, busy: bool) -> El {
    let mut list = col()
        .key("agent-opencode-sessions")
        .gap(6.0)
        .child(
            text(tr("OpenCode sessions"))
                .font(DESCRIPTION)
                .color(p.text()),
        )
        .child(button(
            p,
            "agent-find-opencode-sessions",
            tr("Find sessions"),
            ButtonKind::Secondary,
            |ui: &mut Ui, _| {
                if ui.agent.is_busy() {
                    return;
                }
                match agent_host::handle(AgentAction::Sessions { resume: None }) {
                    crate::control::ControlReply::AgentSessions { sessions, .. } => {
                        let total = sessions.len();
                        ui.agent.session_choices = sessions
                            .into_iter()
                            .take(6)
                            .map(|session| {
                                let label = session
                                    .title
                                    .chars()
                                    .filter(|ch| !ch.is_control())
                                    .take(80)
                                    .collect();
                                (session.id.as_str().to_owned(), label)
                            })
                            .collect();
                        ui.agent.session_message = if total > 6 {
                            Some(format!(
                                "Showing 6 of {total}; use the CLI for the full list"
                            ))
                        } else if total == 0 {
                            Some("No OpenCode sessions in the selected project".into())
                        } else {
                            None
                        };
                    }
                    crate::control::ControlReply::Error { message, .. } => {
                        ui.agent.session_choices.clear();
                        ui.agent.session_message = Some(message);
                    }
                    _ => {
                        ui.agent.session_message =
                            Some("The OpenCode session list was unavailable".into());
                    }
                }
            },
        ));
    if let Some(message) = &pane.session_message {
        list = list.child(
            text(message.clone())
                .key("agent-session-message")
                .font(DESCRIPTION)
                .color(p.muted()),
        );
    }
    for (id, title) in &pane.session_choices {
        let session_id = id.clone();
        list = list.child(button(
            p,
            &format!("agent-session-{id}"),
            &format!("Select {title}"),
            ButtonKind::Secondary,
            move |ui: &mut Ui, _| {
                if ui.agent.is_busy() {
                    return;
                }
                match agent_host::handle(AgentAction::Sessions {
                    resume: Some(session_id.clone()),
                }) {
                    crate::control::ControlReply::AgentSessions { selected: true, .. } => {
                        ui.agent.session_message =
                            Some(format!("Verified and selected {session_id}"));
                    }
                    crate::control::ControlReply::Error { message, .. } => {
                        ui.agent.session_message = Some(message);
                    }
                    _ => {
                        ui.agent.session_message =
                            Some("This session could not be verified".into());
                    }
                }
            },
        ));
    }
    if busy {
        list = list.child(
            text(tr("Finish the current task before selecting a session."))
                .font(DESCRIPTION)
                .color(p.muted()),
        );
    }
    list
}

const PROJECT_CHOICES_PER_PAGE: usize = 5;

fn registered_projects_row(p: Paint, pane: &AgentPane, busy: bool) -> El {
    let mut list = col().key("agent-registered-projects").gap(6.0).child(
        row()
            .gap(8.0)
            .center_y()
            .child(
                text(tr("Registered projects"))
                    .font(DESCRIPTION)
                    .color(p.text()),
            )
            .child(button(
                p,
                "agent-list-projects",
                tr("Browse projects"),
                ButtonKind::Secondary,
                |ui: &mut Ui, _| match agent_host::service().list_registered_projects() {
                    Ok(projects) => {
                        ui.agent.project_choices = projects
                            .into_iter()
                            .map(|project| (project.id.to_string(), project.name))
                            .collect();
                        ui.agent.project_page = 0;
                        ui.agent.project_message = if ui.agent.project_choices.is_empty() {
                            Some(
                                "No registered projects. Use x agent projects register to add one."
                                    .into(),
                            )
                        } else {
                            None
                        };
                    }
                    Err(error) => {
                        ui.agent.project_choices.clear();
                        ui.agent.project_page = 0;
                        ui.agent.project_message = Some(error);
                    }
                },
            )),
    );
    if let Some(message) = &pane.project_message {
        list = list.child(
            text(message.clone())
                .key("agent-project-message")
                .font(DESCRIPTION)
                .color(p.muted()),
        );
    }
    let start = pane
        .project_page
        .saturating_mul(PROJECT_CHOICES_PER_PAGE)
        .min(pane.project_choices.len());
    let end = (start + PROJECT_CHOICES_PER_PAGE).min(pane.project_choices.len());
    for (id, name) in &pane.project_choices[start..end] {
        let project_id = id.clone();
        let shown = name.clone();
        list = list.child(button(
            p,
            &format!("agent-choose-project-{id}"),
            &format!("Use {name}"),
            ButtonKind::Secondary,
            move |ui: &mut Ui, _| {
                if ui.agent.is_busy() {
                    return;
                }
                match agent_host::service().select_registered_project(&project_id) {
                    Ok((project, _)) => {
                        ui.agent.composer.select_project(project.clone());
                        ui.agent.workspace.text = project.root.display().to_string();
                        ui.agent.session_choices.clear();
                        ui.agent.session_message = None;
                        ui.agent.project_message = Some(format!("Selected {}", project.name));
                    }
                    Err(error) => {
                        ui.agent.project_message = Some(format!("Cannot select {shown}: {error}"));
                    }
                }
            },
        ));
    }
    if pane.project_choices.len() > PROJECT_CHOICES_PER_PAGE {
        let page = pane.project_page;
        let pages = pane
            .project_choices
            .len()
            .div_ceil(PROJECT_CHOICES_PER_PAGE);
        list = list.child(
            row()
                .gap(8.0)
                .center_y()
                .child(button(
                    p,
                    "agent-project-previous",
                    tr("Previous"),
                    ButtonKind::Secondary,
                    |ui: &mut Ui, _| {
                        ui.agent.project_page = ui.agent.project_page.saturating_sub(1);
                    },
                ))
                .child(
                    text(format!("{} / {pages}", page + 1))
                        .font(DESCRIPTION)
                        .color(p.muted()),
                )
                .child(button(
                    p,
                    "agent-project-next",
                    tr("Next"),
                    ButtonKind::Secondary,
                    move |ui: &mut Ui, _| {
                        ui.agent.project_page = (ui.agent.project_page + 1).min(pages - 1);
                    },
                )),
        );
    }
    if busy {
        list = list.child(
            text(tr("Finish the current task before switching projects."))
                .font(DESCRIPTION)
                .color(p.muted()),
        );
    }
    list
}

const TEST_SUITES: [(&str, ::x_agent::verification::test_runner::Suite); 6] = [
    (
        "Cargo workspace",
        ::x_agent::verification::test_runner::Suite::CargoWorkspace,
    ),
    (
        "Cargo x-agent",
        ::x_agent::verification::test_runner::Suite::CargoAgent,
    ),
    ("npm test", ::x_agent::verification::test_runner::Suite::Npm),
    (
        "pnpm test",
        ::x_agent::verification::test_runner::Suite::Pnpm,
    ),
    (
        "pytest",
        ::x_agent::verification::test_runner::Suite::Pytest,
    ),
    ("go test", ::x_agent::verification::test_runner::Suite::Go),
];

fn project_tests_row(p: Paint, pane: &AgentPane, busy: bool) -> El {
    let field = TextInput::new(&pane.test_project)
        .placeholder(tr("Registered project name or ID"))
        .width(None)
        .on_edit(|ui: &mut Ui| &mut ui.agent.test_project)
        .build(p.k)
        .key("agent-test-project")
        .w(0.0)
        .grow(1.0)
        .shrink();
    let selected_suite = pane.test_suite_index % TEST_SUITES.len();
    col()
        .key("agent-project-tests")
        .gap(6.0)
        .child(text(tr("Isolated project tests")).font(DESCRIPTION).color(p.text()))
        .child(
            row()
                .gap(8.0)
                .center_y()
                .child(field)
                .child(button(
                    p,
                    "agent-test-suite",
                    TEST_SUITES[selected_suite].0,
                    ButtonKind::Secondary,
                    move |ui: &mut Ui, _| {
                        if !ui.agent.is_busy() {
                            ui.agent.test_suite_index =
                                (ui.agent.test_suite_index + 1) % TEST_SUITES.len();
                        }
                    },
                ))
                .child(button(
                    p,
                    "agent-test-review",
                    tr("Review test run"),
                    if busy {
                        ButtonKind::Secondary
                    } else {
                        ButtonKind::Primary
                    },
                    |ui: &mut Ui, _| {
                        let project_id = ui.agent.test_project.text.trim().to_string();
                        let suite = TEST_SUITES[ui.agent.test_suite_index % TEST_SUITES.len()].1;
                        if let Err(error) =
                            agent_host::service().plan_worktree_tests(&project_id, suite)
                        {
                            agent_host::service().emit("error", &error);
                        }
                    },
                )),
        )
        .child(
            text(tr("Test scripts run with your user permissions, not in a security sandbox. X requires a separate approval and retains the isolated checkout."))
                .font(DESCRIPTION)
                .color(p.warning())
                .wrap(),
        )
}

fn recovery_review_row(p: Paint, pane: &AgentPane) -> El {
    let mut item = col()
        .key("agent-recovery-review")
        .gap(6.0)
        .child(text(tr("Coding recovery")).font(DESCRIPTION).color(p.text()))
        .child(button(
            p,
            "agent-inspect-recovery",
            tr("Inspect interrupted jobs"),
            ButtonKind::Secondary,
            |ui: &mut Ui, _| {
                match agent_host::service().coding_claims_summary() {
                    Ok(report) => {
                        let jobs = report["jobs"].as_array();
                        let count = jobs.map_or(0, Vec::len);
                        let need_check = jobs.map_or(0, |list| {
                            list.iter()
                                .filter(|job| {
                                    job["requires_provider_check_before_retry"]
                                        .as_bool() == Some(true)
                                })
                                .count()
                        });
                        let mut lines = vec![format!(
                            "{count} recorded coding jobs; {need_check} require an independent provider check before any retry."
                        )];
                        if let Some(jobs) = jobs {
                            for job in jobs.iter().take(5) {
                                let node = job["node_id"].as_str().unwrap_or("unknown");
                                let status = job["status"].as_str().unwrap_or("unknown");
                                let attempt = job["attempt"].as_u64().unwrap_or(0);
                                lines.push(format!("{node}: {status} (attempt {attempt})"));
                            }
                            if jobs.len() > 5 {
                                lines.push(format!(
                                    "{} more jobs: see x agent workflow claims",
                                    jobs.len() - 5
                                ));
                            }
                        }
                        ui.agent.recovery_summary = Some(lines.join("\n"));
                    }
                    Err(error) => {
                        ui.agent.recovery_summary = Some(format!("Recovery unavailable: {error}"));
                    }
                }
            },
        ))
        .child(
            text(tr("Read-only inspection; no coding job is retried or resumed."))
                .font(DESCRIPTION)
                .color(p.muted()),
        );
    if let Some(summary) = &pane.recovery_summary {
        item = item.child(
            text(summary.clone())
                .key("agent-recovery-summary")
                .font(DESCRIPTION)
                .color(p.muted()),
        );
    }
    item
}

fn task_card(
    p: Paint,
    pane: &AgentPane,
    snapshot: &Snapshot,
    task: &::x_agent::service::Task,
) -> El {
    let mut card = col()
        .key("agent-task")
        .gap(8.0)
        .px(16.0)
        .py(16.0)
        .radius(p.k.radius.md)
        .fill(p.k.c.layer_01)
        .child(
            text(task.text.clone())
                .key("task-text")
                .font(p.k.ty.small_medium)
                .color(p.text())
                .wrap(),
        );
    if snapshot.phase == Phase::AwaitingApproval
        && let Some(preview) = &task.edit
    {
        return card.child(edit::card(p, task, preview));
    }
    for (index, step) in task.steps.iter().enumerate() {
        let color = match step.status.as_str() {
            "failed" => p.danger(),
            "pending" => p.muted(),
            _ => p.text(),
        };
        let mut line = format!("{}  {}", step_glyph(&step.status), step.title);
        if let Some(execution) = &step.execution {
            use ::x_agent::tools::EffectState;
            line.push_str(match execution.state {
                EffectState::Confirmed => " · Confirmed",
                EffectState::Launched => " · Launched; effect unverified",
                EffectState::Observed => " · Observed; goal unverified",
                EffectState::Unverified => " · Effect unverified",
                EffectState::Failed => " · Failed",
            });
        } else if step.status == "delivered" {
            line.push_str(" · Delivery confirmed; code not verified");
        } else if step.status == "stopping" {
            line.push_str(" · Stop requested");
        }
        if !step.risk.is_empty()
            && matches!(step.status.as_str(), "pending" | "waiting_for_permission")
        {
            line.push_str(&format!("  ·  {}", step.risk));
        }
        if !step.detail.is_empty()
            && (step.execution.is_some() || matches!(step.status.as_str(), "failed" | "launched"))
        {
            line.push_str(&format!("  ·  {}", step.detail));
        }
        card = card.child(
            text(line)
                .key(format!("step{index}"))
                .font(DESCRIPTION)
                .color(color)
                .wrap(),
        );
        if step.status == "waiting_for_permission" {
            card = card.child(runs::permission_review(
                p,
                task.id,
                index,
                &snapshot.permission_waits,
            ));
        }
        if snapshot.cancellable_coding_jobs.contains(&index) {
            let task_id = task.id;
            card = card.child(button(
                p,
                &format!("stop-coding-job-{index}"),
                &format!("Stop job {}", index + 1),
                ButtonKind::Secondary,
                move |_: &mut Ui, _| {
                    if let Err(error) = agent_host::service().cancel_coding_job(task_id, index) {
                        agent_host::service().emit("error", &error);
                    }
                },
            ));
        }
    }
    if snapshot.computer_question.is_some() || snapshot.window_question.is_some() {
        card = card.child(dialogue::panel(p, pane, snapshot));
    }
    if snapshot.phase == Phase::AwaitingApproval {
        let id = task.id;
        let question = snapshot
            .computer_question
            .as_ref()
            .map(|question| question.question_id.clone());
        let window_question = snapshot
            .window_question
            .as_ref()
            .map(|question| question.question_id.clone());
        let discard_window = window_question.clone();
        card = card.child(
            row()
                .key("agent-approval")
                .gap(8.0)
                .child(button(
                    p,
                    "agent-approve",
                    if window_question.is_some() {
                        "Approve window action"
                    } else if question.is_some() {
                        "Approve and resume"
                    } else {
                        tr("Approve and run")
                    },
                    ButtonKind::Primary,
                    move |_: &mut Ui, _| {
                        let service = agent_host::service();
                        let result = match (&window_question, &question) {
                            (Some(token), _) => service.approve_window_answer(id, token),
                            (_, Some(token)) => service.approve_computer_answer(id, token),
                            (None, None) => service.approve(id),
                        };
                        if let Err(error) = result {
                            agent_host::service().emit("error", &error);
                        }
                    },
                ))
                .child(button(
                    p,
                    "agent-discard",
                    tr("Discard"),
                    ButtonKind::Secondary,
                    move |_: &mut Ui, _| {
                        let service = agent_host::service();
                        if let Some(token) = &discard_window {
                            if let Err(error) = service.cancel_window_question(id, token) {
                                service.emit("error", &error);
                            }
                        } else if let Err(error) = service.cancel_foreground_task(id) {
                            service.emit("error", &error);
                        }
                    },
                )),
        );
    }
    if snapshot.can_run_in_background {
        let id = task.id;
        card = card.child(button(
            p,
            "agent-approve-background",
            "Approve in background",
            ButtonKind::Secondary,
            move |_: &mut Ui, _| {
                if let Err(error) = agent_host::service().approve_background(id) {
                    agent_host::service().emit("error", &error);
                }
            },
        ));
    }
    if snapshot.phase == Phase::Completed
        && (::x_agent::wm::recent_undo_available() || ::x_agent::sys::recent_undo_available())
    {
        card = card.child(button(
            p,
            "agent-undo-recent",
            tr("Undo last change"),
            ButtonKind::Secondary,
            |_: &mut Ui, _| {
                let _ = agent_host::handle(AgentAction::Undo);
            },
        ));
    }
    if matches!(snapshot.phase, Phase::Running | Phase::WaitingForInput) {
        let id = task.id;
        let window_question = snapshot
            .window_question
            .as_ref()
            .map(|question| (question.task_id, question.question_id.clone()));
        card = card.child(button(
            p,
            "agent-stop",
            tr("Stop"),
            ButtonKind::Danger,
            move |_: &mut Ui, _| {
                if let Some((id, token)) = &window_question {
                    if let Err(error) = agent_host::service().cancel_window_question(*id, token) {
                        agent_host::service().emit("error", &error);
                    }
                } else {
                    agent_host::handle(AgentAction::StopTask { id });
                }
            },
        ));
    }
    if !snapshot.phase.is_busy() {
        for (index, command) in task.blocked.iter().enumerate() {
            let command = command.clone();
            card = card.child(button(
                p,
                &format!("agent-allow{index}"),
                &format!("{} `{command}`", tr("Allow once and rerun")),
                ButtonKind::Secondary,
                move |_: &mut Ui, _| {
                    agent_host::handle(AgentAction::Allow { index });
                },
            ));
        }
    }
    if let Some(error) = &task.error {
        card = card.child(
            text(error.clone())
                .key("task-error")
                .font(DESCRIPTION)
                .color(p.danger())
                .wrap(),
        );
    }
    if let Some(summary) = &task.summary {
        card = card.child(
            text(summary.clone())
                .key("task-summary")
                .font(DESCRIPTION)
                .color(p.muted())
                .wrap(),
        );
    }
    if let Some(files) = &task.changed_files {
        let shown = if files.is_empty() {
            tr("No files changed").to_string()
        } else {
            files.join(", ")
        };
        card = card.child(
            text(format!("{}: {shown}", tr("Changed files")))
                .key("task-files")
                .font(DESCRIPTION)
                .color(p.text())
                .wrap(),
        );
    }
    card
}

fn event_stream(p: Paint, snapshot: &Snapshot) -> El {
    let mut list = col().key("agent-events").gap(2.0).child(section_label(
        p,
        "agent-activity",
        tr("Activity"),
    ));
    let skip = snapshot.events.len().saturating_sub(SHOWN_EVENTS);
    for event in snapshot.events.iter().skip(skip) {
        let color = match event.kind.as_str() {
            "error" => p.danger(),
            "stall" | "permission" => p.warning(),
            "verify" => p.success(),
            _ => p.muted(),
        };
        list = list.child(
            text(format!("{:<8} {}", event.kind, event.text))
                .key(format!("event{}", event.n))
                .font(DESCRIPTION)
                .color(color),
        );
    }
    list
}

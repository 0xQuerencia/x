//! The Agent pane (Labs › X agent): speak or type a goal, read the plan, approve it, watch
//! it run, and Stop it. Everything real lives in `crates/x-agent`; this file draws its
//! snapshot and forwards presses to the same `hex agent` handler the CLI uses.

mod edit;
mod quick;
mod workflows;

use ::x_agent::phase::Phase;
use ::x_agent::service::{Mode, Snapshot};
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
    input: TextEdit,
    quick_query: TextEdit,
    snippet_trigger: TextEdit,
    snippet_value: TextEdit,
    snippet_note: Option<String>,
    quick_notice: Option<String>,
    quick_file_preview: Option<String>,
    pending_process: Option<::x_agent::quick::processes::ProcessInfo>,
    workspace: TextEdit,
    mode: Mode,
    snapshot: Option<Snapshot>,
    /// Serialized form of the last snapshot, so polling redraws only on change.
    shown: String,
    /// Whether the workflow manager is showing instead of the command view.
    pub(super) show_workflows: bool,
    pub(super) show_quick: bool,
    pub(super) workflows: workflows::Manager,
}

impl Default for AgentPane {
    fn default() -> Self {
        Self {
            input: TextEdit::default(),
            quick_query: TextEdit::default(),
            snippet_trigger: TextEdit::default(),
            snippet_value: TextEdit::default(),
            snippet_note: None,
            quick_notice: None,
            quick_file_preview: None,
            pending_process: None,
            workspace: TextEdit::default(),
            mode: Mode::Actions,
            snapshot: None,
            shown: String::new(),
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
        let serialized = serde_json::to_string(&snapshot).unwrap_or_default();
        let listed = (self.show_workflows || self.show_quick) && self.workflows.refresh();
        if serialized == self.shown {
            return listed;
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
        Phase::Running => tr("Running"),
        Phase::Completed => tr("Completed"),
        Phase::Failed => tr("Failed"),
        Phase::Cancelled => tr("Cancelled"),
    }
}

fn step_glyph(status: &str) -> &'static str {
    match status {
        "running" => "▸",
        "launched" | "done" => "✓",
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
    let tone = match snapshot.phase {
        Phase::Failed => p.danger(),
        Phase::AwaitingApproval => p.warning(),
        Phase::Running | Phase::Listening | Phase::Transcribing | Phase::Planning => p.accent(),
        Phase::Completed => p.success(),
        Phase::Idle | Phase::Cancelled => p.muted(),
    };
    let status = row()
        .key("agent-status")
        .gap(8.0)
        .center_y()
        .child(status_dot(tone))
        .child(
            text(phase_label(snapshot.phase))
                .key("phase")
                .font(DESCRIPTION)
                .color(p.text()),
        );

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
        status,
        text(tr("Dictate keeps its own shortcut"))
            .key("dictate-note")
            .font(DESCRIPTION)
            .color(p.muted()),
        mode_button("mode-actions", tr("Actions"), Mode::Actions),
        mode_button("mode-work", tr("Work"), Mode::Work),
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
        let mut body = col().key("agent-body").gap(16.0);
        if let Some(task) = &snapshot.task {
            body = body.child(task_card(p, snapshot, task));
        }
        body = body.child(quick::view(p, ui));
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
        let mut body = col().key("agent-body").gap(16.0);
        if let Some(task) = &snapshot.task {
            body = body.child(task_card(p, snapshot, task));
        }
        body = body.child(workflows::view(p, ui));
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
        .key("agent-input");
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
                agent_host::handle(AgentAction::Stop);
            },
        ))
    } else {
        capture
    };
    let mut body = col()
        .key("agent-body")
        .gap(16.0)
        .child(capture)
        .child(voice_row)
        .child(autonomy_control)
        .child(text(tr("Autonomous: safe Actions and granted apps run without repeated approval. Work and protected actions still ask."))
            .font(DESCRIPTION)
            .color(p.muted()));
    body = body.child(access_row(p, snapshot));
    if pane.mode == Mode::Work {
        body = body.child(workspace_row(p, pane, busy));
    }
    if let Some(task) = &snapshot.task {
        body = body.child(task_card(p, snapshot, task));
    } else {
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
            .child(body),
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
        .key("agent-workspace");
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

fn task_card(p: Paint, snapshot: &Snapshot, task: &::x_agent::service::Task) -> El {
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
                .color(p.text()),
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
        if !step.risk.is_empty() && step.status == "pending" {
            line.push_str(&format!("  ·  {}", step.risk));
        }
        if !step.detail.is_empty() && matches!(step.status.as_str(), "failed" | "launched") {
            line.push_str(&format!("  ·  {}", step.detail));
        }
        card = card.child(
            text(line)
                .key(format!("step{index}"))
                .font(DESCRIPTION)
                .color(color),
        );
    }
    if snapshot.phase == Phase::AwaitingApproval {
        let id = task.id;
        card = card.child(
            row()
                .key("agent-approval")
                .gap(8.0)
                .child(button(
                    p,
                    "agent-approve",
                    tr("Approve and run"),
                    ButtonKind::Primary,
                    move |_: &mut Ui, _| {
                        if let Err(error) = agent_host::service().approve(id) {
                            agent_host::service().emit("error", &error);
                        }
                    },
                ))
                .child(button(
                    p,
                    "agent-discard",
                    tr("Discard"),
                    ButtonKind::Secondary,
                    |_: &mut Ui, _| {
                        agent_host::service().cancel();
                    },
                )),
        );
    }
    if snapshot.phase == Phase::Running {
        card = card.child(button(
            p,
            "agent-stop",
            tr("Stop"),
            ButtonKind::Danger,
            |_: &mut Ui, _| {
                agent_host::handle(AgentAction::Stop);
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
                .color(p.danger()),
        );
    }
    if let Some(summary) = &task.summary {
        card = card.child(
            text(summary.clone())
                .key("task-summary")
                .font(DESCRIPTION)
                .color(p.muted()),
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
                .color(p.text()),
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

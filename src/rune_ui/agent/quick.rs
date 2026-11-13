//! X Quick: a compact keyboard-first entry point for the existing typed actions.
//! This is intentionally part of the native Agent UI, not a second action engine.

use ::x_agent::quick::{self, Target};
use ::x_agent::service::Mode;
use rune::{El, col, row, text};
use rune_kit::TextInput;

use crate::agent_host;
use crate::desktop::i18n::tr;
use crate::rune_ui::Ui;
use crate::rune_ui::controls::{ButtonKind, DESCRIPTION, Paint, button};
use crate::rune_ui::lists::section_label;

pub(super) fn view(p: Paint, ui: &Ui) -> El {
    let input = TextInput::new(&ui.agent.quick_query)
        .placeholder(tr("Apps, file report, =2*(3+4), color #abc, or web search"))
        .width(None)
        .on_edit(|ui: &mut Ui| &mut ui.agent.quick_query)
        .build(p.k)
        .key("quick-search");

    let workflows = ui.agent.workflows.quick_entries();
    let mut matches = quick::search(&ui.agent.quick_query.text, workflows);
    // History search is deliberate and local; it reuses Hex's own retention,
    // ranking and vocabulary logic rather than creating another index.
    let history_query = ui
        .agent
        .quick_query
        .text
        .strip_prefix("history ")
        .or_else(|| ui.agent.quick_query.text.strip_prefix("hist "));
    if let Some(query) = history_query
        && (3..=120).contains(&query.trim().chars().count())
        && let Ok(entries) = crate::control::transcript_cli::quick_history(query)
    {
        let history_hits = entries.into_iter().map(|(final_text, app)| {
            let preview = final_text.chars().take(80).collect::<String>();
            quick::Hit {
                title: format!("History: {preview}"),
                detail: format!("Retained dictation · {app} · copy on request"),
                target: Target::CopyResult(final_text),
            }
        });
        matches.splice(0..0, history_hits);
        matches.truncate(8);
    }
    let mut content = col()
        .key("x-quick")
        .gap(10.0)
        .child(section_label(p, "quick-title", tr("Quick actions")))
        .child(input)
        .child(
            text(tr(
                "Local search only. Actions use X's normal plan and approval checks.",
            ))
            .key("quick-description")
            .font(DESCRIPTION)
            .color(p.muted()),
        );

    if matches.is_empty() {
        content = content.child(
            text(tr("No matching app, workspace, or runnable workflow"))
                .key("quick-empty")
                .font(DESCRIPTION)
                .color(p.muted()),
        );
    }

    for (i, hit) in matches.into_iter().enumerate() {
        let title = hit.title;
        let detail = hit.detail;
        let target = hit.target;
        let is_copy = matches!(&target, Target::CopyResult(_) | Target::CopySnippet(_));
        let is_process = matches!(&target, Target::EndProcess(_));
        let confirming = matches!(&target, Target::EndProcess(info)
            if ui.agent.pending_process.as_ref().is_some_and(|previous|
                previous.pid == info.pid
                    && previous.start_ticks == info.start_ticks
                    && previous.name == info.name));
        content = content.child(
            row()
                .key(format!("quick-row-{i}"))
                .gap(12.0)
                .center_y()
                .child(
                    col()
                        .key(format!("quick-info-{i}"))
                        .gap(2.0)
                        .child(
                            text(title.clone())
                                .key(format!("quick-label-{i}"))
                                .font(p.k.ty.small_medium)
                                .color(p.text()),
                        )
                        .child(
                            text(detail)
                                .key(format!("quick-detail-{i}"))
                                .font(DESCRIPTION)
                                .color(p.muted()),
                        ),
                )
                .child(button(
                    p,
                    &format!("quick-use-{i}"),
                    if is_copy {
                        tr("Copy")
                    } else if matches!(&target, Target::PickColor) {
                        tr("Pick")
                    } else if matches!(&target, Target::OpenFile(_)) {
                        tr("Open")
                    } else if matches!(&target, Target::PreviewFile(_)) {
                        tr("Preview")
                    } else if is_process {
                        if confirming { tr("Confirm SIGTERM") } else { tr("End…") }
                    } else {
                        tr("Plan")
                    },
                    ButtonKind::Secondary,
                    move |ui: &mut Ui, _| {
                        if !is_copy && !is_process && !matches!(&target, Target::PreviewFile(_)) {
                            ui.agent.show_quick = false;
                        }
                        match &target {
                            Target::EndProcess(info) => {
                                if !confirming {
                                    ui.agent.pending_process = Some(info.clone());
                                    ui.agent.quick_notice = Some(format!(
                                        "Confirm ending {} (PID {}) with a second click. No signal sent yet.",
                                        info.name, info.pid
                                    ));
                                } else {
                                    ui.agent.pending_process = None;
                                    ui.agent.quick_notice = Some(
                                        ::x_agent::quick::processes::terminate(info)
                                            .unwrap_or_else(|error| error),
                                    );
                                }
                            }
                            Target::CopySnippet(trigger) => {
                                let trigger = trigger.clone();
                                std::thread::spawn(move || {
                                    match ::x_agent::quick::snippets::expand(&trigger)
                                        .and_then(|text| ::x_agent::clipboard::copy(&text))
                                    {
                                        Ok(message) => agent_host::service().emit("info", &message),
                                        Err(error) => agent_host::service().emit("error", &error),
                                    }
                                });
                            }
                            Target::CopyResult(value) => {
                                let value = value.clone();
                                std::thread::spawn(move || {
                                    match ::x_agent::clipboard::copy(&value) {
                                        Ok(message) => agent_host::service().emit("info", &message),
                                        Err(error) => agent_host::service().emit("error", &error),
                                    }
                                });
                            }
                            Target::PreviewFile(path) => {
                                ui.agent.quick_file_preview = Some(
                                    ::x_agent::quick::files::preview(path)
                                        .unwrap_or_else(|error| format!("Preview refused: {error}")),
                                );
                            }
                            Target::OpenFile(path) => {
                                let path = path.clone();
                                std::thread::spawn(move || {
                                    match ::x_agent::quick::files::open(&path) {
                                        Ok(message) => agent_host::service().emit("info", &message),
                                        Err(error) => agent_host::service().emit("error", &error),
                                    }
                                });
                            }
                            Target::PickColor => {
                                std::thread::spawn(move || {
                                    match ::x_agent::quick::picker::pick() {
                                        Ok(message) => agent_host::service().emit("info", &message),
                                        Err(error) => agent_host::service().emit("error", &error),
                                    }
                                });
                            }
                            Target::Workflow(id) => {
                                if let Err(error) = agent_host::service().plan_workflow_by_id(id) {
                                    agent_host::service().emit("error", &error);
                                }
                            }
                            _ => {
                                // These phrases come from the typed registry, not
                                // user-provided shell text or an LLM suggestion.
                                if let Some(phrase) = target.phrase() {
                                    ui.agent.input.text = phrase.clone();
                                    std::thread::spawn(move || {
                                        if let Err(error) =
                                            agent_host::service().plan(&phrase, Mode::Actions)
                                        {
                                            agent_host::service().emit("error", &error);
                                        }
                                    });
                                }
                            }
                        }
                    },
                )),
        );
    }
    if let Some(preview) = &ui.agent.quick_file_preview {
        content = content.child(
            col()
                .key("quick-file-preview")
                .gap(6.0)
                .child(section_label(
                    p,
                    "quick-file-preview-title",
                    tr("Local file preview"),
                ))
                .child(
                    text(preview.clone())
                        .key("quick-file-preview-text")
                        .font(DESCRIPTION)
                        .color(p.text()),
                )
                .child(button(
                    p,
                    "quick-file-preview-dismiss",
                    tr("Close preview"),
                    ButtonKind::Secondary,
                    |ui: &mut Ui, _| {
                        ui.agent.quick_file_preview = None;
                    },
                )),
        );
    }
    if let Some(note) = &ui.agent.quick_notice {
        content = content.child(
            text(note.clone())
                .key("quick-process-confirmation")
                .font(DESCRIPTION)
                .color(p.warning()),
        );
    }
    // Background capture is disabled by default, and only runs during a user-started
    // bounded session. Regular snapshots still require an explicit click.
    content = content
        .child(section_label(p, "quick-clipboard-title", tr("Clipboard snapshots")))
        .child(
            text(tr("Manual text snapshots, or opt in to a 10-minute local text-change session. It may capture unmarked secrets: avoid copying passwords while active."))
                .key("quick-clipboard-note")
                .font(DESCRIPTION)
                .color(p.muted()),
        )
        .child(
            row()
                .gap(8.0)
                .child(button(
                    p,
                    "quick-clipboard-capture",
                    tr("Save current text"),
                    ButtonKind::Secondary,
                    |ui: &mut Ui, _| {
                        ui.agent.quick_notice = Some("Capturing clipboard text on request…".into());
                        std::thread::spawn(|| {
                            match ::x_agent::quick::clipboard_history::capture_now() {
                                Ok(message) => agent_host::service().emit("info", &message),
                                Err(error) => agent_host::service().emit("error", &error),
                            }
                        });
                    },
                ))
                .child(button(
                    p,
                    "quick-clipboard-watch-start",
                    tr("Capture changes · 10 min"),
                    ButtonKind::Secondary,
                    |ui: &mut Ui, _| {
                        ui.agent.quick_notice = Some(
                            ::x_agent::quick::clipboard_watch::start(10)
                                .unwrap_or_else(|error| error),
                        );
                    },
                ))
                .child(button(
                    p,
                    "quick-clipboard-watch-stop",
                    tr("Stop capture"),
                    ButtonKind::Secondary,
                    |ui: &mut Ui, _| {
                        ui.agent.quick_notice = Some(if ::x_agent::quick::clipboard_watch::stop() {
                            "Clipboard-change capture stopped".into()
                        } else {
                            "Clipboard-change capture was already off".into()
                        });
                    },
                ))
                .child(button(
                    p,
                    "quick-clipboard-clear",
                    tr("Erase saved snapshots"),
                    ButtonKind::Secondary,
                    |ui: &mut Ui, _| {
                        ui.agent.quick_notice = Some(
                            match ::x_agent::quick::clipboard_history::Store::user()
                                .and_then(|store| store.clear())
                            {
                                Ok(()) => "Saved clipboard snapshots erased".into(),
                                Err(error) => error,
                            },
                        );
                    },
                )),
        );
    // Snippets are explicit, local-only copy actions. This editor intentionally
    // does not install global keyboard hooks or intercept what other apps type.
    let trigger = TextInput::new(&ui.agent.snippet_trigger)
        .placeholder(tr("Snippet trigger, e.g. signature"))
        .width(None)
        .on_edit(|ui: &mut Ui| &mut ui.agent.snippet_trigger)
        .build(p.k)
        .key("quick-snippet-trigger");
    let value = TextInput::new(&ui.agent.snippet_value)
        .placeholder(tr("Text to copy"))
        .width(None)
        .on_edit(|ui: &mut Ui| &mut ui.agent.snippet_value)
        .build(p.k)
        .key("quick-snippet-value");
    let mut editor = col()
        .key("quick-snippet-editor")
        .gap(8.0)
        .child(section_label(
            p,
            "quick-snippet-title",
            tr("Saved snippets"),
        ))
        .child(
            text(tr(
                "Save or remove snippets here. Search 'snip name' to copy one.",
            ))
            .key("quick-snippet-help")
            .font(DESCRIPTION)
            .color(p.muted()),
        )
        .child(trigger)
        .child(value)
        .child(
            row()
                .gap(8.0)
                .child(button(
                    p,
                    "quick-snippet-save",
                    tr("Save snippet"),
                    ButtonKind::Secondary,
                    |ui: &mut Ui, _| {
                        let trigger = ui.agent.snippet_trigger.text.clone();
                        let value = ui.agent.snippet_value.text.clone();
                        ui.agent.snippet_note = Some(
                            match ::x_agent::quick::snippets::Store::user()
                                .and_then(|store| store.upsert(&trigger, &value))
                            {
                                Ok(()) => "Snippet saved locally".into(),
                                Err(error) => error,
                            },
                        );
                    },
                ))
                .child(button(
                    p,
                    "quick-snippet-delete",
                    tr("Delete snippet"),
                    ButtonKind::Secondary,
                    |ui: &mut Ui, _| {
                        let trigger = ui.agent.snippet_trigger.text.clone();
                        ui.agent.snippet_note = Some(
                            match ::x_agent::quick::snippets::Store::user()
                                .and_then(|store| store.delete(&trigger))
                            {
                                Ok(true) => "Snippet removed".into(),
                                Ok(false) => "No snippet has that trigger".into(),
                                Err(error) => error,
                            },
                        );
                    },
                )),
        );
    if let Some(note) = &ui.agent.snippet_note {
        editor = editor.child(
            text(note.clone())
                .key("quick-snippet-note")
                .font(DESCRIPTION)
                .color(p.muted()),
        );
    }
    content.child(editor)
}

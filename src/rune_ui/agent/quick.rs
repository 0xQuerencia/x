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
        .placeholder(tr("Apps, workflows, =2*(3+4), color #abc, or web search"))
        .width(None)
        .on_edit(|ui: &mut Ui| &mut ui.agent.quick_query)
        .build(p.k)
        .key("quick-search");

    let workflows = ui.agent.workflows.quick_entries();
    let matches = quick::search(&ui.agent.quick_query.text, workflows);
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
        let is_copy = matches!(&target, Target::CopyResult(_));
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
                    } else {
                        tr("Plan")
                    },
                    ButtonKind::Secondary,
                    move |ui: &mut Ui, _| {
                        if !is_copy {
                            ui.agent.show_quick = false;
                        }
                        match &target {
                            Target::CopyResult(value) => {
                                let value = value.clone();
                                std::thread::spawn(move || {
                                    match ::x_agent::clipboard::copy(&value) {
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
    content
}

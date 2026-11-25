//! Live background runs share the normal service; these controls name exact IDs.

use rune::{El, col, row, text};

use super::{step_glyph, task_label};
use crate::agent_host;
use crate::rune_ui::Ui;
use crate::rune_ui::controls::{ButtonKind, DESCRIPTION, Paint, button};
use x_agent::service::Snapshot;

pub(super) fn panel(p: Paint, snapshot: &Snapshot) -> El {
    let mut panel = col().key("agent-background-runs").gap(10.0);
    if snapshot.background_runs.is_empty() {
        return panel;
    }
    let mut heading = row().gap(8.0).center_y().child(
        text("Background work")
            .font(p.k.ty.small_medium)
            .color(p.text()),
    );
    if snapshot
        .background_runs
        .iter()
        .any(|run| run.task.phase.is_busy())
    {
        heading = heading.child(button(
            p,
            "agent-stop-all",
            "Stop all tasks",
            ButtonKind::Danger,
            |_: &mut Ui, _| {
                agent_host::service().cancel();
            },
        ));
    }
    panel = panel.child(heading);
    for run in snapshot.background_runs.iter().rev() {
        let id = run.task.id;
        let mut header = row().gap(8.0).center_y().child(
            text(format!("Task {id} · {}", task_label(&run.task)))
                .font(p.k.ty.small_medium)
                .color(p.text())
                .wrap(),
        );
        if run.task.phase.is_busy() {
            header = header.child(button(
                p,
                &format!("stop-run-{id}"),
                "Stop run",
                ButtonKind::Secondary,
                move |_: &mut Ui, _| {
                    if let Err(error) = agent_host::service().cancel_run(id) {
                        agent_host::service().emit("error", &error);
                    }
                },
            ));
        }
        let mut card = col()
            .key(format!("background-run-{id}"))
            .gap(6.0)
            .px(12.0)
            .py(12.0)
            .radius(p.k.radius.md)
            .fill(p.k.c.layer_01)
            .child(header)
            .child(
                text(run.task.text.clone())
                    .font(DESCRIPTION)
                    .color(p.muted())
                    .wrap(),
            );
        for (index, step) in run.task.steps.iter().enumerate() {
            let mut line = col()
                .key(format!("background-step-{id}-{index}"))
                .gap(4.0)
                .child(
                    text(format!(
                        "{} {} · {}",
                        step_glyph(&step.status),
                        step.title,
                        step.status
                    ))
                    .font(DESCRIPTION)
                    .color(if step.status == "failed" {
                        p.danger()
                    } else {
                        p.text()
                    })
                    .wrap(),
                );
            if !step.detail.is_empty() {
                line = line.child(
                    text(step.detail.clone())
                        .font(DESCRIPTION)
                        .color(p.muted())
                        .wrap(),
                );
            }
            if run.cancellable_coding_jobs.contains(&index) {
                line = line.child(button(
                    p,
                    &format!("stop-run-{id}-job-{index}"),
                    &format!("Stop job {}", index + 1),
                    ButtonKind::Secondary,
                    move |_: &mut Ui, _| {
                        if let Err(error) = agent_host::service().cancel_background_job(id, index) {
                            agent_host::service().emit("error", &error);
                        }
                    },
                ));
            }
            card = card.child(line);
        }
        if let Some(summary) = &run.task.summary {
            card = card.child(
                text(summary.clone())
                    .font(DESCRIPTION)
                    .color(p.text())
                    .wrap(),
            );
        }
        if let Some(error) = &run.task.error {
            card = card.child(
                text(error.clone())
                    .font(DESCRIPTION)
                    .color(p.danger())
                    .wrap(),
            );
        }
        for event in run.events.iter().rev().take(3).rev() {
            card = card.child(
                text(format!("{} · {}", event.kind, event.text))
                    .font(DESCRIPTION)
                    .color(p.muted())
                    .wrap(),
            );
        }
        panel = panel.child(card);
    }
    panel
}

//! Live background runs share the normal service; these controls name exact IDs.

use rune::{El, col, row, text};

use super::{step_glyph, task_label};
use crate::agent_host;
use crate::rune_ui::Ui;
use crate::rune_ui::controls::{ButtonKind, DESCRIPTION, Paint, button};
use x_agent::service::{GraphJobPhase, GraphJobView, Mode, PermissionWait, Snapshot};
use x_agent::workflows::recovery::State;

pub(super) fn permission_review(
    p: Paint,
    task: u64,
    index: usize,
    reviews: &[PermissionWait],
) -> El {
    let mut group = col()
        .key(format!("permission-review-{task}-{index}"))
        .gap(4.0);
    if let Some(review) = reviews.iter().find(|review| review.step_index == index) {
        let token = review.review_token.clone();
        return group
            .child(
                text("This job is waiting for renewed approval.")
                    .font(DESCRIPTION)
                    .color(p.muted())
                    .wrap(),
            )
            .child(button(
                p,
                &format!("renew-job-{task}-{index}"),
                &format!("Renew approval for job {}", index + 1),
                ButtonKind::Secondary,
                move |_: &mut Ui, _| {
                    let service = agent_host::service();
                    if let Err(error) = service.renew_coding_permission(task, &token) {
                        service.emit("error", &error);
                    }
                },
            ));
    }
    group = group.child(
        text("The approval review expired or was revoked.")
            .font(DESCRIPTION)
            .color(p.muted())
            .wrap(),
    );
    group
}

pub(super) fn activity_row(p: Paint, task: u64, job: &GraphJobView) -> El {
    let seconds = job.elapsed_seconds;
    let elapsed = if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    };
    let mut group = col()
        .key(format!("job-activity-{task}-{}", job.index))
        .gap(4.0)
        .child(
            text(format!("{} · {elapsed}", job.phase.label()))
                .font(DESCRIPTION)
                .color(p.muted())
                .wrap(),
        );
    if !job.dependencies.is_empty() {
        group = group.child(
            text(format!("Depends on {}", job.dependencies.join(", ")))
                .font(DESCRIPTION)
                .color(p.muted())
                .wrap(),
        );
    }
    if job.has_checkout {
        let state = match job.phase {
            GraphJobPhase::Verified | GraphJobPhase::Tested => State::Verified,
            GraphJobPhase::Delivered => State::Delivered,
            GraphJobPhase::Failed => State::Failed,
            GraphJobPhase::Stopped => State::Cancelled,
            _ => State::Unverified,
        };
        let run = job.run.to_string();
        let node = job.node.to_string();
        group = group.child(button(
            p,
            &format!("inspect-job-result-{task}-{}", job.index),
            "Review recorded checkout",
            ButtonKind::Secondary,
            move |ui: &mut Ui, _| {
                ui.agent.mode = Mode::Work;
                ui.agent
                    .results
                    .show_graph_result(run.clone(), node.clone(), state);
            },
        ));
    }
    group
}

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
                                if let Some(job) = run.graph_jobs.get(index) {
                        line = line.child(activity_row(p, id, job));
                    }
                    if step.status == "waiting_for_permission" {
                                    "waiting for permission"
                                } else {
                                    &step.status
                                }
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
            if step.status == "waiting_for_permission" {
                if !step.risk.is_empty() {
                    line = line.child(
                        text(step.risk.clone())
                            .font(DESCRIPTION)
                            .color(p.muted())
                            .wrap(),
                    );
                }
                line = line.child(permission_review(p, id, index, &run.permission_waits));
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

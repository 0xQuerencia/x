//! The compact review card for a text edit: what would change, with the changed words
//! marked, and three plain answers. No chat surface. The same answers exist as spoken
//! commands ("replace it", "try again", "keep the original").

use ::x_agent::edit::diff::{Kind, Seg};
use ::x_agent::service::{Mode, Preview, Task};
use rune::{El, Span, col, rich, row, text};

use super::Ui;
use super::controls::{ButtonKind, DESCRIPTION, Paint, button};
use crate::agent_host;
use crate::desktop::i18n::tr;

/// The diff as one paragraph: removed words struck through in the danger colour, added
/// words in the success colour. Ranges are byte offsets into the joined string.
fn diff_paragraph(p: Paint, segs: &[Seg]) -> El {
    let mut joined = String::new();
    let mut spans = Vec::new();
    let mut strikes = Vec::new();
    for seg in segs {
        let start = joined.len();
        joined.push_str(&seg.text);
        let end = joined.len();
        match seg.kind {
            Kind::Same => {}
            Kind::Added => spans.push(Span::new(start, end).color(p.success()).weight(600.0)),
            Kind::Removed => {
                spans.push(Span::new(start, end).color(p.danger()));
                strikes.push((start, end, Some(p.danger())));
            }
        }
    }
    rich(joined, spans)
        .key("edit-diff")
        .font(DESCRIPTION)
        .color(p.text())
        .strikes(strikes)
}

fn block(p: Paint, key: &str, label: &str, body: &str) -> El {
    col()
        .key(key.to_string())
        .gap(2.0)
        .child(text(label.to_string()).key(format!("{key}-label")).font(DESCRIPTION).color(p.muted()))
        .child(text(body.to_string()).key(format!("{key}-text")).font(DESCRIPTION).color(p.text()).wrap())
}

fn run_in_background(said: &'static str) {
    std::thread::spawn(move || {
        if let Err(error) = agent_host::service().plan(said, Mode::Actions) {
            agent_host::service().emit("error", &error);
        }
    });
}

/// The review card for `task`, whose pending edit is `preview`.
pub(super) fn card(p: Paint, task: &Task, preview: &Preview) -> El {
    let id = task.id;
    let model = preview.model.clone().unwrap_or_default();
    let consent = preview.stage == "consent";
    let title = if consent {
        format!("{} {model}?", tr("Send the selected text to"))
    } else {
        format!("{}?", preview.label)
    };
    let mut card = col().key("edit-preview").gap(8.0).child(
        text(title)
            .key("edit-title")
            .font(p.k.ty.small_medium)
            .color(p.text()),
    );
    if preview.remote {
        card = card.child(
            text(format!(
                "{} {model}. {}",
                tr("This text leaves your computer for"),
                tr("Nothing is sent unless you allow it.")
            ))
            .key("edit-remote")
            .font(DESCRIPTION)
            .color(p.warning())
            .wrap(),
        );
    }
    card = card.child(block(p, "edit-original", tr("Original"), &preview.original));
    if let Some(proposed) = &preview.proposed {
        card = match &preview.diff {
            Some(segs) => card.child(
                col()
                    .key("edit-changes")
                    .gap(2.0)
                    .child(text(tr("Suggested").to_string()).key("edit-changes-label").font(DESCRIPTION).color(p.muted()))
                    .child(diff_paragraph(p, segs)),
            ),
            None => card.child(block(p, "edit-suggested", tr("Suggested"), proposed)),
        };
    }
    let kind = if preview.kind == "model" { tr("Written by a model") } else { tr("Rule-based edit, no model") };
    card = card.child(text(kind.to_string()).key("edit-kind").font(DESCRIPTION).color(p.muted()));
    let mut actions = row().key("edit-actions").gap(8.0).child(button(
        p,
        "edit-accept",
        if consent { tr("Send") } else { tr("Replace") },
        ButtonKind::Primary,
        move |_: &mut Ui, _| {
            if let Err(error) = agent_host::service().approve(id) {
                agent_host::service().emit("error", &error);
            }
        },
    ));
    if !consent && preview.kind == "model" {
        actions = actions.child(button(p, "edit-retry", tr("Retry"), ButtonKind::Secondary, |_: &mut Ui, _| {
            run_in_background("try again");
        }));
    }
    actions = actions.child(button(p, "edit-cancel", tr("Cancel"), ButtonKind::Secondary, |_: &mut Ui, _| {
        agent_host::service().cancel();
    }));
    card.child(actions)
}

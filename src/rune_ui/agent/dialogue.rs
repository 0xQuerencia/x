//! A clarification stays attached to its original task and exact question token.

#[cfg(test)]
mod tests;

use rune::{El, TextEdit, col, text};
use rune_kit::TextInput;
use x_agent::service::Snapshot;

use super::{AgentPane, ButtonKind, DESCRIPTION, Paint, Ui, agent_host, button};

#[derive(Default)]
pub(super) struct Dialogue {
    current: Option<(u64, String)>,
    answer: TextEdit,
    error: Option<String>,
}

impl Dialogue {
    pub(super) fn poll(&mut self, snapshot: &Snapshot) -> bool {
        let current = snapshot
            .computer_question
            .as_ref()
            .map(|question| (question.task_id, question.question_id.clone()))
            .or_else(|| {
                snapshot
                    .window_question
                    .as_ref()
                    .map(|question| (question.task_id, question.question_id.clone()))
            });
        if current == self.current {
            return false;
        }
        self.current = current;
        self.answer = TextEdit::default();
        self.error = None;
        true
    }
}

pub(super) fn panel(p: Paint, pane: &AgentPane, snapshot: &Snapshot) -> El {
    if let Some(question) = &snapshot.window_question {
        return window_panel(p, pane, question);
    }
    let Some(question) = &snapshot.computer_question else {
        return col().key("computer-question-none");
    };
    let mut content = col()
        .key("computer-question")
        .gap(8.0)
        .child(
            text(question.prompt.clone())
                .font(DESCRIPTION)
                .color(p.text())
                .wrap(),
        )
        .child(
            text(format!(
                "Task {} · step {} · {} seconds remaining to answer, review and finish",
                question.task_id,
                question.step + 1,
                question.expires_in_ms.div_ceil(1000)
            ))
            .font(DESCRIPTION)
            .color(p.muted())
            .wrap(),
        );
    if let Some(answer) = &question.pending_answer {
        return content.child(
            text(format!(
                "Your answer: {answer}\nReview this answer before resuming the same app and step."
            ))
            .key("computer-answer-review")
            .font(DESCRIPTION)
            .color(p.text())
            .wrap(),
        );
    }
    let task_id = question.task_id;
    let token = question.question_id.clone();
    content = content
        .child(
            TextInput::new(&pane.dialogue.answer)
                .placeholder("Answer this question (up to 300 characters)")
                .width(None)
                .on_edit(|ui: &mut Ui| &mut ui.agent.dialogue.answer)
                .build(p.k)
                .key("computer-answer")
                .label("Answer the current computer question"),
        )
        .child(button(
            p,
            "computer-answer-review-button",
            "Review answer",
            ButtonKind::Primary,
            move |ui: &mut Ui, _| {
                let result = agent_host::service().answer_computer(
                    task_id,
                    &token,
                    &ui.agent.dialogue.answer.text,
                );
                ui.agent.dialogue.error = result.err();
            },
        ));
    if let Some(error) = &pane.dialogue.error {
        content = content.child(
            text(error.clone())
                .font(DESCRIPTION)
                .color(p.danger())
                .wrap(),
        );
    }
    content
}

fn window_panel(p: Paint, pane: &AgentPane, question: &x_agent::service::WindowQuestion) -> El {
    let mut content = col()
        .key("window-question")
        .gap(8.0)
        .child(
            text(format!("Choose a window for: {}", question.operation))
                .key("window-question-prompt")
                .font(DESCRIPTION)
                .color(p.text())
                .wrap(),
        )
        .child(
            text(format!(
                "Task {} · {} seconds remaining to select, review and act",
                question.task_id,
                question.expires_in_ms.div_ceil(1000)
            ))
            .font(DESCRIPTION)
            .color(p.muted())
            .wrap(),
        );
    for choice in &question.choices {
        let number = choice.number;
        let id = question.task_id;
        let token = question.question_id.clone();
        let selected = question.selected == Some(number);
        let label = format!(
            "{number}. {}{}",
            choice.context.value.title,
            if selected { " · selected" } else { "" }
        );
        content = content.child(
            col()
                .key(format!("window-choice-{number}"))
                .gap(4.0)
                .child(
                    text(label)
                        .key(format!("window-choice-label-{number}"))
                        .font(DESCRIPTION)
                        .color(p.text())
                        .wrap(),
                )
                .child(button(
                    p,
                    &format!("window-select-{number}"),
                    &format!("Select window {number}"),
                    ButtonKind::Secondary,
                    move |ui: &mut Ui, _| {
                        ui.agent.dialogue.error = agent_host::service()
                            .answer_window(id, &token, &number.to_string())
                            .err();
                    },
                )),
        );
    }
    if question.selected.is_some() {
        content = content.child(text("Review the selected window and unchanged operation before approving. You can correct the choice below.")
            .key("window-selection-review").font(DESCRIPTION).color(p.text()).wrap());
    }
    let id = question.task_id;
    let token = question.question_id.clone();
    content = content
        .child(
            TextInput::new(&pane.dialogue.answer)
                .placeholder("A number, 'other', or 'previous'")
                .width(None)
                .on_edit(|ui: &mut Ui| &mut ui.agent.dialogue.answer)
                .build(p.k)
                .key("window-answer")
                .label("Answer the current window question"),
        )
        .child(button(
            p,
            "window-answer-review",
            "Review selection",
            ButtonKind::Secondary,
            move |ui: &mut Ui, _| {
                ui.agent.dialogue.error = agent_host::service()
                    .answer_window(id, &token, &ui.agent.dialogue.answer.text)
                    .err();
            },
        ));
    if let Some(error) = &pane.dialogue.error {
        content = content.child(
            text(error.clone())
                .font(DESCRIPTION)
                .color(p.danger())
                .wrap(),
        );
    }
    content
}

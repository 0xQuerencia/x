//! A clarification stays attached to its original task and exact question token.

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
            .map(|question| (question.task_id, question.question_id.clone()));
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

//! The local Work review uses durable claim identities and independent Git evidence.

use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};

use rune::{El, TextEdit, col, row, text};
use rune_kit::TextInput;
use x_agent::service::{CodingReviewEntry, CorrectionReview};
use x_agent::workflows::recovery::State;
use x_agent::worktrees::WorktreeReview;

use crate::agent_host;
use crate::rune_ui::controls::{ButtonKind, DESCRIPTION, Paint, button};
use crate::rune_ui::{Ui, list_detail};

enum Reply {
    Entries(Vec<CodingReviewEntry>),
    Review(Box<WorktreeReview>),
    Correction(Box<CorrectionReview>),
    Note(&'static str),
}

#[derive(Default)]
pub(super) struct Results {
    expanded: bool,
    entries: Vec<CodingReviewEntry>,
    selected: Option<(String, String, State)>,
    review: Option<Box<WorktreeReview>>,
    correction: Option<Box<CorrectionReview>>,
    goal: TextEdit,
    pending: Option<Receiver<Result<Reply, String>>>,
    note: Option<String>,
}

impl Results {
    pub(super) fn poll(&mut self) -> bool {
        let reply = match self.pending.as_ref().map(Receiver::try_recv) {
            Some(Ok(reply)) => reply,
            Some(Err(TryRecvError::Disconnected)) => {
                Err("Review stopped before returning a result".into())
            }
            _ => return false,
        };
        self.pending = None;
        if !self.expanded {
            return false;
        }
        match reply {
            Ok(Reply::Entries(entries)) => self.entries = entries,
            Ok(Reply::Review(review)) => self.review = Some(review),
            Ok(Reply::Correction(review)) => self.correction = Some(review),
            Ok(Reply::Note(note)) => self.note = Some(note.into()),
            Err(error) => self.note = Some(error),
        }
        true
    }

    fn start(&mut self, work: impl FnOnce() -> Result<Reply, String> + Send + 'static) {
        if self.pending.is_some() {
            return;
        }
        self.note = None;
        let (send, receive) = sync_channel(1);
        match std::thread::Builder::new()
            .name("x-work-result-review".into())
            .spawn(move || {
                let _ = send.send(work());
            }) {
            Ok(_) => self.pending = Some(receive),
            Err(error) => self.note = Some(format!("Could not start the local review: {error}")),
        }
    }

    fn list(&mut self) {
        if self.pending.is_none() {
            self.selected = None;
            self.review = None;
            self.correction = None;
            self.goal = TextEdit::default();
            self.start(|| {
                agent_host::service()
                    .recent_coding_reviews()
                    .map(Reply::Entries)
            });
        }
    }

    fn inspect(&mut self, run: String, node: String, state: State) {
        if self.pending.is_none() {
            self.selected = Some((run.clone(), node.clone(), state));
            self.review = None;
            self.correction = None;
            self.goal = TextEdit::default();
            self.start(move || {
                agent_host::service()
                    .coding_worktree_review(&run, &node)
                    .map(|review| Reply::Review(Box::new(review)))
            });
        }
    }
}

fn label(state: State) -> &'static str {
    match state {
        State::Claimed => "Running or interrupted",
        State::Delivered => "Delivery recorded",
        State::Verified => "Evidence recorded",
        State::Failed => "Failed",
        State::Unverified => "Unverified",
        State::Cancelled => "Stopped",
    }
}

pub(super) fn panel(p: Paint, results: &Results) -> El {
    let mut view = col().key("agent-work-results").gap(8.0).child(button(
        p,
        "agent-work-results-toggle",
        if results.expanded {
            "Hide Work results"
        } else {
            "Review Work results"
        },
        ButtonKind::Secondary,
        |ui: &mut Ui, _| {
            let results = &mut ui.agent.results;
            results.expanded = !results.expanded;
            if results.expanded {
                results.list();
            } else {
                results.entries.clear();
                results.selected = None;
                results.review = None;
                results.correction = None;
                results.goal = TextEdit::default();
                results.note = None;
            }
        },
    ));
    if !results.expanded {
        return view;
    }
    if let Some((run, node, state)) = &results.selected {
        let (open_run, open_node) = (run.clone(), node.clone());
        view = view
            .child(line(p, format!("{node} · {}", label(*state))))
            .child(line(p, format!("Run {run}")))
            .child(
                row()
                    .gap(8.0)
                    .child(button(
                        p,
                        "work-result-back",
                        "Back to results",
                        ButtonKind::Secondary,
                        |ui: &mut Ui, _| ui.agent.results.list(),
                    ))
                    .child(button(
                        p,
                        "work-result-open",
                        "Open checkout",
                        ButtonKind::Secondary,
                        move |ui: &mut Ui, _| {
                            let (run, node) = (open_run.clone(), open_node.clone());
                            ui.agent.results.start(move || {
                                agent_host::service()
                                    .open_coding_worktree(&run, &node)
                                    .map(|()| {
                                        Reply::Note(
                                            "Requested the exact checkout in the file manager.",
                                        )
                                    })
                            });
                        },
                    )),
            );
        if let Some(correction) = &results.correction {
            return finish_panel(
                p,
                results,
                view.child(correction_panel(p, results, correction)),
            );
        }
        if let Some(review) = &results.review {
            view = view.child(evidence(p, review));
            if *state == State::Verified
                && review.clean_inputs
                && review.recorded_artifact.as_deref() == Some(review.current_commit.as_str())
                && review.test_evidence.as_ref().is_none_or(|test| {
                    test.passed == Some(true) && review.test_matches_checkout == Some(true)
                })
            {
                let (run, node, head) = (run.clone(), node.clone(), review.current_commit.clone());
                view = view.child(button(
                    p,
                    "work-result-integrate",
                    "Review fast-forward",
                    ButtonKind::Secondary,
                    move |ui: &mut Ui, _| {
                        let (run, node, head) = (run.clone(), node.clone(), head.clone());
                        ui.agent.results.start(move || {
                            agent_host::service()
                                .plan_coding_result_integration(&run, &node, &head)
                                .map(|_| {
                                    Reply::Note(
                                        "Review the new fast-forward plan above before approving.",
                                    )
                                })
                        });
                    },
                ));
            }
        }
        if results
            .review
            .as_ref()
            .and_then(|review| review.test_evidence.as_ref())
            .is_some_and(|test| test.passed == Some(false))
        {
            let source = format!("{run}/{node}");
            view = view.child(button(
                p,
                "work-result-review-correction",
                "Review failed test and correction",
                ButtonKind::Secondary,
                move |ui: &mut Ui, _| {
                    let source = source.clone();
                    ui.agent.results.start(move || {
                        agent_host::service()
                            .review_correction_failure(&source)
                            .map(|review| Reply::Correction(Box::new(review)))
                    });
                },
            ));
        }
    } else if results.entries.is_empty() && results.pending.is_none() && results.note.is_none() {
        view = view.child(line(p, "No recorded Work checkouts."));
    } else {
        for entry in &results.entries {
            let (run, node, state) = (entry.run_id.clone(), entry.node_id.clone(), entry.state);
            let prefix = run.split('-').next().unwrap_or(&run);
            let name = format!("{node} · {} · {prefix}", label(state));
            view = view.child(button(
                p,
                &format!("work-result-{run}-{node}"),
                &name,
                ButtonKind::Secondary,
                move |ui: &mut Ui, _| ui.agent.results.inspect(run.clone(), node.clone(), state),
            ));
        }
    }
    finish_panel(p, results, view)
}

fn finish_panel(p: Paint, results: &Results, mut view: El) -> El {
    if results.pending.is_some() {
        view = view.child(line(p, "Checking the recorded Work result…"));
    }
    if let Some(note) = &results.note {
        view = view.child(line(p, note.clone()));
    }
    view
}

fn line(p: Paint, value: impl Into<String>) -> El {
    text(value.into()).font(DESCRIPTION).color(p.text()).wrap()
}

fn correction_panel(p: Paint, results: &Results, review: &CorrectionReview) -> El {
    let failure = &review.failure;
    let mut view = col()
        .key("work-result-correction")
        .gap(8.0)
        .child(line(
            p,
            format!("{} failed at {}", failure.suite.label(), failure.commit),
        ))
        .child(line(
            p,
            "Test output stays local. Enter only the information you want OpenCode to receive.",
        ));
    let excerpt = &failure.excerpt;
    for (title, value, truncated) in [
        (
            "Standard output",
            excerpt.stdout.as_str(),
            excerpt.stdout_truncated,
        ),
        (
            "Standard error",
            excerpt.stderr.as_str(),
            excerpt.stderr_truncated,
        ),
    ] {
        if value.is_empty() {
            continue;
        }
        view = view
            .child(line(
                p,
                if truncated {
                    format!("{title} · truncated tail")
                } else {
                    title.into()
                },
            ))
            .child(
                col().h(130.0).child(list_detail::card_scroll(
                    title,
                    text(value.to_owned())
                        .font(p.k.ty.mono)
                        .color(p.text())
                        .wrap(),
                )),
            );
    }
    let Some(attempt) = review.next_attempt else {
        return view.child(line(
            p,
            "This test result has no remaining correction approval.",
        ));
    };
    let source = format!("{}/{}", failure.run_id, failure.node_id);
    view.child(line(
        p,
        format!("Correction attempt {attempt} of {}.", review.max_attempts),
    ))
    .child(line(
        p,
        format!(
            "OpenCode makes an isolated commit, then X reruns {} on that commit.",
            failure.suite.label()
        ),
    ))
    .child(
        TextInput::new(&results.goal)
            .placeholder("Correction goal")
            .width(None)
            .on_edit(|ui: &mut Ui| &mut ui.agent.results.goal)
            .build(p.k)
            .key("work-result-correction-goal"),
    )
    .child(button(
        p,
        "work-result-plan-correction",
        "Review correction plan",
        ButtonKind::Secondary,
        move |ui: &mut Ui, _| {
            let source = source.clone();
            let results = &mut ui.agent.results;
            let goal = results.goal.text.trim().to_owned();
            if goal.is_empty() {
                results.note = Some("Enter an explicit correction goal.".into());
                return;
            }
            results.start(move || {
                agent_host::service()
                    .plan_correction(&source, &goal)
                    .map(|_| Reply::Note("Review and approve the separate correction plan above."))
            });
        },
    ))
}

fn evidence(p: Paint, review: &WorktreeReview) -> El {
    let mut view = col()
        .key("work-result-evidence")
        .gap(6.0)
        .child(line(p, format!("Checkout {}", review.path.display())))
        .child(line(p, format!("Base {}", review.base_commit)))
        .child(line(p, format!("Head {}", review.current_commit)));
    if let Some(commit) = &review.recorded_artifact {
        view = view.child(line(p, format!("Recorded artifact {commit}")));
        if commit != &review.current_commit {
            view = view.child(line(
                p,
                "The recorded artifact differs from the current head.",
            ));
        }
    }
    if review.clean_inputs {
        view = view.child(line(p, "Tracked and untracked inputs are clean."));
    } else {
        if review.tracked_changes {
            view = view.child(line(p, "Tracked changes are outside the committed patch."));
        }
        if review.untracked_files {
            view = view.child(line(p, "Untracked files are outside the committed patch."));
        }
    }
    if let Some(test) = &review.test_evidence {
        let result = match test.passed {
            Some(true) => "passed",
            Some(false) => "failed",
            None => "unverified",
        };
        view = view.child(line(
            p,
            format!("{} {result} at {}", test.suite.label(), test.commit),
        ));
        if review.test_matches_checkout != Some(true) {
            view = view.child(line(p, "The recorded test does not verify this checkout."));
        }
    } else {
        view = view.child(line(p, "No test result was recorded for this checkout."));
    }
    match review.binary_changes {
        Some(true) => view = view.child(line(p, "The committed changes include binary files.")),
        None => view = view.child(line(p, "Binary file status is incomplete.")),
        _ => {}
    }
    for file in &review.files {
        view = view.child(line(p, format!("{}  {}", file.status, file.path)));
    }
    if review.files_truncated {
        view = view.child(line(p, "The committed file list is truncated."));
    }
    if review.diff_truncated {
        view = view.child(line(p, "The committed patch is truncated."));
    }
    if review.committed_diff.is_empty() {
        view.child(line(p, "No committed patch."))
    } else {
        view.child(
            col().h(220.0).child(list_detail::card_scroll(
                "work-result-patch",
                text(review.committed_diff.clone())
                    .font(p.k.ty.mono)
                    .color(p.text())
                    .wrap(),
            )),
        )
    }
}

#[cfg(test)]
pub(super) fn fixture(stale: bool) -> Results {
    use x_agent::verification::test_runner::{Suite, TestEvidence};
    use x_agent::worktrees::ChangedFile;

    let base = "a".repeat(40);
    let head = "b".repeat(40);
    Results {
        expanded: true,
        selected: Some((
            "ca299c56-b96d-4e0d-ab8c-031f6891f3b9".into(),
            "check-dates".into(),
            State::Verified,
        )),
        review: Some(Box::new(WorktreeReview {
            path: "/home/you/X Worktrees/calendar/check-dates".into(),
            base_commit: base.clone(),
            current_commit: head.clone(),
            recorded_artifact: Some(if stale { base.clone() } else { head.clone() }),
            clean_inputs: !stale,
            tracked_changes: false,
            untracked_files: stale,
            binary_changes: Some(false),
            files: vec![ChangedFile { status: 'M', path: "src/date.rs".into() }],
            files_truncated: stale,
            committed_diff: "diff --git a/src/date.rs b/src/date.rs\n--- a/src/date.rs\n+++ b/src/date.rs\n@@ -1 +1 @@\n-fn end() -> u32 { 29 }\n+fn end() -> u32 { 30 }".into(),
            diff_truncated: stale,
            test_evidence: Some(TestEvidence {
                suite: Suite::CargoWorkspace,
                commit: if stale { base } else { head },
                passed: Some(true),
                observed_at: 1,
            }),
            test_matches_checkout: Some(!stale),
        })),
        ..Results::default()
    }
}

#[cfg(test)]
pub(super) fn correction_fixture(exhausted: bool) -> Results {
    use x_agent::verification::test_runner::{CorrectionDraft, FailureExcerpt, Suite};
    let mut result = fixture(false);
    let selected = result.selected.as_mut().unwrap();
    selected.2 = State::Failed;
    result.correction = Some(Box::new(CorrectionReview {
        failure: CorrectionDraft {
            project_id: "cd2eb7c6-9b31-4cfe-b990-5b69061f4c25".parse().unwrap(),
            run_id: selected.0.parse().unwrap(),
            node_id: selected.1.clone(),
            commit: "b".repeat(40),
            suite: Suite::CargoWorkspace,
            excerpt: FailureExcerpt {
                stdout: "test calendar::ends_on_the_selected_date … FAILED".into(),
                stderr: "assertion failed
  expected 30, got 29"
                    .into(),
                stdout_truncated: false,
                stderr_truncated: true,
            },
        },
        next_attempt: (!exhausted).then_some(2),
        max_attempts: 2,
    }));
    result
}

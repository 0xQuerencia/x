//! The bounded Computer decision loop and retained-progress accounting.

use super::*;

pub(super) fn drive(
    mut continuation: Continuation,
    desktop: &dyn Desktop,
    decide: &DecisionFn<'_>,
    host: &dyn Host,
) -> Outcome {
    let mut goal = continuation.goal.clone();
    for answer in &continuation.answers {
        goal.push_str("\nUser detail for this same app and goal: ");
        goal.push_str(answer);
    }
    let app = continuation.app.clone();
    let start = continuation.started;
    let limits = continuation.limits;
    let p = &mut continuation.progress;
    // A pending wait exists only between decisions. Ask is reached after its
    // observation, so no old readiness result is carried across a human pause.
    let mut pending_change = None;
    for turn in continuation.next_turn..limits.steps {
        if host.stopped() {
            return Outcome::Cancelled;
        }
        if start.elapsed() > limits.deadline {
            return Outcome::Failed("Stopped: the time limit was reached".into());
        }
        let wait = pending_change.take();
        let ready = match (readiness::Observer {
            desktop,
            app: &app,
            window: &continuation.window,
            host,
            started: start,
            deadline: limits.deadline,
        })
        .after_change(
            wait.map(|_| p.last_render.as_str()),
            wait.unwrap_or_default(),
        ) {
            Ok(ready) => ready,
            Err(_) if host.stopped() => return Outcome::Cancelled,
            Err(why) => return Outcome::Failed(why),
        };
        if ready.timed_out {
            host.emit(
                "info",
                "No visible change before the readiness wait limit; using fresh context",
            );
        }
        let win = ready.window;
        let obs = ready.observation;
        let render = obs.render();
        if turn == 0 {
            p.first = obs.clone();
        } else if !p.acted_last {
            p.unchanged = 0;
        } else if render == p.last_render {
            p.unchanged += 1;
            host.emit("verify", "No visible change after the last step");
        } else {
            p.unchanged = 0;
            host.emit("verify", "The screen changed after the last step");
            let fresh = new_on_screen(&p.prev, &obs);
            if let Some(last) = p.history.last_mut() {
                last.push_str(&format!(" => screen changed, new: {fresh}"));
            }
        }
        p.prev = obs.clone();
        p.last_render = render;
        host.emit(
            "observe",
            &format!("{:?}: {} controls", obs.title, obs.elements.len()),
        );
        if p.unchanged >= 2 {
            return Outcome::Failed("Stopped: two steps in a row changed nothing".into());
        }
        p.acted_last = false;
        if host.stopped() {
            return Outcome::Cancelled;
        }
        if start.elapsed() >= limits.deadline {
            return Outcome::Failed("Stopped: the time limit was reached".into());
        }
        let step = decide(&goal, &app, &obs, &p.history);
        if host.stopped() {
            return Outcome::Cancelled;
        }
        if start.elapsed() > limits.deadline {
            return Outcome::Failed("Stopped: the time limit was reached".into());
        }
        let step = match step {
            Ok(step) => step,
            Err(e) => {
                p.errors += 1;
                host.emit("error", &e);
                p.history.push(format!("(rejected: {e})"));
                if p.errors >= limits.errors {
                    return Outcome::Failed(format!("Stopped after repeated failures: {e}"));
                }
                continue;
            }
        };
        match step {
            Step::Ask(q) => {
                if continuation.answers.len() >= MAX_CLARIFICATIONS {
                    return Outcome::Failed("Stopped: the clarification limit was reached".into());
                }
                let question = match check_answer(&q) {
                    Ok(question) => crate::desktop::prompt_text(&question),
                    Err(_) => {
                        return Outcome::Failed("The model supplied an unusable question".into());
                    }
                };
                if question.is_empty() {
                    return Outcome::Failed("The model supplied an unusable question".into());
                }
                continuation.question = question;
                continuation.next_turn = turn + 1;
                if let Err(why) = continuation.check_live() {
                    return Outcome::Failed(why);
                }
                return Outcome::Waiting(Box::new(continuation));
            }
            Step::Fail(why) => return Outcome::Failed(format!("Cannot be done: {why}")),
            Step::Wait => {
                p.history.push(
                    "(waiting for changed interface context, not assuming loading is complete)"
                        .into(),
                );
                pending_change = Some(REQUESTED_WAIT);
                continue;
            }
            Step::Done {
                evidence,
                summary: _,
            } => {
                if !obs.shows_exact(&evidence) {
                    p.errors += 1;
                    host.emit("error", "Completion claimed without evidence on screen");
                    p.history
                        .push("(done rejected: that evidence is not on screen now)".into());
                } else if p.acts > 0 && p.first.shows(&evidence) {
                    p.errors += 1;
                    p.history
                        .push("(done rejected: that was already visible before you acted)".into());
                } else {
                    host.emit("verify", &format!("Found on screen: {evidence:?}"));
                    return Outcome::Observed(format!("Observed on screen: {evidence:?}"));
                }
                if p.errors >= limits.errors {
                    return Outcome::Failed("Stopped: could not verify the result".into());
                }
            }
            action => {
                let wait = match &action {
                    Step::Navigate(_)
                    | Step::Key(
                        Key::Enter | Key::Back | Key::Forward | Key::Reload | Key::NewTab,
                    ) => NAVIGATION_WAIT,
                    _ => CHANGE_WAIT,
                };
                let scope = ActionScope {
                    host,
                    started: start,
                    deadline: limits.deadline,
                };
                match act(desktop, &win, &obs, action, p, &scope, &goal) {
                    Ok(()) => {
                        continuation.stale_retry_used = false;
                        p.acts += 1;
                        p.acted_last = true;
                        pending_change = Some(wait);
                    }
                    Err(_) if host.stopped() => return Outcome::Cancelled,
                    Err(Refusal::Stop(reason)) => return Outcome::Failed(reason),
                    Err(Refusal::Retry(reason)) => {
                        p.typed_into_navigation = false;
                        if reason == "That control changed; observe again"
                            && !continuation.stale_retry_used
                        {
                            // The original element reference is stale. Observe
                            // afresh and ask for a NEW grounded choice; never
                            // replay the old reference on an updated screen.
                            continuation.stale_retry_used = true;
                            p.history
                                .push("(control changed: re-observing once)".into());
                            host.emit("info", "A control changed; getting a fresh snapshot");
                            continue;
                        }
                        p.errors += 1;
                        host.emit("error", &reason);
                        p.history.push(format!("(failed: {reason})"));
                        if p.errors >= limits.errors {
                            return Outcome::Failed(format!(
                                "Stopped after repeated failures: {reason}"
                            ));
                        }
                    }
                }
            }
        }
    }
    Outcome::Failed(format!("Stopped: {} steps without finishing", limits.steps))
}

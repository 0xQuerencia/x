use super::*;
use crate::desktop::Element;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct Screen {
    reads: AtomicUsize,
    changed: AtomicBool,
    missing: AtomicBool,
    acts: Mutex<Vec<UiAct>>,
}

impl Desktop for Screen {
    fn find_window(&self, app: &str) -> Result<Win, String> {
        if self.missing.load(Ordering::SeqCst) {
            return Err("The original window is gone".into());
        }
        Ok(Win {
            address: "0x1".into(),
            pid: if self.changed.load(Ordering::SeqCst) {
                2
            } else {
                1
            },
            class: app.into(),
            title: "Search".into(),
            id: "window".into(),
        })
    }
    fn observe(&self, _: &Win) -> Result<Observation, String> {
        let sequence = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Observation {
            snapshot: format!("s{sequence}"),
            title: "Search".into(),
            elements: vec![Element {
                id: "e1".into(),
                reference: format!("@s{sequence}:e1"),
                role: "searchbox".into(),
                name: "Search".into(),
                value: String::new(),
                actions: vec!["Click".into(), "SetFocus".into()],
            }],
            texts: vec![
                if self.acts.lock().unwrap().is_empty() {
                    "idle"
                } else {
                    "result loaded"
                }
                .into(),
            ],
        })
    }
    fn act(&self, _: &Win, act: &UiAct) -> Result<(), String> {
        self.acts.lock().unwrap().push(act.clone());
        Ok(())
    }
}

struct Quiet;
impl Host for Quiet {
    fn emit(&self, _: &str, _: &str) {}
    fn stopped(&self) -> bool {
        false
    }
    fn open_app(&self, _: &str) -> Result<(), String> {
        panic!("a continuation must never launch an app")
    }
}

fn waiting(outcome: Outcome) -> Continuation {
    match outcome {
        Outcome::Waiting(continuation) => *continuation,
        other => panic!("expected a live question, got {other:?}"),
    }
}

fn ask(desktop: &Screen, limits: &Limits) -> Continuation {
    waiting(run_with(
        "chrome",
        "open the result",
        desktop,
        &|_, _, _, _| Ok(Step::Ask("Which result?".into())),
        &Quiet,
        limits,
    ))
}

#[test]
fn resumed_loop_uses_fresh_observation_and_retains_prior_effects() {
    let desktop = Screen::default();
    let continuation = waiting(run_with(
        "chrome",
        "open the result",
        &desktop,
        &|_, _, obs, _| {
            Ok(if obs.snapshot == "s1" {
                Step::Click("e1".into())
            } else {
                Step::Ask("Which result?".into())
            })
        },
        &Quiet,
        &Limits::default(),
    ));
    assert_eq!(continuation.next_turn, 2);
    assert_eq!(continuation.progress.acts, 1);
    let outcome = resume_with(
        continuation,
        "the blue result",
        &desktop,
        &|goal, app, obs, history| {
            assert_eq!(app, "chrome");
            assert!(goal.starts_with("open the result"));
            assert!(goal.ends_with("the blue result"));
            assert_eq!(obs.snapshot, "s3");
            assert!(history.iter().any(|item| item.contains("clicked")));
            Ok(Step::Done {
                evidence: "result loaded".into(),
                summary: "ignore generated claim".into(),
            })
        },
        &Quiet,
    );
    assert!(matches!(outcome, Outcome::Observed(_)), "{outcome:?}");
    assert_eq!(
        *desktop.acts.lock().unwrap(),
        vec![UiAct::Click("@s1:e1".into())]
    );
}

#[test]
fn resuming_does_not_reset_original_evidence_or_error_budget() {
    let desktop = Screen::default();
    let decisions = AtomicUsize::new(0);
    let continuation = waiting(run_with(
        "chrome",
        "open the result",
        &desktop,
        &|_, _, _, _| match decisions.fetch_add(1, Ordering::SeqCst) {
            0 => Err("a rejected decision".into()),
            1 => Ok(Step::Click("e1".into())),
            _ => Ok(Step::Ask("Which result?".into())),
        },
        &Quiet,
        &Limits {
            errors: 2,
            ..Limits::default()
        },
    ));
    assert_eq!(continuation.progress.errors, 1);
    let outcome = resume_with(
        continuation,
        "the blue result",
        &desktop,
        &|_, _, _, _| {
            Ok(Step::Done {
                evidence: "Search".into(),
                summary: String::new(),
            })
        },
        &Quiet,
    );
    assert!(
        matches!(outcome, Outcome::Failed(ref why) if why.contains("verify")),
        "{outcome:?}"
    );
    assert_eq!(desktop.acts.lock().unwrap().len(), 1);
}

#[test]
fn questions_cannot_reset_or_exceed_total_decision_budget() {
    let desktop = Screen::default();
    let continuation = ask(
        &desktop,
        &Limits {
            steps: 2,
            ..Limits::default()
        },
    );
    let outcome = resume_with(
        continuation,
        "blue",
        &desktop,
        &|_, _, _, _| Ok(Step::Ask("Which shade?".into())),
        &Quiet,
    );
    assert!(
        matches!(outcome, Outcome::Failed(ref why) if why.contains("no decision steps remaining")),
        "{outcome:?}"
    );
    assert_eq!(desktop.reads.load(Ordering::SeqCst), 2);
    assert!(desktop.acts.lock().unwrap().is_empty());
}

#[test]
fn expired_and_stopped_continuations_never_observe_or_decide() {
    struct Stopped;
    impl Host for Stopped {
        fn emit(&self, _: &str, _: &str) {}
        fn stopped(&self) -> bool {
            true
        }
        fn open_app(&self, _: &str) -> Result<(), String> {
            panic!("stopped")
        }
    }
    let desktop = Screen::default();
    let mut continuation = ask(&desktop, &Limits::default());
    continuation.started = Instant::now() - Duration::from_secs(181);
    let outcome = resume_with(
        continuation,
        "blue",
        &desktop,
        &|_, _, _, _| panic!("expired task decided"),
        &Quiet,
    );
    assert!(matches!(outcome, Outcome::Failed(ref why) if why.contains("expired")));
    assert_eq!(desktop.reads.load(Ordering::SeqCst), 1);
    let continuation = ask(&desktop, &Limits::default());
    assert_eq!(
        resume_with(
            continuation,
            "blue",
            &desktop,
            &|_, _, _, _| panic!("stopped task decided"),
            &Stopped
        ),
        Outcome::Cancelled
    );
    assert_eq!(desktop.reads.load(Ordering::SeqCst), 2);
}

#[test]
fn a_replaced_or_missing_window_cannot_be_reopened_on_resume() {
    for missing in [false, true] {
        let desktop = Screen::default();
        let continuation = ask(&desktop, &Limits::default());
        desktop.missing.store(missing, Ordering::SeqCst);
        desktop.changed.store(!missing, Ordering::SeqCst);
        let outcome = resume_with(
            continuation,
            "blue",
            &desktop,
            &|_, _, _, _| panic!("stale window decided"),
            &Quiet,
        );
        assert!(matches!(outcome, Outcome::Failed(_)), "{outcome:?}");
        assert_eq!(desktop.reads.load(Ordering::SeqCst), 1);
        assert!(desktop.acts.lock().unwrap().is_empty());
    }
}

#[test]
fn deadline_expiry_during_fresh_context_reads_cannot_reach_the_model() {
    struct Slow<'a>(&'a Screen, bool);
    impl Desktop for Slow<'_> {
        fn find_window(&self, app: &str) -> Result<Win, String> {
            if self.1 {
                std::thread::sleep(Duration::from_millis(120));
            }
            self.0.find_window(app)
        }
        fn observe(&self, win: &Win) -> Result<Observation, String> {
            assert!(!self.1, "an expired window rebind must not observe");
            std::thread::sleep(Duration::from_millis(120));
            self.0.observe(win)
        }
        fn act(&self, _: &Win, _: &UiAct) -> Result<(), String> {
            panic!("expired")
        }
    }
    for during_rebind in [false, true] {
        let desktop = Screen::default();
        let mut continuation = ask(&desktop, &Limits::default());
        continuation.limits.deadline = continuation.started.elapsed() + Duration::from_millis(100);
        let outcome = resume_with(
            continuation,
            "blue",
            &Slow(&desktop, during_rebind),
            &|_, _, _, _| panic!("expired task reached model"),
            &Quiet,
        );
        assert!(
            matches!(outcome, Outcome::Failed(ref why) if why.contains("time limit")),
            "{outcome:?}"
        );
        assert!(desktop.acts.lock().unwrap().is_empty());
    }
}

#[test]
fn pausing_invalidates_the_transient_right_to_press_enter() {
    let desktop = Screen::default();
    let continuation = waiting(run_with(
        "chrome",
        "search for blue",
        &desktop,
        &|_, _, obs, _| {
            Ok(if obs.snapshot == "s1" {
                Step::Type("e1".into(), "blue".into())
            } else {
                Step::Ask("Which result?".into())
            })
        },
        &Quiet,
        &Limits {
            errors: 1,
            ..Limits::default()
        },
    ));
    assert!(continuation.progress.typed_into_navigation);
    let outcome = resume_with(
        continuation,
        "blue",
        &desktop,
        &|_, _, _, _| Ok(Step::Key(Key::Enter)),
        &Quiet,
    );
    assert!(
        matches!(outcome, Outcome::Failed(ref why) if why.contains("right after typing")),
        "{outcome:?}"
    );
    assert_eq!(
        *desktop.acts.lock().unwrap(),
        vec![UiAct::Focus("@s1:e1".into()), UiAct::Type("blue".into())]
    );
}

#[test]
fn answers_and_questions_are_bounded_and_questions_are_redacted() {
    let desktop = Screen::default();
    for answer in [
        String::new(),
        "x".repeat(MAX_ANSWER + 1),
        "blue\nred".into(),
    ] {
        let continuation = ask(&desktop, &Limits::default());
        let before = desktop.reads.load(Ordering::SeqCst);
        let outcome = resume_with(
            continuation,
            &answer,
            &desktop,
            &|_, _, _, _| panic!("invalid answer decided"),
            &Quiet,
        );
        assert!(matches!(outcome, Outcome::Failed(_)));
        assert_eq!(desktop.reads.load(Ordering::SeqCst), before);
    }
    let continuation = waiting(run_with(
        "chrome",
        "open result",
        &desktop,
        &|_, _, _, _| {
            Ok(Step::Ask(
                "Use person@example.com or account 123456789?".into(),
            ))
        },
        &Quiet,
        &Limits::default(),
    ));
    assert!(!continuation.question().contains("example.com"));
    assert!(!continuation.question().contains("123456789"));
    assert!(continuation.question().contains("[redacted]"));
    assert!(!format!("{continuation:?}").contains("idle"));
}

#[test]
fn repeated_questions_remain_bounded_in_the_same_session() {
    let desktop = Screen::default();
    let mut continuation = ask(&desktop, &Limits::default());
    for _ in 0..MAX_CLARIFICATIONS - 1 {
        continuation = waiting(resume_with(
            continuation,
            "blue",
            &desktop,
            &|_, _, _, _| Ok(Step::Ask("Which blue?".into())),
            &Quiet,
        ));
    }
    let outcome = resume_with(
        continuation,
        "blue",
        &desktop,
        &|_, _, _, _| Ok(Step::Ask("Again?".into())),
        &Quiet,
    );
    assert!(
        matches!(outcome, Outcome::Failed(ref why) if why.contains("clarification limit")),
        "{outcome:?}"
    );
    assert!(desktop.acts.lock().unwrap().is_empty());
}

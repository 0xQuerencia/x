use super::*;
use crate::cancellation::Cancellation;
use crate::desktop::Element;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Harness {
    stop: Cancellation,
    events: Mutex<Vec<String>>,
}
impl Default for Harness {
    fn default() -> Self {
        Self {
            stop: Cancellation::default(),
            events: Mutex::new(vec![]),
        }
    }
}
impl Host for Harness {
    fn emit(&self, _: &str, text: &str) {
        self.events.lock().unwrap().push(text.into());
    }
    fn stopped(&self) -> bool {
        self.stop.requested()
    }
    fn cancellation(&self) -> Option<&Cancellation> {
        Some(&self.stop)
    }
    fn open_app(&self, _: &str) -> Result<(), String> {
        panic!("fixture already has its exact window")
    }
}

struct Delayed {
    delay: Duration,
    changed_at: Mutex<Option<Instant>>,
    reads: AtomicUsize,
    actions: Mutex<Vec<UiAct>>,
}
impl Delayed {
    fn new(delay: Duration) -> Self {
        Self {
            delay,
            changed_at: Mutex::new(None),
            reads: AtomicUsize::new(0),
            actions: Mutex::new(vec![]),
        }
    }
    fn window(app: &str) -> Win {
        Win {
            address: "0x1".into(),
            pid: 1,
            class: app.into(),
            title: "Search".into(),
            id: "original".into(),
        }
    }
}
impl Desktop for Delayed {
    fn find_window(&self, app: &str) -> Result<Win, String> {
        Ok(Self::window(app))
    }
    fn observe(&self, _: &Win) -> Result<Observation, String> {
        let generation = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        let ready = self
            .changed_at
            .lock()
            .unwrap()
            .is_some_and(|at| at.elapsed() >= self.delay);
        Ok(Observation {
            snapshot: format!("s{generation}"),
            title: "Search".into(),
            elements: vec![Element {
                id: "e1".into(),
                reference: format!("@s{generation}:e1"),
                role: "link".into(),
                name: "Open result".into(),
                value: String::new(),
                actions: vec!["Click".into()],
            }],
            texts: vec![
                if ready {
                    "Result loaded"
                } else {
                    "Waiting for result"
                }
                .into(),
            ],
        })
    }
    fn act(&self, _: &Win, action: &UiAct) -> Result<(), String> {
        if let UiAct::Click(reference) = action {
            assert_eq!(
                *reference,
                format!("@s{}:e1", self.reads.load(Ordering::SeqCst))
            );
        }
        self.actions.lock().unwrap().push(action.clone());
        if matches!(action, UiAct::Click(_) | UiAct::Key(Key::Enter)) {
            *self.changed_at.lock().unwrap() = Some(Instant::now());
        }
        Ok(())
    }
}

#[test]
fn fast_effect_is_observed_without_a_fixed_settle_sleep() {
    let desktop = Delayed::new(Duration::ZERO);
    let decisions = AtomicUsize::new(0);
    let start = Instant::now();
    let outcome = run_with(
        "chrome",
        "open the result",
        &desktop,
        &|_, _, obs, _| {
            Ok(match decisions.fetch_add(1, Ordering::SeqCst) {
                0 => Step::Click("e1".into()),
                1 => {
                    assert!(obs.shows_exact("Result loaded"));
                    Step::Done {
                        evidence: "Result loaded".into(),
                        summary: String::new(),
                    }
                }
                _ => panic!("unexpected repeated decision"),
            })
        },
        &Harness::default(),
        &Limits::default(),
    );
    assert!(matches!(outcome, Outcome::Observed(_)), "{outcome:?}");
    assert!(
        start.elapsed() < Duration::from_millis(700),
        "fast context waited for a fixed settle period"
    );
    assert_eq!(desktop.reads.load(Ordering::SeqCst), 2);
    assert_eq!(desktop.actions.lock().unwrap().len(), 1);
}

#[test]
fn slow_effect_is_polled_without_repeating_input_or_model_calls() {
    let desktop = Delayed::new(Duration::from_millis(160));
    let decisions = AtomicUsize::new(0);
    let outcome = run_with(
        "chrome",
        "open the result",
        &desktop,
        &|_, _, obs, _| {
            if decisions.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(Step::Click("e1".into()))
            } else {
                assert!(
                    obs.shows_exact("Result loaded"),
                    "the chooser was called before changed context"
                );
                Ok(Step::Done {
                    evidence: "Result loaded".into(),
                    summary: String::new(),
                })
            }
        },
        &Harness::default(),
        &Limits::default(),
    );
    assert!(matches!(outcome, Outcome::Observed(_)), "{outcome:?}");
    assert_eq!(decisions.load(Ordering::SeqCst), 2);
    assert!(desktop.reads.load(Ordering::SeqCst) >= 4);
    assert_eq!(desktop.actions.lock().unwrap().len(), 1);
}

#[test]
fn generation_changes_alone_time_out_as_unchanged_context() {
    let desktop = Delayed::new(Duration::from_secs(10));
    let window = Delayed::window("chrome");
    let first = desktop.observe(&window).unwrap();
    let host = Harness::default();
    let ready = readiness::Observer {
        desktop: &desktop,
        app: "chrome",
        window: &window,
        host: &host,
        started: Instant::now(),
        deadline: Duration::from_secs(2),
    }
    .after_change(Some(&first.render()), Duration::from_millis(60))
    .unwrap();
    assert!(ready.timed_out);
    assert_ne!(first.snapshot, ready.observation.snapshot);
    assert_eq!(first.render(), ready.observation.render());
    assert!(desktop.reads.load(Ordering::SeqCst) >= 3);
    assert!(desktop.actions.lock().unwrap().is_empty());
}

#[test]
fn stop_while_waiting_cancels_without_any_desktop_input() {
    let desktop = Delayed::new(Duration::from_secs(10));
    let host = Harness::default();
    let decisions = AtomicUsize::new(0);
    let trigger = Mutex::new(None);
    let outcome = run_with(
        "chrome",
        "read the result",
        &desktop,
        &|_, _, _, _| {
            assert_eq!(decisions.fetch_add(1, Ordering::SeqCst), 0);
            let stop = host.stop.clone();
            *trigger.lock().unwrap() = Some(std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(35));
                stop.request();
            }));
            Ok(Step::Wait)
        },
        &host,
        &Limits::default(),
    );
    trigger.into_inner().unwrap().unwrap().join().unwrap();
    assert_eq!(outcome, Outcome::Cancelled);
    assert_eq!(decisions.load(Ordering::SeqCst), 1);
    assert!(desktop.actions.lock().unwrap().is_empty());
}

#[test]
fn original_deadline_bounds_polling_without_another_decision() {
    let desktop = Delayed::new(Duration::from_secs(10));
    let decisions = AtomicUsize::new(0);
    let start = Instant::now();
    let outcome = run_with(
        "chrome",
        "open the result",
        &desktop,
        &|_, _, _, _| {
            assert_eq!(decisions.fetch_add(1, Ordering::SeqCst), 0);
            Ok(Step::Click("e1".into()))
        },
        &Harness::default(),
        &Limits {
            deadline: Duration::from_millis(80),
            ..Limits::default()
        },
    );
    assert!(
        matches!(outcome, Outcome::Failed(ref why) if why.contains("time limit")),
        "{outcome:?}"
    );
    assert!(start.elapsed() < Duration::from_millis(600));
    assert_eq!(desktop.actions.lock().unwrap().len(), 1);
    assert_eq!(decisions.load(Ordering::SeqCst), 1);
}

#[test]
fn every_readiness_poll_rebinds_the_original_window() {
    struct Replaced(Delayed);
    impl Desktop for Replaced {
        fn find_window(&self, app: &str) -> Result<Win, String> {
            let mut window = Delayed::window(app);
            if self.0.reads.load(Ordering::SeqCst) >= 2 {
                window.pid = 2;
            }
            Ok(window)
        }
        fn observe(&self, window: &Win) -> Result<Observation, String> {
            assert_eq!(window.pid, 1);
            self.0.observe(window)
        }
        fn act(&self, window: &Win, action: &UiAct) -> Result<(), String> {
            self.0.act(window, action)
        }
    }
    let desktop = Replaced(Delayed::new(Duration::from_secs(10)));
    let decisions = AtomicUsize::new(0);
    let outcome = run_with(
        "chrome",
        "open the result",
        &desktop,
        &|_, _, _, _| {
            assert_eq!(decisions.fetch_add(1, Ordering::SeqCst), 0);
            Ok(Step::Click("e1".into()))
        },
        &Harness::default(),
        &Limits::default(),
    );
    assert!(
        matches!(outcome, Outcome::Failed(ref why) if why.contains("changed identity")),
        "{outcome:?}"
    );
    assert_eq!(desktop.0.reads.load(Ordering::SeqCst), 2);
    assert_eq!(desktop.0.actions.lock().unwrap().len(), 1);
}

#[test]
fn navigation_keeps_inter_key_debounce_but_does_not_sleep_for_page_load() {
    let desktop = Delayed::new(Duration::ZERO);
    let decisions = AtomicUsize::new(0);
    let start = Instant::now();
    let outcome = run_with(
        "chrome",
        "open wikipedia",
        &desktop,
        &|_, _, obs, _| {
            Ok(if decisions.fetch_add(1, Ordering::SeqCst) == 0 {
                Step::Navigate("https://www.wikipedia.org".into())
            } else {
                assert!(obs.shows_exact("Result loaded"));
                Step::Done {
                    evidence: "Result loaded".into(),
                    summary: String::new(),
                }
            })
        },
        &Harness::default(),
        &Limits::default(),
    );
    assert!(matches!(outcome, Outcome::Observed(_)), "{outcome:?}");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "navigation retained a fixed page-load sleep"
    );
    assert_eq!(
        *desktop.actions.lock().unwrap(),
        vec![
            UiAct::Key(Key::Address),
            UiAct::Type("https://www.wikipedia.org".into()),
            UiAct::Key(Key::Enter)
        ]
    );
    assert_eq!(desktop.reads.load(Ordering::SeqCst), 2);
}

#[test]
fn readiness_timeout_is_explicit_and_does_not_claim_goal_proof() {
    let desktop = Delayed::new(Duration::from_secs(10));
    let host = Harness::default();
    let decisions = AtomicUsize::new(0);
    let outcome = run_with(
        "chrome",
        "read the result",
        &desktop,
        &|_, _, _, _| {
            Ok(if decisions.fetch_add(1, Ordering::SeqCst) == 0 {
                Step::Wait
            } else {
                Step::Fail("No result yet".into())
            })
        },
        &host,
        &Limits::default(),
    );
    assert!(matches!(outcome, Outcome::Failed(_)));
    assert!(
        host.events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.contains("readiness wait limit"))
    );
    assert!(desktop.actions.lock().unwrap().is_empty());
}

#[test]
fn an_immediately_opened_window_has_no_fixed_startup_sleep() {
    struct Appearing {
        opened: AtomicBool,
    }
    impl Host for Appearing {
        fn emit(&self, _: &str, _: &str) {}
        fn stopped(&self) -> bool {
            false
        }
        fn open_app(&self, _: &str) -> Result<(), String> {
            self.opened.store(true, Ordering::SeqCst);
            Ok(())
        }
    }
    impl Desktop for Appearing {
        fn find_window(&self, app: &str) -> Result<Win, String> {
            assert!(self.opened.load(Ordering::SeqCst));
            Ok(Delayed::window(app))
        }
        fn find_window_or_absent(&self, app: &str) -> Result<Option<Win>, String> {
            Ok(self
                .opened
                .load(Ordering::SeqCst)
                .then(|| Delayed::window(app)))
        }
        fn observe(&self, _: &Win) -> Result<Observation, String> {
            Ok(Observation {
                texts: vec!["Ready".into()],
                ..Observation::default()
            })
        }
        fn act(&self, _: &Win, _: &UiAct) -> Result<(), String> {
            panic!("read-only task")
        }
    }
    let desktop = Appearing {
        opened: AtomicBool::new(false),
    };
    let start = Instant::now();
    let outcome = run_with(
        "chrome",
        "read Ready",
        &desktop,
        &|_, _, _, _| {
            Ok(Step::Done {
                evidence: "Ready".into(),
                summary: String::new(),
            })
        },
        &desktop,
        &Limits::default(),
    );
    assert!(matches!(outcome, Outcome::Observed(_)));
    assert!(
        start.elapsed() < Duration::from_millis(400),
        "startup retained its fixed500ms delay"
    );
}

#[test]
fn expiry_during_absence_lookup_cannot_launch_an_app() {
    struct SlowAbsent;
    impl Desktop for SlowAbsent {
        fn find_window(&self, _: &str) -> Result<Win, String> {
            panic!("not used")
        }
        fn find_window_or_absent(&self, _: &str) -> Result<Option<Win>, String> {
            std::thread::sleep(Duration::from_millis(60));
            Ok(None)
        }
        fn observe(&self, _: &Win) -> Result<Observation, String> {
            panic!("expired")
        }
        fn act(&self, _: &Win, _: &UiAct) -> Result<(), String> {
            panic!("expired")
        }
    }
    let outcome = run_with(
        "chrome",
        "read status",
        &SlowAbsent,
        &|_, _, _, _| panic!("expired model request"),
        &Harness::default(),
        &Limits {
            deadline: Duration::from_millis(30),
            ..Limits::default()
        },
    );
    assert!(
        matches!(outcome, Outcome::Failed(ref why) if why.contains("time limit")),
        "{outcome:?}"
    );
}

#[test]
fn navigation_debounce_cannot_send_later_keys_after_the_original_deadline() {
    let desktop = Delayed::new(Duration::ZERO);
    let outcome = run_with(
        "chrome",
        "open wikipedia",
        &desktop,
        &|_, _, _, _| Ok(Step::Navigate("https://www.wikipedia.org".into())),
        &Harness::default(),
        &Limits {
            deadline: Duration::from_millis(80),
            ..Limits::default()
        },
    );
    assert!(
        matches!(outcome, Outcome::Failed(ref why) if why.contains("time limit")),
        "{outcome:?}"
    );
    assert_eq!(
        *desktop.actions.lock().unwrap(),
        vec![UiAct::Key(Key::Address)]
    );
}

#[test]
fn expiry_during_field_focus_prevents_following_text_input() {
    struct SlowFocus(Delayed);
    impl Desktop for SlowFocus {
        fn find_window(&self, app: &str) -> Result<Win, String> {
            self.0.find_window(app)
        }
        fn observe(&self, win: &Win) -> Result<Observation, String> {
            let mut observation = self.0.observe(win)?;
            observation.elements[0].role = "searchbox".into();
            Ok(observation)
        }
        fn act(&self, win: &Win, action: &UiAct) -> Result<(), String> {
            assert!(
                matches!(action, UiAct::Focus(_)),
                "text arrived after expiry"
            );
            self.0.act(win, action)?;
            std::thread::sleep(Duration::from_millis(60));
            Ok(())
        }
    }
    let desktop = SlowFocus(Delayed::new(Duration::ZERO));
    let outcome = run_with(
        "chrome",
        "search blue",
        &desktop,
        &|_, _, _, _| Ok(Step::Type("e1".into(), "blue".into())),
        &Harness::default(),
        &Limits {
            deadline: Duration::from_millis(30),
            ..Limits::default()
        },
    );
    assert!(
        matches!(outcome, Outcome::Failed(ref why) if why.contains("time limit")),
        "{outcome:?}"
    );
    assert_eq!(desktop.0.actions.lock().unwrap().len(), 1);
}

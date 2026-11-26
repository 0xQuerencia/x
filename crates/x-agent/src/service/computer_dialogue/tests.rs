use super::*;
use crate::computer::Step as ComputerStep;
use crate::desktop::{Desktop, Element, Observation, UiAct, Win};
use crate::hexcli::{Stt, SttError};
use crate::service::{Launcher, Mode, PlanOptions, Snapshot};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

struct Speech;
impl Stt for Speech {
    fn start(&self) -> Result<(), SttError> {
        Ok(())
    }
    fn stop_print(&self) -> Result<String, SttError> {
        Err(SttError::NoSpeech)
    }
    fn cancel(&self) -> Result<(), SttError> {
        Ok(())
    }
}

#[derive(Default)]
struct Launches(Mutex<Vec<String>>);
impl Launcher for Launches {
    fn launch(&self, action: &Action) -> Result<String, String> {
        self.0.lock().unwrap().push(action.describe());
        Ok("Launched".into())
    }
}

#[derive(Default)]
struct Screen {
    observations: AtomicUsize,
    clicks: AtomicUsize,
}
impl Desktop for Screen {
    fn find_window(&self, app: &str) -> Result<Win, String> {
        Ok(Win {
            address: "0x1".into(),
            pid: 1,
            class: app.into(),
            title: "Search".into(),
            id: "window".into(),
        })
    }
    fn observe(&self, _: &Win) -> Result<Observation, String> {
        let n = self.observations.fetch_add(1, Ordering::SeqCst);
        Ok(Observation {
            snapshot: format!("s{n}"),
            title: "Search".into(),
            elements: vec![Element {
                id: "e1".into(),
                reference: format!("@s{n}:e1"),
                role: "link".into(),
                name: "Open result".into(),
                value: String::new(),
                actions: vec!["Click".into()],
            }],
            texts: vec![
                if self.clicks.load(Ordering::SeqCst) == 0 {
                    "idle"
                } else {
                    "result loaded"
                }
                .into(),
            ],
        })
    }
    fn act(&self, _: &Win, action: &UiAct) -> Result<(), String> {
        assert!(matches!(action, UiAct::Click(_)));
        self.clicks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct Choices {
    local: bool,
    steps: Mutex<VecDeque<ComputerStep>>,
    goals: Mutex<Vec<String>>,
}
impl Planner for Choices {
    fn is_local(&self) -> bool {
        self.local
    }
    fn name(&self) -> String {
        "fixture choices".into()
    }
    fn decide_computer(
        &self,
        goal: &str,
        app: &str,
        _: &Observation,
        _: &[String],
        _: bool,
    ) -> Option<Result<ComputerStep, String>> {
        assert_eq!(app, "chrome");
        self.goals.lock().unwrap().push(goal.into());
        Some(
            self.steps
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "No decision remaining".into()),
        )
    }
}

fn rig(
    local: bool,
    steps: Vec<ComputerStep>,
) -> (Service, Arc<Screen>, Arc<Launches>, Arc<Choices>) {
    let launches = Arc::new(Launches::default());
    let screen = Arc::new(Screen::default());
    let choices = Arc::new(Choices {
        local,
        steps: Mutex::new(steps.into()),
        goals: Mutex::new(vec![]),
    });
    let service = Service::new(Arc::new(Speech), launches.clone(), None);
    service.set_desktop(screen.clone());
    service.set_planner(Some(choices.clone()));
    service.set_computer_remote(!local);
    (service, screen, launches, choices)
}

fn computer_action() -> Action {
    Action::Computer {
        app: "chrome".into(),
        goal: "open the result".into(),
    }
}

fn plan(service: &Service, actions: Vec<Action>) -> Task {
    service
        .plan_task(
            "open the result",
            Mode::Actions,
            Some(actions),
            PlanOptions::default(),
        )
        .unwrap()
}

fn wait(service: &Service, phase: Phase) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = service.snapshot(0);
        if snapshot.phase == phase {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "timed out: {:?}", snapshot.task);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn approve(service: &Service, task_id: u64) {
    let question_id = service
        .snapshot(0)
        .computer_question
        .filter(|question| question.pending_answer.is_some())
        .map(|question| question.question_id);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let result = match &question_id {
            Some(question_id) => service.approve_computer_answer(task_id, question_id),
            None => service.approve(task_id),
        };
        match result {
            Ok(()) => return,
            Err(error) if error.contains("execution lease") && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("approval failed: {error}"),
        }
    }
}

fn paused(service: &Service) -> ComputerQuestion {
    wait(service, Phase::WaitingForInput)
        .computer_question
        .unwrap()
}

#[test]
fn clarification_resumes_exact_step_without_replaying_earlier_or_losing_later_actions() {
    let (service, screen, launches, choices) = rig(
        true,
        vec![
            ComputerStep::Click("e1".into()),
            ComputerStep::Ask("Which result?".into()),
            ComputerStep::Done {
                evidence: "result loaded".into(),
                summary: String::new(),
            },
        ],
    );
    let task = plan(
        &service,
        vec![
            Action::App {
                target: "terminal".into(),
            },
            computer_action(),
            Action::App {
                target: "firefox".into(),
            },
        ],
    );
    approve(&service, task.id);
    let question = paused(&service);
    assert_eq!(question.task_id, task.id);
    assert_eq!(question.step, 1);
    assert!(question.pending_answer.is_none());
    assert_eq!(launches.0.lock().unwrap().len(), 1);
    assert_eq!(screen.clicks.load(Ordering::SeqCst), 1);
    let answered = service
        .answer_computer(task.id, &question.question_id, "the blue result")
        .unwrap();
    assert_eq!(answered.id, task.id);
    assert_eq!(answered.phase, Phase::AwaitingApproval);
    assert_eq!(answered.steps[1].status, "waiting_review");
    assert!(answered.steps[1].detail.contains("the blue result"));
    assert_eq!(choices.goals.lock().unwrap().len(), 2);
    approve(&service, task.id);
    let done = wait(&service, Phase::Completed);
    assert!(done.computer_question.is_none());
    let task = done.task.unwrap();
    assert_eq!(
        task.steps
            .iter()
            .map(|step| step.status.as_str())
            .collect::<Vec<_>>(),
        vec!["launched", "launched", "launched"]
    );
    assert_eq!(
        task.steps[1].execution.as_ref().unwrap().state,
        crate::tools::EffectState::Observed
    );
    assert_eq!(
        done.action_outcome,
        Some(crate::service::ActionCompletion::Partial)
    );
    assert_eq!(launches.0.lock().unwrap().len(), 2);
    assert_eq!(screen.clicks.load(Ordering::SeqCst), 1);
    assert_eq!(screen.observations.load(Ordering::SeqCst), 3);
    assert!(choices.goals.lock().unwrap()[2].ends_with("the blue result"));
    assert!(
        !done
            .events
            .iter()
            .any(|event| event.text.contains("the blue result"))
    );
}

#[test]
fn answers_require_exact_task_question_and_explicit_review_even_when_autonomous() {
    let (service, screen, _, choices) = rig(true, vec![ComputerStep::Ask("Which result?".into())]);
    service.set_trust_actions(true);
    let task = plan(&service, vec![computer_action()]);
    approve(&service, task.id);
    let question = paused(&service);
    assert!(
        service
            .answer_computer(task.id + 1, &question.question_id, "blue")
            .is_err()
    );
    assert!(
        service
            .answer_computer(task.id, "stale-question", "blue")
            .is_err()
    );
    assert!(
        service
            .answer_computer(task.id, &question.question_id, &"x".repeat(301))
            .is_err()
    );
    assert!(service.approve(task.id).is_err());
    service
        .answer_computer(task.id, &question.question_id, "blue")
        .unwrap();
    assert!(
        service
            .answer_computer(task.id, &question.question_id, "red")
            .is_err()
    );
    service.approve_if_automatic(task.id).unwrap();
    // A delayed initial-task approval cannot authorize the newly supplied detail.
    assert!(service.approve(task.id).is_err());
    assert!(
        service
            .approve_computer_answer(task.id, "stale-question")
            .is_err()
    );
    assert!(
        service
            .approve_computer_answer(task.id + 1, &question.question_id)
            .is_err()
    );
    let snapshot = service.snapshot(0);
    assert_eq!(snapshot.phase, Phase::AwaitingApproval);
    assert_eq!(
        snapshot
            .computer_question
            .unwrap()
            .pending_answer
            .as_deref(),
        Some("blue")
    );
    assert_eq!(choices.goals.lock().unwrap().len(), 1);
    assert_eq!(screen.observations.load(Ordering::SeqCst), 1);
    service.cancel();
}

#[test]
fn stop_discards_both_unanswered_and_reviewed_continuations() {
    for answered in [false, true] {
        let (service, screen, _, _) = rig(true, vec![ComputerStep::Ask("Which result?".into())]);
        let task = plan(&service, vec![computer_action()]);
        approve(&service, task.id);
        let question = paused(&service);
        if answered {
            service
                .answer_computer(task.id, &question.question_id, "blue")
                .unwrap();
        }
        service.cancel();
        let snapshot = service.snapshot(0);
        assert_eq!(snapshot.phase, Phase::Cancelled);
        assert!(snapshot.computer_question.is_none());
        assert!(
            service
                .answer_computer(task.id, &question.question_id, "blue")
                .is_err()
        );
        assert!(service.approve(task.id).is_err());
        assert_eq!(screen.observations.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn revoked_remote_consent_or_changed_model_prevents_any_resumed_observation() {
    for change_model in [false, true] {
        let (service, screen, _, choices) =
            rig(false, vec![ComputerStep::Ask("Which result?".into())]);
        let task = plan(&service, vec![computer_action()]);
        approve(&service, task.id);
        let question = paused(&service);
        service
            .answer_computer(task.id, &question.question_id, "blue")
            .unwrap();
        if change_model {
            service.set_planner(Some(Arc::new(Choices {
                local: true,
                steps: Mutex::new(VecDeque::new()),
                goals: Mutex::new(vec![]),
            })));
        } else {
            service.set_computer_remote(false);
        }
        approve(&service, task.id);
        let failed = wait(&service, Phase::Failed);
        assert!(failed.computer_question.is_none());
        assert_eq!(screen.observations.load(Ordering::SeqCst), 1);
        assert_eq!(choices.goals.lock().unwrap().len(), 1);
    }
}

#[test]
fn revoking_an_app_grant_discards_its_question_and_exact_approval() {
    let (service, screen, _, _) = rig(false, vec![ComputerStep::Ask("Which result?".into())]);
    service.set_computer_apps(vec!["chrome".into(), "firefox".into()]);
    let task = plan(&service, vec![computer_action()]);
    approve(&service, task.id);
    let question = paused(&service);
    service
        .answer_computer(task.id, &question.question_id, "blue")
        .unwrap();
    service.set_computer_apps(vec!["chrome".into()]);
    assert_eq!(service.snapshot(0).phase, Phase::AwaitingApproval);
    service.set_computer_apps(vec![]);
    let cancelled = service.snapshot(0);
    assert_eq!(cancelled.phase, Phase::Cancelled);
    assert!(cancelled.computer_question.is_none());
    service.set_computer_apps(vec!["chrome".into()]);
    assert!(
        service
            .approve_computer_answer(task.id, &question.question_id)
            .is_err()
    );
    assert_eq!(screen.observations.load(Ordering::SeqCst), 1);
    assert_eq!(screen.clicks.load(Ordering::SeqCst), 0);
}

#[test]
fn revoking_an_app_grant_ignores_a_model_decision_already_in_flight() {
    use std::sync::mpsc::{Receiver, Sender, channel};

    struct BlockingDecision {
        arrived: Sender<()>,
        release: Mutex<Receiver<()>>,
    }

    impl Planner for BlockingDecision {
        fn name(&self) -> String {
            "blocking fixture".into()
        }

        fn decide_computer(
            &self,
            _: &str,
            _: &str,
            _: &Observation,
            _: &[String],
            _: bool,
        ) -> Option<Result<ComputerStep, String>> {
            self.arrived.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            Some(Ok(ComputerStep::Click("e1".into())))
        }
    }

    let (service, screen, _, _) = rig(false, vec![]);
    let (arrived, started) = channel();
    let (release, wait_for_release) = channel();
    service.set_planner(Some(Arc::new(BlockingDecision {
        arrived,
        release: Mutex::new(wait_for_release),
    })));
    service.set_computer_apps(vec!["chrome".into()]);
    let task = plan(&service, vec![computer_action()]);
    approve(&service, task.id);
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    service.set_computer_apps(vec![]);
    release.send(()).unwrap();
    let cancelled = wait(&service, Phase::Cancelled);
    assert_eq!(cancelled.task.unwrap().id, task.id);
    assert_eq!(screen.observations.load(Ordering::SeqCst), 1);
    assert_eq!(screen.clicks.load(Ordering::SeqCst), 0);
}

#[test]
fn repeated_questions_get_new_tokens_and_do_not_accept_old_answers() {
    let (service, _, _, _) = rig(
        true,
        vec![
            ComputerStep::Ask("Which result?".into()),
            ComputerStep::Ask("Which blue?".into()),
        ],
    );
    let task = plan(&service, vec![computer_action()]);
    approve(&service, task.id);
    let first = paused(&service);
    service
        .answer_computer(task.id, &first.question_id, "blue")
        .unwrap();
    approve(&service, task.id);
    let second = paused(&service);
    assert_ne!(first.question_id, second.question_id);
    assert_eq!(first.step, second.step);
    assert!(second.expires_in_ms < first.expires_in_ms);
    assert!(
        service
            .answer_computer(task.id, &first.question_id, "red")
            .is_err()
    );
    assert!(
        service
            .answer_computer(task.id, &second.question_id, "dark blue")
            .is_ok()
    );
    assert!(
        service
            .approve_computer_answer(task.id, &first.question_id)
            .is_err()
    );
    assert_eq!(service.snapshot(0).phase, Phase::AwaitingApproval);
    service.cancel();
}

#[test]
fn a_live_execution_lease_blocks_resume_without_consuming_the_answer() {
    let (service, _, _, _) = rig(true, vec![ComputerStep::Ask("Which result?".into())]);
    let task = plan(&service, vec![computer_action()]);
    approve(&service, task.id);
    let question = paused(&service);
    service
        .answer_computer(task.id, &question.question_id, "blue")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let lease = loop {
        let result = service.reserve_work(&service.lock(), task.id);
        if let Ok(lease) = result {
            break lease;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        service
            .approve_computer_answer(task.id, &question.question_id)
            .unwrap_err()
            .contains("execution lease")
    );
    let snapshot = service.snapshot(0);
    assert_eq!(snapshot.phase, Phase::AwaitingApproval);
    assert_eq!(
        snapshot
            .computer_question
            .unwrap()
            .pending_answer
            .as_deref(),
        Some("blue")
    );
    drop(lease);
    service.cancel();
}

#[test]
fn expired_questions_are_removed_from_snapshots_and_cannot_resume() {
    struct Quiet;
    impl computer::Host for Quiet {
        fn emit(&self, _: &str, _: &str) {}
        fn stopped(&self) -> bool {
            false
        }
        fn open_app(&self, _: &str) -> Result<(), String> {
            panic!("must not launch")
        }
    }
    let (service, screen, _, _) = rig(true, vec![]);
    let task = plan(&service, vec![computer_action()]);
    {
        let mut state = service.lock();
        set_phase(&mut state, Phase::Running);
    }
    let id = ActionId {
        task: task.id,
        step: 0,
    };
    assert!(service.claim_action(id).unwrap());
    let outcome = computer::run_with(
        "chrome",
        "open the result",
        screen.as_ref(),
        &|_, _, _, _| Ok(ComputerStep::Ask("Which result?".into())),
        &Quiet,
        &computer::Limits {
            deadline: Duration::from_millis(200),
            ..computer::Limits::default()
        },
    );
    let Outcome::Waiting(continuation) = outcome else {
        panic!("expected question")
    };
    service.pause_computer(id, *continuation).unwrap();
    let question = service.snapshot(0).computer_question.unwrap();
    std::thread::sleep(Duration::from_millis(220));
    assert!(
        service
            .answer_computer(task.id, &question.question_id, "blue")
            .is_err()
    );
    let snapshot = service.snapshot(0);
    assert_eq!(snapshot.phase, Phase::Failed);
    assert!(snapshot.task.unwrap().error.unwrap().contains("expired"));
    assert!(snapshot.computer_question.is_none());
    assert!(service.approve(task.id).is_err());
    assert_eq!(screen.observations.load(Ordering::SeqCst), 1);
}

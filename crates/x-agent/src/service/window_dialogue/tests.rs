use super::*;
use crate::context::WindowRef;
use crate::hexcli::{Stt, SttError};
use crate::wm::{self, Backend, Cap, Cmd, Controller, Op, Place, WinInfo, WindowTarget, WmError};
use std::time::Duration;

#[test]
fn raw_spoken_references_create_review_and_cannot_cross_task_replacement() {
    let (service, launcher, task, _) = rig(Op::Focus);
    assert!(
        service
            .answer_current_window_reference("I saw two windows")
            .is_none()
    );
    let selected = service
        .plan_spoken("Friday actually the second one instead", Mode::Actions)
        .unwrap();
    assert_eq!(selected.id, task.id);
    assert_eq!(selected.phase, Phase::AwaitingApproval);
    assert_eq!(
        service.snapshot(0).window_question.unwrap().selected,
        Some(2)
    );
    assert!(launcher.native.0.lock().unwrap().commands.is_empty());
    let corrected = service.plan_smart("the other one").unwrap();
    assert_eq!(corrected.id, task.id);
    assert_eq!(
        service.snapshot(0).window_question.unwrap().selected,
        Some(1)
    );
    let cancelled = service
        .plan_spoken("cancel this one", Mode::Actions)
        .unwrap();
    assert_eq!(cancelled.id, task.id);
    assert_eq!(cancelled.phase, Phase::Cancelled);
    let replacement = service.plan("open firefox", Mode::Actions).unwrap();
    assert!(service.answer_current_window_reference("two").is_none());
    assert!(
        service
            .answer_current_window_reference("cancel this one")
            .is_none()
    );
    assert_eq!(service.snapshot(0).task.unwrap().id, replacement.id);
    assert!(launcher.native.0.lock().unwrap().commands.is_empty());
}

#[test]
fn window_corrections_refresh_attention_identity_without_approving_input() {
    let (service, launcher, task, question) = rig(Op::Focus);
    let first = service.task_attention().unwrap();
    assert_eq!(
        first.question_token.as_deref(),
        Some(question.question_id.as_str())
    );
    service
        .answer_window(task.id, &question.question_id, "second")
        .unwrap();
    let review = service.task_attention().unwrap();
    assert_ne!(review.question_token, first.question_token);
    assert_eq!(review.phase, Phase::AwaitingApproval);
    assert!(service.approve(task.id).is_err());
    assert!(launcher.native.0.lock().unwrap().commands.is_empty());
}

struct Speech;
impl Stt for Speech {
    fn start(&self) -> Result<(), SttError> {
        unreachable!()
    }
    fn stop_print(&self) -> Result<String, SttError> {
        unreachable!()
    }
    fn cancel(&self) -> Result<(), SttError> {
        Ok(())
    }
}

struct World {
    windows: Vec<WinInfo>,
    focus: Option<String>,
    commands: Vec<Cmd>,
}

#[derive(Clone)]
struct Native(Arc<Mutex<World>>);
impl Backend for Native {
    fn name(&self) -> &'static str {
        "window-reference-fixture"
    }
    fn caps(&self) -> Vec<Cap> {
        vec![
            Cap::List,
            Cap::Focus,
            Cap::Close,
            Cap::MoveToWorkspace,
            Cap::Maximize,
            Cap::Minimize,
        ]
    }
    fn windows(&self) -> Result<Vec<WinInfo>, WmError> {
        Ok(self.0.lock().unwrap().windows.clone())
    }
    fn focused(&self) -> Result<Option<String>, WmError> {
        Ok(self.0.lock().unwrap().focus.clone())
    }
    fn workspace(&self) -> Result<i64, WmError> {
        Ok(1)
    }
    fn run(&self, command: &Cmd) -> Result<(), WmError> {
        let mut world = self.0.lock().unwrap();
        world.commands.push(command.clone());
        match command {
            Cmd::Close(id) => world.windows.retain(|window| &window.id != id),
            Cmd::Focus(id) => world.focus = Some(id.clone()),
            Cmd::Move { win, to } => {
                let window = world
                    .windows
                    .iter_mut()
                    .find(|window| &window.id == win)
                    .unwrap();
                window.place = to.clone();
                if let Place::N(workspace) = to {
                    window.workspace = *workspace;
                }
            }
            Cmd::Maximize { win, on } => {
                world
                    .windows
                    .iter_mut()
                    .find(|window| &window.id == win)
                    .unwrap()
                    .maximized = *on
            }
            Cmd::Minimize { win } => {
                world
                    .windows
                    .iter_mut()
                    .find(|window| &window.id == win)
                    .unwrap()
                    .minimized = true
            }
            _ => unreachable!(),
        }
        Ok(())
    }
}

struct LauncherFixture {
    native: Native,
    controller: Mutex<Controller>,
    after_read: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
impl Launcher for LauncherFixture {
    fn launch(&self, _: &Action) -> Result<String, String> {
        panic!("typed window adapter required")
    }
    fn prepare_window(&self, _: &wm::Request) -> Result<Option<WindowTarget>, String> {
        Ok(Some(WindowTarget::Choose(
            self.native
                .0
                .lock()
                .unwrap()
                .windows
                .iter()
                .filter(|window| window.class == "firefox")
                .map(|window| wm::Choice {
                    id: window.id.clone(),
                    pid: window.pid,
                    class: window.class.clone(),
                    label: window.title.clone(),
                })
                .collect(),
        )))
    }
    fn prepare_window_choice(&self, reference: &WindowRef) -> Result<WindowTarget, String> {
        let target = self
            .controller
            .lock()
            .unwrap()
            .prepare_window_choice(reference)
            .map_err(|error| error.to_string())?;
        if let Some(after_read) = self.after_read.lock().unwrap().take() {
            after_read();
        }
        Ok(target)
    }
    fn execute_prepared(
        &self,
        action: &Action,
        target: Option<&wm::CloseTarget>,
        stop: &Cancellation,
    ) -> Result<tools::ExecutionOutcome, String> {
        stop.check()?;
        let Action::Window {
            op,
            target: request_target,
            ws,
        } = action
        else {
            panic!("no broader actions")
        };
        self.controller
            .lock()
            .unwrap()
            .run_with_target_scoped(
                &wm::Request {
                    op: op.clone(),
                    target: request_target.clone(),
                    ws: ws.clone(),
                },
                target,
                stop,
            )
            .map(tools::ExecutionOutcome::from)
            .map_err(|error| error.to_string())
    }
}

fn rig(op: Op) -> (Service, Arc<LauncherFixture>, Task, WindowQuestion) {
    let window = |id: &str, pid: u32, class: &str| WinInfo {
        id: id.into(),
        pid,
        class: class.into(),
        title: format!("Window {id}"),
        workspace: 1,
        place: Place::N(1),
        minimized: false,
        maximized: false,
        recent: u32::MAX,
    };
    let native = Native(Arc::new(Mutex::new(World {
        windows: vec![
            window("w1", 10, "firefox"),
            window("w2", 20, "firefox"),
            window("editor", 30, "code"),
        ],
        focus: Some("editor".into()),
        commands: vec![],
    })));
    let launcher = Arc::new(LauncherFixture {
        controller: Mutex::new(Controller::new(Box::new(native.clone()))),
        native,
        after_read: Mutex::new(None),
    });
    let service = Service::new(Arc::new(Speech), launcher.clone(), None);
    let task = service
        .plan_task(
            "change the selected window",
            Mode::Actions,
            Some(vec![Action::Window {
                op,
                target: wm::Target {
                    app: "firefox".into(),
                    pick: None,
                },
                ws: Some(wm::Ws::N(3)),
            }]),
            PlanOptions::default(),
        )
        .unwrap();
    let question = service.snapshot(0).window_question.unwrap();
    (service, launcher, task, question)
}

fn wait(service: &Service) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot = service.snapshot(0);
        if !snapshot.phase.is_busy() {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "{:?}", snapshot.phase);
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn displayed_ordinal_survives_reordering_and_requires_its_exact_review_token() {
    let (service, launcher, task, question) = rig(Op::Close);
    assert_eq!(task.phase, Phase::WaitingForInput);
    assert!(service.approve(task.id).is_err());
    assert!(
        service
            .approve_window_answer(task.id, &question.question_id)
            .is_err()
    );
    launcher.native.0.lock().unwrap().windows.swap(0, 1);
    service
        .answer_window(
            task.id,
            &question.question_id,
            "actually the second one instead",
        )
        .unwrap();
    let reviewed = service.snapshot(0).window_question.unwrap();
    assert_ne!(reviewed.question_id, question.question_id);
    assert_eq!(reviewed.selected, Some(2));
    assert!(
        service.approve(task.id).is_err(),
        "delayed initial approval must not approve a selection"
    );
    service.approve_if_automatic(task.id).unwrap();
    assert_eq!(service.snapshot(0).phase, Phase::AwaitingApproval);
    assert!(
        service
            .approve_window_answer(task.id, &question.question_id)
            .is_err()
    );
    assert!(launcher.native.0.lock().unwrap().commands.is_empty());
    service
        .approve_window_answer(task.id, &reviewed.question_id)
        .unwrap();
    assert_eq!(
        wait(&service).action_outcome,
        Some(super::super::ActionCompletion::Confirmed)
    );
    assert_eq!(
        launcher.native.0.lock().unwrap().commands,
        [Cmd::Close("w2".into())]
    );
    assert!(
        service
            .approve_window_answer(task.id, &reviewed.question_id)
            .is_err()
    );
}

#[test]
fn previous_other_and_this_reference_only_explicit_choices_in_this_question() {
    let (service, launcher, task, mut question) = rig(Op::Move);
    assert!(
        service
            .answer_window(task.id, &question.question_id, "previous")
            .is_err()
    );
    assert!(
        service
            .answer_window(task.id, &question.question_id, "the other one")
            .is_err()
    );
    for (answer, expected) in [
        ("first", 1),
        ("the other one", 2),
        ("previous", 1),
        ("this", 1),
    ] {
        service
            .answer_window(task.id, &question.question_id, answer)
            .unwrap();
        let next = service.snapshot(0).window_question.unwrap();
        assert_eq!(next.selected, Some(expected));
        assert_ne!(next.question_id, question.question_id);
        question = next;
    }
    assert!(
        service
            .answer_window(task.id, &question.question_id, "2 then close everything")
            .is_err()
    );
    service
        .approve_window_answer(task.id, &question.question_id)
        .unwrap();
    assert_eq!(wait(&service).phase, Phase::Completed);
    assert_eq!(
        launcher.native.0.lock().unwrap().commands,
        [Cmd::Move {
            win: "w1".into(),
            to: Place::N(3)
        }]
    );
}

#[test]
fn a_recycled_native_id_cannot_answer_or_execute_an_old_choice() {
    let (service, launcher, task, question) = rig(Op::Close);
    launcher.native.0.lock().unwrap().windows[1].pid += 1;
    assert!(
        service
            .answer_window(task.id, &question.question_id, "2")
            .is_err()
    );
    assert_eq!(service.snapshot(0).phase, Phase::WaitingForInput);
    service
        .answer_window(task.id, &question.question_id, "1")
        .unwrap();
    let reviewed = service.snapshot(0).window_question.unwrap();
    launcher.native.0.lock().unwrap().windows[0].pid += 1;
    service
        .approve_window_answer(task.id, &reviewed.question_id)
        .unwrap();
    assert_eq!(wait(&service).phase, Phase::Failed);
    assert!(launcher.native.0.lock().unwrap().commands.is_empty());
}

#[test]
fn stale_task_and_question_tokens_cannot_select_or_cancel_a_replacement() {
    let (service, launcher, task, question) = rig(Op::Close);
    service
        .cancel_window_question(task.id, &question.question_id)
        .unwrap();
    let replacement = service.plan("open firefox", Mode::Actions).unwrap();
    assert_ne!(replacement.id, task.id);
    assert!(
        service
            .answer_window(task.id, &question.question_id, "2")
            .is_err()
    );
    assert!(
        service
            .cancel_window_question(task.id, &question.question_id)
            .is_err()
    );
    assert!(
        service
            .approve_window_answer(task.id, &question.question_id)
            .is_err()
    );
    assert_eq!(service.snapshot(0).task.unwrap().id, replacement.id);
    assert_eq!(service.snapshot(0).phase, Phase::AwaitingApproval);
    assert!(launcher.native.0.lock().unwrap().commands.is_empty());
}

#[test]
fn stop_during_window_revalidation_cannot_recreate_the_question() {
    let (service, launcher, task, question) = rig(Op::Close);
    let concurrent = service.clone();
    *launcher.after_read.lock().unwrap() = Some(Box::new(move || concurrent.cancel_foreground()));
    assert!(
        service
            .answer_window(task.id, &question.question_id, "2")
            .is_err()
    );
    assert_eq!(service.snapshot(0).phase, Phase::Cancelled);
    assert!(service.snapshot(0).window_question.is_none());
    assert!(launcher.native.0.lock().unwrap().commands.is_empty());
}

#[test]
fn expiry_rejects_answers_and_approvals_and_retires_the_question() {
    for answered in [false, true] {
        let (service, launcher, task, question) = rig(Op::Close);
        if answered {
            service
                .answer_window(task.id, &question.question_id, "2")
                .unwrap();
        }
        let current = service.snapshot(0).window_question.unwrap();
        service
            .lock()
            .window_dialogue
            .as_mut()
            .unwrap()
            .references
            .expire();
        assert!(
            service
                .answer_window(task.id, &current.question_id, "1")
                .is_err()
        );
        assert!(
            service
                .approve_window_answer(task.id, &current.question_id)
                .is_err()
        );
        let snapshot = service.snapshot(0);
        assert!(snapshot.window_question.is_none());
        assert_eq!(snapshot.phase, Phase::Failed);
        assert!(launcher.native.0.lock().unwrap().commands.is_empty());
    }
}

#[test]
fn a_reference_that_expires_while_waiting_for_input_lease_sends_no_input() {
    let (service, launcher, task, question) = rig(Op::Close);
    service
        .answer_window(task.id, &question.question_id, "2")
        .unwrap();
    let question = service.snapshot(0).window_question.unwrap();
    let lease = service.try_desktop_control().unwrap();
    service
        .approve_window_answer(task.id, &question.question_id)
        .unwrap();
    service.lock().actions[0].reference_expires = Some(Instant::now() - Duration::from_secs(1));
    drop(lease);
    let snapshot = wait(&service);
    assert_eq!(snapshot.phase, Phase::Failed);
    assert!(snapshot.task.unwrap().error.unwrap().contains("expired"));
    assert!(launcher.native.0.lock().unwrap().commands.is_empty());
}

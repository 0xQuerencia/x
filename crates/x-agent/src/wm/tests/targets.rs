use super::*;
use crate::cancellation::Cancellation;
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn prepared_focus_move_and_previous_bind_exact_reviewed_identities() {
    for phrase in [
        "focus firefox 2",
        "move firefox 2 to workspace 3",
        "maximize firefox 2",
        "minimize firefox 2",
    ] {
        let (mut controller, fake) = setup();
        let request = p(phrase).unwrap();
        let target = controller.prepare_window(&request).unwrap();
        assert_eq!(target.identity().unwrap().id, "0x3");
        fake.0.lock().unwrap().wins.swap(0, 2);
        controller
            .run_with_target_scoped(&request, Some(&target), &Cancellation::default())
            .unwrap();
        assert!(
            fake.0
                .lock()
                .unwrap()
                .log
                .iter()
                .all(|command| match command {
                    Cmd::Focus(id) | Cmd::Close(id) => id == "0x3",
                    Cmd::Move { win, .. } | Cmd::Maximize { win, .. } | Cmd::Minimize { win } =>
                        win == "0x3",
                    Cmd::Workspace(_) => false,
                })
        );
    }
    let (mut controller, fake) = setup();
    controller.view().unwrap();
    fake.0.lock().unwrap().focus = Some("0x2".into());
    let request = p("previous window").unwrap();
    let target = controller.prepare_window(&request).unwrap();
    assert_eq!(target.identity().unwrap().id, "0x1");
    fake.0.lock().unwrap().focus = Some("0x3".into());
    controller.view().unwrap();
    controller
        .run_with_target_scoped(&request, Some(&target), &Cancellation::default())
        .unwrap();
    assert_eq!(fake.0.lock().unwrap().focus.as_deref(), Some("0x1"));
}

struct ReadBoundary {
    inner: Fake,
    armed: Arc<AtomicBool>,
    stop: Cancellation,
    delay: Duration,
}
impl Backend for ReadBoundary {
    fn name(&self) -> &'static str {
        "read-boundary"
    }
    fn caps(&self) -> Vec<Cap> {
        self.inner.caps()
    }
    fn windows(&self) -> Result<Vec<WinInfo>, WmError> {
        let windows = self.inner.windows()?;
        if self.armed.swap(false, Ordering::SeqCst) {
            if self.delay.is_zero() {
                self.stop.request();
            } else {
                std::thread::sleep(self.delay);
            }
        }
        Ok(windows)
    }
    fn focused(&self) -> Result<Option<String>, WmError> {
        self.inner.focused()
    }
    fn workspace(&self) -> Result<i64, WmError> {
        self.inner.workspace()
    }
    fn run(&self, command: &Cmd) -> Result<(), WmError> {
        self.inner.run(command)
    }
}

#[test]
fn stop_or_expiry_during_native_readback_prevents_dispatch() {
    for delay in [Duration::ZERO, Duration::from_millis(60)] {
        let fake = world(vec![win("0x1", "firefox", "Docs", 1)]);
        let stop = Cancellation::default();
        let armed = Arc::new(AtomicBool::new(false));
        let mut controller = Controller::new(Box::new(ReadBoundary {
            inner: fake.clone(),
            armed: armed.clone(),
            stop: stop.clone(),
            delay,
        }))
        .with_catalog(catalog);
        let request = p("close firefox").unwrap();
        let mut target = controller.prepare_window(&request).unwrap();
        if !delay.is_zero() {
            target.bound_until(Instant::now() + Duration::from_millis(20));
        }
        armed.store(true, Ordering::SeqCst);
        assert!(
            controller
                .run_with_target_scoped(&request, Some(&target), &stop)
                .is_err()
        );
        assert!(fake.0.lock().unwrap().log.is_empty());
        assert!(controller.operation_scope.is_none());
    }
}

#[test]
fn preparation_refuses_missing_identity_or_reused_pids_and_has_no_mutations() {
    let (mut controller, fake) = setup();
    let request = p("maximize firefox 2").unwrap();
    let target = controller.prepare_window(&request).unwrap();
    let identity = target.identity().unwrap();
    fake.0.lock().unwrap().wins[2].pid += 1;
    assert!(controller.prepare_window_choice(&identity).is_err());
    assert!(
        controller
            .run_with_target_scoped(&request, Some(&target), &Cancellation::default())
            .is_err()
    );
    fake.0.lock().unwrap().wins[2].pid = 0;
    assert!(controller.prepare_window(&request).is_err());
    assert!(fake.0.lock().unwrap().log.is_empty());
}

#[test]
fn exact_window_helpers_honor_stop_raised_inside_their_own_lookup() {
    for op in [Op::Focus, Op::Move, Op::Close] {
        let window = win("0x1", "firefox", "Docs", 1);
        let fake = world(vec![window.clone()]);
        let stop = Cancellation::default();
        let mut controller = Controller::new(Box::new(ReadBoundary {
            inner: fake.clone(),
            armed: Arc::new(AtomicBool::new(true)),
            stop: stop.clone(),
            delay: Duration::ZERO,
        }));
        let workspace = (op == Op::Move).then_some(3);
        assert!(
            controller
                .run_exact_scoped(op, &window, workspace, &stop)
                .is_err()
        );
        assert!(stop.requested());
        assert!(fake.0.lock().unwrap().log.is_empty());
    }
}

#[test]
fn exact_close_returns_unverified_when_the_window_stays_open() {
    let window = win("0x1", "firefox", "Unsaved document", 1);
    let fake = world(vec![window.clone()]);
    fake.0.lock().unwrap().inert = true;
    let mut controller = Controller::new(Box::new(fake.clone()));
    assert!(matches!(
        controller.run_exact_scoped(Op::Close, &window, None, &Cancellation::default()),
        Err(WmError::Unverified(_))
    ));
    assert_eq!(fake.0.lock().unwrap().log, [Cmd::Close("0x1".into())]);
}

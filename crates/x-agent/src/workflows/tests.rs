use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::engine::{Config, Host, Reversal};
use super::model::{OnFailure, Step, StepAction, Workflow};
use super::*;
use crate::tools::Action;
use crate::wm::{Cap, Place, WinInfo};

fn win(id: &str, class: &str, ws: i64) -> WinInfo {
    WinInfo {
        id: id.into(),
        pid: id.len() as u32 * 1000 + ws as u32,
        class: class.into(),
        title: format!("{class} window"),
        workspace: ws,
        place: Place::N(ws),
        minimized: false,
        maximized: false,
    }
}

fn class_of(app: &str) -> &'static str {
    match app {
        "vscode" => "code",
        "terminal" => "kitty",
        "firefox" => "firefox",
        "files" => "dolphin",
        _ => "other",
    }
}

struct World {
    wins: Vec<WinInfo>,
    /// Windows that appear after this many polls of the window list.
    arriving: Vec<(usize, WinInfo)>,
    focus: Option<String>,
    ws: i64,
    installed: HashSet<&'static str>,
    caps: Vec<Cap>,
    never_appears: HashSet<&'static str>,
    refuse_moves: bool,
    launch_fails: HashSet<&'static str>,
    log: Vec<String>,
    next: u32,
    startup_polls: usize,
}

struct Fake {
    w: Mutex<World>,
    cancel_after_launches: Option<usize>,
    deny_permit: bool,
    launches: AtomicUsize,
    opencode_result: Result<String, String>,
}

impl Fake {
    fn new() -> Self {
        Self {
            w: Mutex::new(World {
                wins: vec![],
                arriving: vec![],
                focus: None,
                ws: 1,
                installed: ["vscode", "terminal", "firefox", "files"].into(),
                caps: vec![
                    Cap::List,
                    Cap::Focus,
                    Cap::SwitchWorkspace,
                    Cap::MoveToWorkspace,
                    Cap::Close,
                ],
                never_appears: HashSet::new(),
                refuse_moves: false,
                launch_fails: HashSet::new(),
                log: vec![],
                next: 0,
                startup_polls: 2,
            }),
            cancel_after_launches: None,
            deny_permit: false,
            launches: AtomicUsize::new(0),
            opencode_result: Ok("OpenCode exited 0".into()),
        }
    }

    fn log(&self) -> Vec<String> {
        self.w.lock().unwrap().log.clone()
    }

    fn windows_now(&self) -> Vec<WinInfo> {
        self.w.lock().unwrap().wins.clone()
    }

    fn app_of(action: &Action) -> Option<&'static str> {
        match action {
            Action::App { target } => ["vscode", "firefox", "terminal", "files"]
                .into_iter()
                .find(|a| a == target),
            Action::Terminal { .. } => Some("terminal"),
            Action::Folder { .. } => Some("files"),
            _ => None,
        }
    }
}

impl Host for Fake {
    fn windows(&self) -> Result<Vec<WinInfo>, String> {
        let mut w = self.w.lock().unwrap();
        let mut ready = vec![];
        w.arriving.retain_mut(|(n, win)| {
            if *n == 0 {
                ready.push(win.clone());
                false
            } else {
                *n -= 1;
                true
            }
        });
        w.wins.extend(ready);
        Ok(w.wins.clone())
    }
    fn focused(&self) -> Result<Option<WinInfo>, String> {
        let w = self.w.lock().unwrap();
        Ok(w.focus
            .as_ref()
            .and_then(|id| w.wins.iter().find(|x| x.id == *id).cloned()))
    }
    fn workspace(&self) -> Result<i64, String> {
        Ok(self.w.lock().unwrap().ws)
    }
    fn launch(&self, action: &Action) -> Result<String, String> {
        self.launches.fetch_add(1, Ordering::SeqCst);
        let mut w = self.w.lock().unwrap();
        w.log.push(action.describe());
        let app = Self::app_of(action);
        if let Some(app) = app {
            if w.launch_fails.contains(app) {
                return Err(format!("could not launch {app}"));
            }
            if !w.never_appears.contains(app) {
                w.next += 1;
                let id = format!("0x{}", w.next);
                let polls = w.startup_polls;
                let ws = w.ws;
                w.arriving.push((polls, win(&id, class_of(app), ws)));
            }
        }
        Ok("ok".into())
    }
    fn installed(&self, action: &Action) -> Result<(), String> {
        let w = self.w.lock().unwrap();
        match Self::app_of(action) {
            Some(app) if !w.installed.contains(app) => {
                Err(format!("{app} is not installed or not on PATH"))
            }
            _ => Ok(()),
        }
    }
    fn can(&self, cap: Cap) -> bool {
        self.w.lock().unwrap().caps.contains(&cap)
    }
    fn focus(&self, window: &WinInfo) -> Result<(), String> {
        let mut w = self.w.lock().unwrap();
        w.log.push(format!("focus {}", window.class));
        w.focus = Some(window.id.clone());
        Ok(())
    }
    fn switch_workspace(&self, n: i64) -> Result<(), String> {
        let mut w = self.w.lock().unwrap();
        w.log.push(format!("workspace {n}"));
        w.ws = n;
        Ok(())
    }
    fn move_window(&self, window: &WinInfo, to: i64) -> Result<(), String> {
        let mut w = self.w.lock().unwrap();
        w.log.push(format!("move {} to {to}", window.class));
        if w.refuse_moves {
            return Err("The move did not take effect".into());
        }
        if let Some(x) = w.wins.iter_mut().find(|x| x.id == window.id) {
            x.workspace = to;
            x.place = Place::N(to);
        }
        Ok(())
    }
    fn close_window(&self, window: &WinInfo) -> Result<(), String> {
        let mut w = self.w.lock().unwrap();
        w.log.push(format!("close {}", window.class));
        w.wins.retain(|x| x.id != window.id);
        Ok(())
    }
    fn opencode_ready(&self) -> Result<(), String> {
        Ok(())
    }
    fn opencode(&self, goal: &str, dir: &Path) -> Result<String, String> {
        self.w
            .lock()
            .unwrap()
            .log
            .push(format!("opencode {goal} in {}", dir.display()));
        self.opencode_result.clone()
    }
    fn permit(&self, _: &StepAction) -> bool {
        !self.deny_permit
    }
    fn cancelled(&self) -> bool {
        self.cancel_after_launches
            .is_some_and(|n| self.launches.load(Ordering::SeqCst) >= n)
    }
}

fn cfg(home: &Path) -> Config {
    Config {
        poll: Duration::from_millis(1),
        home: home.to_path_buf(),
    }
}

fn step(id: &str, action: StepAction) -> Step {
    Step {
        id: id.into(),
        action,
        requires: vec![],
        timeout_secs: Some(1),
        on_failure: OnFailure::Stop,
    }
}

fn open(app: &str, ws: Option<u8>) -> StepAction {
    StepAction::OpenApp {
        app: app.into(),
        workspace: ws,
    }
}

fn coding(home: &Path) -> Workflow {
    std::fs::create_dir_all(home.join("projects/my-project")).unwrap();
    Workflow {
        id: "coding".into(),
        name: "Start coding".into(),
        triggers: vec!["start coding".into(), "coding setup".into()],
        steps: vec![
            step("editor", open("vscode", Some(1))),
            step(
                "terminal",
                StepAction::OpenTerminal {
                    directory: "~/projects/my-project".into(),
                    workspace: Some(1),
                },
            ),
            step("browser", open("firefox", Some(2))),
            step(
                "focus",
                StepAction::FocusWindow {
                    app: "vscode".into(),
                    pick: None,
                },
            ),
        ],
    }
}

fn home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn statuses(o: &Outcome) -> Vec<StepStatus> {
    o.report.steps.iter().map(|s| s.status).collect()
}

mod validation {
    use super::*;

    fn bad(wf: &Workflow, home: &Path) -> String {
        wf.validate(home)
            .unwrap_err()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" | ")
    }

    #[test]
    fn the_example_workflow_is_valid() {
        let h = home();
        coding(h.path()).validate(h.path()).unwrap();
    }

    #[test]
    fn unknown_keys_actions_and_arguments_are_rejected() {
        let parse = |s: &str| serde_json::from_str::<Workflow>(s);
        let ok = r#"{"id":"a","name":"A","steps":[{"id":"s","do":{"action":"open_app","app":"vscode"}}]}"#;
        assert!(parse(ok).is_ok());
        for evil in [
            r#"{"id":"a","name":"A","steps":[{"id":"s","do":{"action":"run_shell","cmd":"rm -rf /"}}]}"#,
            r#"{"id":"a","name":"A","steps":[{"id":"s","do":{"action":"open_app","app":"vscode","args":"--evil"}}]}"#,
            r#"{"id":"a","name":"A","steps":[{"id":"s","do":{"action":"open_app","app":"vscode"},"shell":"x"}]}"#,
            r#"{"id":"a","name":"A","extra":1,"steps":[]}"#,
        ] {
            assert!(parse(evil).is_err(), "{evil}");
        }
    }

    #[test]
    fn arguments_are_checked() {
        let h = home();
        let with = |action| Workflow {
            steps: vec![step("s", action)],
            ..coding(h.path())
        };
        for (action, needle) in [
            (open("firefox; rm -rf /", None), "approved app"),
            (open("rm", None), "approved app"),
            (open("vscode", Some(0)), "1-10"),
            (open("vscode", Some(11)), "1-10"),
            (
                StepAction::OpenTerminal {
                    directory: "~/../etc".into(),
                    workspace: None,
                },
                "..",
            ),
            (
                StepAction::OpenTerminal {
                    directory: "/etc".into(),
                    workspace: None,
                },
                "home folder",
            ),
            (
                StepAction::OpenTerminal {
                    directory: "relative/dir".into(),
                    workspace: None,
                },
                "absolute",
            ),
            (
                StepAction::OpenTerminal {
                    directory: "~/.ssh".into(),
                    workspace: None,
                },
                "credentials",
            ),
            (
                StepAction::OpenTerminal {
                    directory: "~/x/.gnupg/keys".into(),
                    workspace: None,
                },
                "credentials",
            ),
            (
                StepAction::OpenDirectory {
                    directory: "~root".into(),
                },
                "~",
            ),
            (
                StepAction::OpenUrl {
                    url: "http://example.com".into(),
                },
                "HTTPS",
            ),
            (
                StepAction::OpenUrl {
                    url: "https://localhost/x".into(),
                },
                "Local",
            ),
            (
                StepAction::OpenUrl {
                    url: "javascript:alert(1)".into(),
                },
                "HTTPS",
            ),
            (StepAction::SwitchWorkspace { workspace: 99 }, "1-10"),
            (
                StepAction::RunAction {
                    phrase: "delete everything".into(),
                },
                "does not understand",
            ),
            (
                StepAction::RunAction {
                    phrase: "close this window".into(),
                },
                "approval",
            ),
            (
                StepAction::RunAction {
                    phrase: "open firefox and type hello".into(),
                },
                "Typing",
            ),
            (
                StepAction::OpencodeTask {
                    goal: "a\nb".into(),
                    directory: "~/p".into(),
                },
                "one line",
            ),
        ] {
            let msg = bad(&with(action.clone()), h.path());
            assert!(
                msg.to_lowercase().contains(&needle.to_lowercase()),
                "{action:?}: {msg}"
            );
        }
    }

    #[test]
    fn structure_is_checked() {
        let h = home();
        let mut wf = coding(h.path());
        wf.steps[1].id = "editor".into();
        assert!(bad(&wf, h.path()).contains("unique"));
        let mut wf = coding(h.path());
        wf.steps[0].requires = vec!["terminal".into()];
        assert!(
            bad(&wf, h.path()).contains("not an earlier step"),
            "no forward or self references, so no cycles"
        );
        let mut wf = coding(h.path());
        wf.steps = (0..21)
            .map(|i| step(&format!("s{i}"), open("vscode", None)))
            .collect();
        assert!(bad(&wf, h.path()).contains("1-20"));
        let mut wf = coding(h.path());
        wf.steps.clear();
        assert!(bad(&wf, h.path()).contains("1-20"));
        let mut wf = coding(h.path());
        wf.id = "Bad Id!".into();
        assert!(bad(&wf, h.path()).contains("id"));
        let mut wf = coding(h.path());
        wf.steps[0].timeout_secs = Some(100_000);
        assert!(bad(&wf, h.path()).contains("timeout"));
    }

    #[test]
    fn triggers_cannot_shadow_built_in_commands() {
        let h = home();
        for phrase in [
            "open firefox",
            "search youtube for cats",
            "make that shorter",
            "scratch that",
            "search this on github",
            "focus my editor",
        ] {
            let mut wf = coding(h.path());
            wf.triggers = vec![phrase.into()];
            assert!(bad(&wf, h.path()).contains("built-in"), "{phrase}");
        }
        let mut wf = coding(h.path());
        wf.triggers = vec!["start coding".into(), "Start Coding!".into()];
        assert!(bad(&wf, h.path()).contains("twice"));
    }

    #[test]
    fn a_workflow_cannot_run_another_one_or_itself() {
        let h = home();
        let mut wf = coding(h.path());
        wf.triggers = vec!["start coding".into()];
        wf.steps = vec![step(
            "again",
            StepAction::RunAction {
                phrase: "start coding".into(),
            },
        )];
        assert!(
            bad(&wf, h.path()).contains("does not understand"),
            "workflow phrases are not built-in actions"
        );
    }
}

mod running {
    use super::*;

    #[test]
    fn runs_every_step_in_order_and_verifies_them() {
        let h = home();
        let host = Fake::new();
        let wf = coding(h.path());
        let out = run(&wf, &host, &cfg(h.path()));
        assert_eq!(out.report.state, RunState::Completed, "{:#?}", out.report);
        assert!(statuses(&out).iter().all(|s| *s == StepStatus::Done));
        assert!(out.report.summary.contains("all 4 steps done and checked"));
        let wins = host.windows_now();
        let ws = |class: &str| wins.iter().find(|w| w.class == class).unwrap().workspace;
        assert_eq!((ws("code"), ws("kitty"), ws("firefox")), (1, 1, 2));
        let log = host.log();
        let order = |needle: &str| log.iter().position(|l| l.contains(needle)).unwrap();
        assert!(
            order("Open vscode") < order("terminal in")
                && order("terminal in") < order("Open firefox")
        );
        assert_eq!(
            host.w.lock().unwrap().focus.as_deref(),
            wins.iter()
                .find(|w| w.class == "code")
                .map(|w| w.id.as_str())
        );
    }

    #[test]
    fn a_missing_app_stops_everything_before_anything_launches() {
        let h = home();
        let host = Fake::new();
        host.w.lock().unwrap().installed.remove("firefox");
        let out = run(&coding(h.path()), &host, &cfg(h.path()));
        assert_eq!(out.report.state, RunState::Failed);
        assert!(
            host.log().is_empty(),
            "no partial session: {:?}",
            host.log()
        );
        assert!(
            out.report.summary.contains("firefox is not installed"),
            "{}",
            out.report.summary
        );
        assert!(statuses(&out).iter().all(|s| *s == StepStatus::Skipped));
    }

    #[test]
    fn a_missing_folder_is_caught_up_front() {
        let h = home();
        let host = Fake::new();
        let mut wf = coding(h.path());
        wf.steps[1].action = StepAction::OpenTerminal {
            directory: "~/nowhere".into(),
            workspace: None,
        };
        let out = run(&wf, &host, &cfg(h.path()));
        assert!(host.log().is_empty());
        assert!(out.report.summary.contains("does not exist"));
    }

    #[test]
    fn unsupported_window_manager_features_fail_before_starting() {
        let h = home();
        let host = Fake::new();
        host.w.lock().unwrap().caps = vec![Cap::List];
        let out = run(&coding(h.path()), &host, &cfg(h.path()));
        assert_eq!(out.report.state, RunState::Failed);
        assert!(host.log().is_empty());
        assert!(
            out.report.summary.contains("cannot"),
            "{}",
            out.report.summary
        );
    }

    #[test]
    fn a_window_that_never_appears_times_out_and_later_steps_do_not_run() {
        let h = home();
        let host = Fake::new();
        host.w.lock().unwrap().never_appears.insert("terminal");
        let mut wf = coding(h.path());
        wf.steps[1].timeout_secs = Some(1);
        let started = std::time::Instant::now();
        let out = run(&wf, &host, &cfg(h.path()));
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(out.report.state, RunState::Failed);
        use StepStatus::*;
        assert_eq!(statuses(&out), vec![Done, Failed, Skipped, Skipped]);
        assert!(
            out.report.steps[1].detail.contains("no kitty")
                || out.report.steps[1]
                    .detail
                    .contains("no terminal window appeared"),
            "{}",
            out.report.steps[1].detail
        );
        assert!(out.report.summary.contains("failed at \"terminal\""));
        assert!(
            out.report.summary.contains("1 done"),
            "{}",
            out.report.summary
        );
        assert!(
            !out.report.summary.contains("all"),
            "never claims success: {}",
            out.report.summary
        );
    }

    #[test]
    fn a_refused_move_is_a_failure_not_a_success() {
        let h = home();
        let host = Fake::new();
        host.w.lock().unwrap().refuse_moves = true;
        let mut wf = coding(h.path());
        wf.steps[0].action = open("vscode", Some(2)); // opens on workspace 1, so it must move
        let out = run(&wf, &host, &cfg(h.path()));
        assert_eq!(out.report.state, RunState::Failed);
        assert_eq!(out.report.steps[0].status, StepStatus::Failed);
        assert!(out.report.steps[0].detail.contains("could not place"));
    }

    #[test]
    fn stop_ends_the_run_at_a_safe_boundary() {
        let h = home();
        let mut host = Fake::new();
        host.cancel_after_launches = Some(1);
        host.w.lock().unwrap().startup_polls = 0; // the first window is there at once
        let out = run(&coding(h.path()), &host, &cfg(h.path()));
        assert_eq!(out.report.state, RunState::Cancelled);
        use StepStatus::*;
        assert_eq!(statuses(&out), vec![Done, Skipped, Skipped, Skipped]);
        assert_eq!(host.launches.load(Ordering::SeqCst), 1);
        assert!(out.report.summary.contains("stopped after 1 of 4"));
    }

    #[test]
    fn stop_while_waiting_for_a_window_is_honoured() {
        let h = home();
        let mut host = Fake::new();
        host.w.lock().unwrap().never_appears.insert("vscode");
        host.cancel_after_launches = Some(1);
        let mut wf = coding(h.path());
        wf.steps[0].timeout_secs = Some(60);
        let started = std::time::Instant::now();
        let out = run(&wf, &host, &cfg(h.path()));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "did not wait out the timeout"
        );
        assert_eq!(out.report.state, RunState::Cancelled);
        assert_eq!(out.report.steps[0].status, StepStatus::Cancelled);
    }

    #[test]
    fn running_again_reuses_windows_instead_of_piling_up() {
        let h = home();
        let host = Fake::new();
        let wf = coding(h.path());
        run(&wf, &host, &cfg(h.path()));
        let first = host.windows_now().len();
        let again = run(&wf, &host, &cfg(h.path()));
        assert_eq!(again.report.state, RunState::Completed);
        let apps_launched = host
            .log()
            .iter()
            .filter(|l| l.starts_with("Open vscode") || l.starts_with("Open firefox"))
            .count();
        assert_eq!(
            apps_launched,
            2,
            "apps already open are reused: {:?}",
            host.log()
        );
        assert_eq!(
            host.windows_now().len(),
            first + 1,
            "only the terminal, which has no identity to reuse, is new"
        );
        assert!(again.report.steps[0].detail.contains("Already open"));
    }

    #[test]
    fn a_failed_launch_is_never_retried_blindly() {
        let h = home();
        let host = Fake::new();
        host.w.lock().unwrap().launch_fails.insert("terminal");
        let out = run(&coding(h.path()), &host, &cfg(h.path()));
        let attempts = host
            .log()
            .iter()
            .filter(|l| l.contains("terminal in"))
            .count();
        assert_eq!(attempts, 1);
        assert_eq!(out.report.steps[1].status, StepStatus::Failed);
    }

    #[test]
    fn continue_on_failure_runs_independent_steps_but_still_reports_failure() {
        let h = home();
        let host = Fake::new();
        host.w.lock().unwrap().launch_fails.insert("terminal");
        let mut wf = coding(h.path());
        wf.steps[1].on_failure = OnFailure::Continue;
        wf.steps[3].requires = vec!["terminal".into()];
        let out = run(&wf, &host, &cfg(h.path()));
        use StepStatus::*;
        assert_eq!(
            statuses(&out),
            vec![Done, Failed, Done, Skipped],
            "the browser still opened; the dependent step did not run"
        );
        assert_eq!(out.report.state, RunState::Failed);
        assert!(
            out.report.steps[3]
                .detail
                .contains("needs step \"terminal\"")
        );
    }

    #[test]
    fn opening_a_url_is_reported_as_unverified() {
        let h = home();
        let host = Fake::new();
        let mut wf = coding(h.path());
        wf.steps = vec![step(
            "docs",
            StepAction::OpenUrl {
                url: "https://docs.rs".into(),
            },
        )];
        let out = run(&wf, &host, &cfg(h.path()));
        assert_eq!(out.report.state, RunState::Completed);
        assert_eq!(out.report.steps[0].status, StepStatus::Unverified);
        assert!(out.report.summary.contains("could not be verified"));
        assert!(!out.report.summary.contains("all 1 steps"));
    }

    #[test]
    fn denied_permission_stops_an_opencode_step() {
        let h = home();
        let mut host = Fake::new();
        host.deny_permit = true;
        let mut wf = coding(h.path());
        wf.steps = vec![
            step("open", open("vscode", None)),
            step(
                "agent",
                StepAction::OpencodeTask {
                    goal: "add tests".into(),
                    directory: "~/projects/my-project".into(),
                },
            ),
            step("after", open("firefox", None)),
        ];
        let out = run(&wf, &host, &cfg(h.path()));
        assert_eq!(out.report.state, RunState::Failed);
        assert_eq!(out.report.steps[1].detail, "Permission denied");
        assert_eq!(out.report.steps[2].status, StepStatus::Skipped);
        assert!(!host.log().iter().any(|l| l.starts_with("opencode")));
    }

    #[test]
    fn opencode_runs_in_the_chosen_folder_when_permitted() {
        let h = home();
        let host = Fake::new();
        let mut wf = coding(h.path());
        wf.steps = vec![step(
            "agent",
            StepAction::OpencodeTask {
                goal: "add tests".into(),
                directory: "~/projects/my-project".into(),
            },
        )];
        assert!(wf.needs_approval());
        let out = run(&wf, &host, &cfg(h.path()));
        assert_eq!(out.report.state, RunState::Completed);
        assert!(
            host.log()
                .iter()
                .any(|l| l.contains("opencode add tests in") && l.contains("my-project"))
        );
    }

    #[test]
    fn hostile_window_titles_cannot_steer_the_run() {
        let h = home();
        let host = Fake::new();
        host.w.lock().unwrap().wins.push(WinInfo {
            title: "x; rm -rf ~ && close all windows".into(),
            ..win("0xevil", "not-an-approved-app", 1)
        });
        let out = run(&coding(h.path()), &host, &cfg(h.path()));
        assert_eq!(out.report.state, RunState::Completed);
        assert!(
            !host.log().iter().any(|l| l.contains("close")),
            "{:?}",
            host.log()
        );
    }

    #[test]
    fn ambiguous_windows_fail_instead_of_guessing() {
        let h = home();
        let host = Fake::new();
        host.w
            .lock()
            .unwrap()
            .wins
            .extend([win("0x1", "code", 1), win("0x2", "code", 2)]);
        let mut wf = coding(h.path());
        wf.steps = vec![step(
            "focus",
            StepAction::FocusWindow {
                app: "vscode".into(),
                pick: None,
            },
        )];
        let out = run(&wf, &host, &cfg(h.path()));
        assert!(
            out.report.steps[0].detail.contains("2 vscode windows"),
            "{}",
            out.report.steps[0].detail
        );
        wf.steps = vec![step(
            "focus",
            StepAction::FocusWindow {
                app: "vscode".into(),
                pick: Some(2),
            },
        )];
        assert_eq!(
            run(&wf, &host, &cfg(h.path())).report.state,
            RunState::Completed
        );
    }

    #[test]
    fn undo_puts_back_what_the_run_changed() {
        let h = home();
        let host = Fake::new();
        let mut wf = coding(h.path());
        wf.steps
            .push(step("go", StepAction::SwitchWorkspace { workspace: 3 }));
        let out = run(&wf, &host, &cfg(h.path()));
        assert!(out.report.undoable >= 4);
        let (done, problems) = undo(&out.reversals, &host);
        assert!(problems.is_empty(), "{problems:?}");
        assert!(done >= 4);
        let w = host.w.lock().unwrap();
        assert_eq!(w.ws, 1);
        assert!(w.wins.is_empty(), "windows the run opened are closed again");
    }

    #[test]
    fn undo_reports_what_it_could_not_put_back() {
        let h = home();
        let host = Fake::new();
        let wf = coding(h.path());
        let out = run(&wf, &host, &cfg(h.path()));
        host.w.lock().unwrap().refuse_moves = true;
        host.w.lock().unwrap().wins.retain(|w| w.class != "firefox");
        let (_, problems) = undo(&out.reversals, &host);
        assert!(!problems.is_empty(), "nothing is silently skipped");
        assert!(
            out.reversals
                .iter()
                .any(|r| matches!(r, Reversal::CloseWindow(_)))
        );
    }

    #[test]
    fn state_machine_only_moves_forward() {
        use RunState::*;
        assert!(Idle.can_move_to(Preparing) && Preparing.can_move_to(Running));
        assert!(Running.can_move_to(Waiting) && Waiting.can_move_to(Running));
        for done in [Completed, Failed, Cancelled] {
            assert!(done.is_final());
            for next in [
                Idle,
                Preparing,
                Running,
                Waiting,
                NeedsPermission,
                Completed,
            ] {
                assert!(!done.can_move_to(next), "{done:?} -> {next:?}");
            }
        }
        assert!(!Idle.can_move_to(Running));
    }
}

mod storage {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn store(h: &Path) -> Store {
        Store::new(h.join("data/workflows"), h.to_path_buf())
    }

    #[test]
    fn saves_privately_and_round_trips() {
        let h = home();
        let s = store(h.path());
        let wf = coding(h.path());
        s.save(&wf).unwrap();
        assert_eq!(s.get("coding").unwrap(), wf);
        let file = h.path().join("data/workflows/coding.json");
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(file.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let json = std::fs::read_to_string(file).unwrap();
        assert!(!json.contains("password") && !json.contains("token"));
    }

    #[test]
    fn invalid_workflows_are_not_saved() {
        let h = home();
        let s = store(h.path());
        let mut wf = coding(h.path());
        wf.steps[0].action = open("rm", None);
        assert!(s.save(&wf).is_err());
        assert!(s.list().is_empty());
    }

    #[test]
    fn trigger_collisions_are_refused_on_save() {
        let h = home();
        let s = store(h.path());
        s.save(&coding(h.path())).unwrap();
        let mut other = coding(h.path());
        other.id = "study".into();
        other.name = "Study".into();
        other.triggers = vec!["Coding setup!".into()];
        let err = s.save(&other).unwrap_err();
        assert!(err[0].message.contains("already belongs to"), "{err:?}");
        other.triggers = vec!["open my study setup".into()];
        s.save(&other).unwrap();
    }

    #[test]
    fn finds_by_trigger_or_by_name() {
        let h = home();
        let s = store(h.path());
        s.save(&coding(h.path())).unwrap();
        for said in [
            "start coding",
            "Start coding.",
            "coding setup",
            "run the start coding workflow",
            "run my start coding",
        ] {
            assert!(
                matches!(s.find(said), Match::One(w) if w.id == "coding"),
                "{said}"
            );
        }
        for said in ["start coding now please", "coding", "open firefox", ""] {
            assert!(matches!(s.find(said), Match::None), "{said}");
        }
    }

    #[test]
    fn hand_edited_collisions_run_nothing_and_say_why() {
        let h = home();
        let s = store(h.path());
        s.save(&coding(h.path())).unwrap();
        let mut other = coding(h.path());
        other.id = "dup".into();
        other.name = "Duplicate".into();
        std::fs::write(
            h.path().join("data/workflows/dup.json"),
            serde_json::to_vec(&other).unwrap(),
        )
        .unwrap();
        match s.find("start coding") {
            Match::Conflict { names, .. } => assert_eq!(names.len(), 2),
            other => panic!("{other:?}"),
        }
        assert!(s.list().iter().all(|e| !e.conflicts.is_empty()));
    }

    #[test]
    fn broken_files_are_reported_not_run() {
        let h = home();
        let s = store(h.path());
        s.save(&coding(h.path())).unwrap();
        let mut bad = coding(h.path());
        bad.steps[0].action = open("vscode", Some(42));
        std::fs::write(
            h.path().join("data/workflows/coding.json"),
            serde_json::to_vec(&bad).unwrap(),
        )
        .unwrap();
        assert!(matches!(s.find("start coding"), Match::Broken { .. }));
        std::fs::write(h.path().join("data/workflows/junk.json"), b"{ not json").unwrap();
        assert_eq!(s.list().len(), 1, "unparseable files are skipped");
    }

    #[test]
    fn symlinks_and_oversized_files_are_ignored() {
        let h = home();
        let s = store(h.path());
        s.save(&coding(h.path())).unwrap();
        let dir = h.path().join("data/workflows");
        std::os::unix::fs::symlink("/etc/passwd", dir.join("link.json")).unwrap();
        std::fs::write(dir.join("huge.json"), vec![b' '; 100_000]).unwrap();
        assert_eq!(s.list().len(), 1);
        assert!(s.get("link").is_err());
    }

    #[test]
    fn duplicate_delete_and_import() {
        let h = home();
        let s = store(h.path());
        s.save(&coding(h.path())).unwrap();
        let copy = s.duplicate("coding").unwrap();
        assert_eq!(
            (copy.id.as_str(), copy.name.as_str()),
            ("coding-copy", "Start coding (copy)")
        );
        assert!(
            copy.triggers.is_empty(),
            "a copy never steals the original's phrases"
        );
        assert_eq!(s.duplicate("coding").unwrap().id, "coding-copy-2");
        s.delete("coding-copy").unwrap();
        assert!(s.get("coding-copy").is_err());
        let file = h.path().join("mine.json");
        let mut mine = coding(h.path());
        mine.id = "mine".into();
        mine.name = "Mine".into();
        mine.triggers = vec!["my own setup".into()];
        std::fs::write(&file, serde_json::to_vec(&mine).unwrap()).unwrap();
        assert_eq!(s.import(&file).unwrap().id, "mine");
        std::fs::write(
            &file,
            br#"{"id":"x","name":"x","steps":[{"id":"s","do":{"action":"shell","cmd":"id"}}]}"#,
        )
        .unwrap();
        assert!(s.import(&file).is_err());
        assert!(s.get("../etc/passwd").is_err());
    }

    #[test]
    fn remembers_the_last_result() {
        let h = home();
        let s = store(h.path());
        let host = Fake::new();
        let wf = coding(h.path());
        s.save(&wf).unwrap();
        let out = run(&wf, &host, &cfg(h.path()));
        s.record_last("coding", &out.report);
        let last = s.last("coding").unwrap();
        assert_eq!(last.state, RunState::Completed);
        assert_eq!(last.steps.len(), 4);
        assert_eq!(
            s.list().len(),
            1,
            "the result file is not mistaken for a workflow"
        );
        s.delete("coding").unwrap();
        assert!(s.last("coding").is_none());
    }
}

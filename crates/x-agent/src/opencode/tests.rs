use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;
use std::thread;
use std::time::Instant;

struct Fixture {
    dir: tempfile::TempDir,
    repo: PathBuf,
    events: Arc<Mutex<Vec<(String, String)>>>,
}

fn sh(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for a in [
        &["init", "-q"][..],
        &["config", "user.email", "t@t"],
        &["config", "user.name", "t"],
    ] {
        sh(&repo, a);
    }
    std::fs::write(repo.join("a.py"), "x=1\n").unwrap();
    sh(&repo, &["add", "."]);
    sh(&repo, &["commit", "-qm", "i"]);
    Fixture {
        dir,
        repo,
        events: Arc::default(),
    }
}

impl Fixture {
    fn fake(&self, body: &str) -> PathBuf {
        let path = self.dir.path().join("fake-opencode");
        let script = format!(
            "#!/bin/sh\nif [ \"$2\" = \"--help\" ]; then printf '%s\\n' '--standalone' '--dir' '--session' '--format json'; exit 0; fi\n{body}\n"
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn start(&self, body: &str, stall: Duration) -> Run {
        let ev = self.events.clone();
        let mut cfg = RunConfig::new(self.fake(body), "p", self.repo.clone());
        cfg.stall_after = stall;
        Run::start(
            cfg,
            Arc::new(move |k, t| ev.lock().unwrap().push((k.into(), t.into()))),
        )
        .unwrap()
    }

    fn run(&self, body: &str) -> RunResult {
        self.start(body, STALL_AFTER).join()
    }
}

#[test]
fn success_needs_exit_zero_and_reports_changed_files() {
    let f = fixture();
    let r = f.run("echo 'x=2' > a.py\necho '{\"type\":\"tool_use\",\"part\":{\"tool\":\"edit\",\"state\":{\"status\":\"completed\",\"title\":\"a.py\"}}}'");
    assert_eq!(r.status, RunStatus::Completed);
    assert_eq!(r.changed_files, Some(vec!["a.py".to_string()]));
    assert!(
        f.events
            .lock()
            .unwrap()
            .contains(&("tool".into(), "edit completed a.py".into()))
    );
}

#[test]
fn error_event_fails_the_run_and_ends_a_hanging_process() {
    let f = fixture();
    let t = Instant::now();
    let r = f.run("echo '{\"type\":\"error\",\"error\":{\"message\":\"boom\"}}'\nsleep 60");
    assert!(t.elapsed() < Duration::from_secs(15));
    assert_eq!(
        (r.status, r.errors),
        (RunStatus::Failed, vec!["boom".to_string()])
    );
}

#[test]
fn nonzero_exit_fails() {
    let r = fixture().run("exit 3");
    assert_eq!((r.status, r.exit_code), (RunStatus::Failed, Some(3)));
}

#[test]
fn stop_kills_a_silent_process_and_its_children() {
    let f = fixture();
    let pidfile = f.dir.path().join("child.pid");
    let run = f.start(
        &format!("sleep 60 &\necho $! > {}\nsleep 60", pidfile.display()),
        STALL_AFTER,
    );
    for _ in 0..50 {
        if std::fs::read_to_string(&pidfile).is_ok_and(|s| !s.trim().is_empty()) {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    run.cancel();
    assert_eq!(run.join().status, RunStatus::Cancelled);
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    thread::sleep(Duration::from_millis(300));
    // SAFETY: signal 0 only probes for existence.
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "child {pid} survived Stop"
    );
}

#[test]
fn stop_reports_partial_changes() {
    let f = fixture();
    let run = f.start("echo y > b.py\necho '{}'\nsleep 60", STALL_AFTER);
    for _ in 0..100 {
        if f.repo.join("b.py").exists() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    run.cancel();
    let result = run.join();
    assert_eq!(result.status, RunStatus::Cancelled);
    assert_eq!(result.changed_files, Some(vec!["b.py".to_string()]));
}

#[test]
fn stall_is_surfaced_not_killed() {
    let f = fixture();
    let r = f.start("sleep 2", Duration::from_millis(500)).join();
    assert_eq!(r.status, RunStatus::Completed);
    assert!(f.events.lock().unwrap().iter().any(|(k, _)| k == "stall"));
}

#[test]
fn non_json_lines_are_kept_raw() {
    let f = fixture();
    f.run("echo hello");
    assert!(
        f.events
            .lock()
            .unwrap()
            .contains(&("raw".into(), "hello".into()))
    );
}

#[test]
fn the_child_is_pinned_to_the_workspace() {
    let f = fixture();
    let args = RunConfig::new(f.fake(""), "go", f.repo.clone()).args();
    let at = args.iter().position(|a| a == "--dir").unwrap();
    assert_eq!(args[at + 1], f.repo.to_string_lossy());
    f.run("echo \"pwd=$PWD\"");
    let want = ("raw".to_string(), format!("pwd={}", f.repo.display()));
    assert!(f.events.lock().unwrap().contains(&want));
}

#[test]
fn option_looking_prompts_stay_after_the_separator() {
    let f = fixture();
    let args = RunConfig::new(f.fake(""), "--auto", f.repo.clone()).args();
    let n = args.len();
    assert_eq!(&args[n - 2..], ["--", "--auto"]);
}

#[test]
fn broad_or_secret_workspaces_are_refused() {
    for p in ["/", "/etc", "/usr", "/home"] {
        assert!(validate_workspace(Path::new(p)).is_err(), "{p}");
    }
    let home = dirs::home_dir().unwrap();
    assert!(validate_workspace(&home).is_err());
    assert!(validate_workspace(&home.join(".config")).is_err());
}

#[test]
fn never_passes_auto_and_prompt_is_one_argument() {
    let f = fixture();
    let args = RunConfig::new(f.fake(""), "fix it; rm -rf ~ --auto", f.repo.clone()).args();
    assert!(!args.iter().any(|a| a == "--auto"));
    assert_eq!(args.last().unwrap(), "fix it; rm -rf ~ --auto");
}

#[test]
fn workspace_validation() {
    let f = fixture();
    assert!(validate_workspace(Path::new("/")).is_err());
    assert!(validate_workspace(&dirs::home_dir().unwrap()).is_err());
    assert!(validate_workspace(&f.dir.path().join("missing")).is_err());
    assert!(validate_workspace(&f.repo.join("a.py")).is_err());
    assert_eq!(
        validate_workspace(&f.repo).unwrap(),
        f.repo.canonicalize().unwrap()
    );
}

#[test]
fn non_git_workspace_has_unknown_changes() {
    let f = fixture();
    let plain = f.dir.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let cfg = RunConfig::new(f.fake(""), "p", plain);
    assert_eq!(
        Run::start(cfg, Arc::new(|_, _| {}))
            .unwrap()
            .join()
            .changed_files,
        None
    );
}

#[test]
fn a_task_stop_reaps_only_its_own_open_code_run() {
    let owned_fixture = fixture();
    let external_fixture = fixture();
    let root = Cancellation::default();
    let owned = Run::start_scoped(
        RunConfig::new(
            owned_fixture.fake("sleep 60"),
            "p",
            owned_fixture.repo.clone(),
        ),
        Arc::new(|_, _| {}),
        &root,
    )
    .unwrap();
    let external = external_fixture.start("sleep 1", STALL_AFTER);
    let started = Instant::now();
    root.request();
    assert_eq!(owned.join().status, RunStatus::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(external.join().status, RunStatus::Completed);
    let config = RunConfig::new(
        owned_fixture.fake("touch started-after-stop"),
        "p",
        owned_fixture.repo.clone(),
    );
    assert!(Run::start_scoped(config, Arc::new(|_, _| {}), &root).is_err());
    assert!(!owned_fixture.repo.join("started-after-stop").exists());
}

#[test]
fn new_cli_without_dir_flag_still_runs_in_the_approved_workspace() {
    let f = fixture();
    let exe = f.fake("");
    std::fs::write(
        &exe,
        concat!(
            "#!/bin/sh\n",
            "if [ \"$2\" = '--help' ]; then printf '%s\\n' '--standalone' '--format json'; exit 0; fi\n",
            "for arg in \"$@\"; do if [ \"$arg\" = '--dir' ]; then exit 41; fi; done\n",
            "printf 'ran here' > reached\n",
        ),
    ).unwrap();
    let config = RunConfig::new(exe, "inspect the project", f.repo.clone());
    assert!(!config.args().contains(&"--dir".to_string()));
    let run = Run::start(config, Arc::new(|_, _| {})).unwrap().join();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(
        std::fs::read_to_string(f.repo.join("reached")).unwrap(),
        "ran here"
    );
}

#[test]
fn named_runs_require_a_matching_structured_acknowledgement() {
    for (body, status) in [
        (
            r#"echo '{"type":"step_start","sessionID":"ses_exact","part":{"type":"step-start"}}'"#,
            RunStatus::Completed,
        ),
        (
            r#"echo '{"type":"text","sessionID":"ses_other","part":{"text":"wrong session"}}'"#,
            RunStatus::Failed,
        ),
        ("echo 'I am in ses_exact'", RunStatus::Failed),
    ] {
        let f = fixture();
        let id = SessionId::try_from("ses_exact".to_string()).unwrap();
        let mut config = RunConfig::new(f.fake(body), "inspect the project", f.repo.clone());
        config.session = Some(id.clone());
        let args = config.args();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--session", id.as_str()])
        );
        assert!(args.iter().any(|arg| arg == "--standalone"));
        assert!(!args.iter().any(|arg| arg == "--auto"));
        let result = Run::start(config, Arc::new(|_, _| {})).unwrap().join();
        assert_eq!(result.status, status);
        assert_eq!(
            result.accepted_session,
            (status == RunStatus::Completed).then_some(id)
        );
    }
}

#[test]
fn named_delivery_does_not_fall_back_when_private_session_run_is_unsupported() {
    let f = fixture();
    let exe = f.fake("");
    std::fs::write(&exe, "#!/bin/sh\nif [ \"$2\" = '--help' ]; then printf '%s\\n' '--format json'; exit 0; fi\ntouch started\n").unwrap();
    let mut cfg = RunConfig::new(exe, "inspect the project", f.repo.clone());
    cfg.session = Some(SessionId::try_from("ses_exact".to_string()).unwrap());
    assert!(Run::start(cfg, Arc::new(|_, _| {})).is_err());
    assert!(!f.repo.join("started").exists());
}

#[test]
fn dropping_a_run_stops_its_owned_process() {
    let f = fixture();
    let run = f.start("echo $$ > owner.pid\nsleep 60", STALL_AFTER);
    let pidfile = f.repo.join("owner.pid");
    for _ in 0..100 {
        if std::fs::read_to_string(&pidfile).is_ok_and(|s| !s.trim().is_empty()) {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let pid: i32 = std::fs::read_to_string(pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    drop(run);
    // SAFETY: signal 0 only checks process existence.
    assert_ne!(unsafe { libc::kill(pid, 0) }, 0);
}

#[test]
fn forced_exit_after_output_closes_is_not_completion() {
    let f = fixture();
    let result = f.run("trap 'exit 0' TERM\nexec 1>&- 2>&-\nwhile :; do sleep 1; done");
    assert_eq!(result.status, RunStatus::Failed);
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("did not exit"))
    );
}

#[test]
fn fresh_runs_cannot_switch_acknowledged_session_identity() {
    let f = fixture();
    let result = f.run(concat!(
        "echo '{\"type\":\"step_start\",\"sessionID\":\"ses_first\",\"part\":{\"type\":\"step-start\"}}'\n",
        "echo '{\"type\":\"text\",\"sessionID\":\"ses_other\",\"part\":{\"text\":\"unrelated output\"}}'\n",
    ));
    assert_eq!(result.status, RunStatus::Failed);
    assert_eq!(result.accepted_session.unwrap().as_str(), "ses_first");
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("different session"))
    );
}

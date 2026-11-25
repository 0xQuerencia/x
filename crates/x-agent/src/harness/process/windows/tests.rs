//! Executed on the Windows CI host, using real immediately-spawned descendants.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, mpsc};

use windows_sys::Win32::Foundation::{ERROR_INVALID_PARAMETER, FILETIME, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

use super::super::{Run, Spec};
use super::*;
use crate::harness::{RunResult, RunStatus, SessionId};

const HELPER: &str = "harness::process::windows::tests::owned_job_fixture_process";
const ROLE: &str = "X_OWNED_JOB_FIXTURE_ROLE";
const DIRECTORY: &str = "X_OWNED_JOB_FIXTURE_DIRECTORY";

fn fixture_command(role: &str, directory: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", HELPER, "--nocapture"])
        .env(ROLE, role)
        .env(DIRECTORY, directory)
        .stdin(Stdio::null());
    command
}

/// The parent test invokes exactly this test in a separate executable process.
/// It returns immediately during the normal suite and never uses a shell.
#[test]
fn owned_job_fixture_process() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let directory = PathBuf::from(std::env::var_os(DIRECTORY).unwrap());
    // An owned descendant may use a nested Job of its own. Parent termination
    // and accounting must still cover that child and its immediate descendant.
    let _nested = (role == "descendant").then(|| {
        let job = Job::new().unwrap();
        // SAFETY: this fixture assigns only its current process to its own Job.
        checked(
            unsafe { AssignProcessToJobObject(job.raw(), GetCurrentProcess()) },
            "nested Job",
        )
        .unwrap();
        job
    });
    let child_role = match role.as_str() {
        "parent" => Some("descendant"),
        "descendant" | "leftover" => Some("grandchild"),
        _ => None,
    };
    if let Some(child_role) = child_role {
        let mut command = fixture_command(child_role, &directory);
        if role == "leftover" {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
        // Spawn immediately: a post-spawn parent assignment would be racy here.
        drop(command.spawn().unwrap());
    }
    std::fs::write(
        directory.join(format!("{role}.pid")),
        std::process::id().to_string(),
    )
    .unwrap();
    println!("owned_ack");
    if matches!(role.as_str(), "exit" | "leftover") {
        return;
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        std::fs::write(
            directory.join(format!("{role}.heartbeat")),
            start.elapsed().as_nanos().to_string(),
        )
        .unwrap();
        thread::sleep(Duration::from_millis(20));
    }
}

fn start_run(role: &str, directory: &Path, events: EventSink) -> Run {
    Run::start(
        Spec {
            name: "Windows fixture",
            workspace: directory.into(),
            stall_after: Duration::from_secs(10),
            session: None,
            require_terminal: false,
            parse: |line| {
                let mut event = Event::new(EventKind::Raw, line);
                if line == "owned_ack" {
                    event.kind = EventKind::StepStart;
                    event.session = Some(SessionId::try_from("ses_owned_job".to_owned()).unwrap());
                }
                event
            },
        },
        fixture_command(role, directory),
        events,
        &Cancellation::default(),
    )
    .unwrap()
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "fixture readiness timed out");
        thread::sleep(Duration::from_millis(10));
    }
}

fn joined(run: Run) -> RunResult {
    bounded(move || run.join())
}

fn bounded<T: Send + 'static>(operation: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || tx.send(operation()).unwrap());
    let result = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("native ownership did not close");
    worker.join().unwrap();
    result
}

struct Independent(Child);

impl Drop for Independent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Identity {
    pid: u32,
    created: u64,
}

fn open_process(pid: u32) -> io::Result<OwnedHandle> {
    // SAFETY: read-only observation; this handle is never used to kill by PID.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful OpenProcess transferred ownership of a new handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

fn creation_time(handle: &OwnedHandle) -> u64 {
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: all output structs and the query handle are live.
    let ok = unsafe {
        GetProcessTimes(
            handle.as_raw_handle(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    assert_ne!(ok, 0);
    (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime)
}

impl Identity {
    fn observe(directory: &Path, role: &str) -> Self {
        let path = directory.join(format!("{role}.pid"));
        let mut pid = None;
        wait_for(|| {
            pid = std::fs::read_to_string(&path)
                .ok()
                .and_then(|value| value.parse().ok());
            pid.is_some()
        });
        let pid = pid.unwrap();
        let handle = open_process(pid).unwrap();
        // SAFETY: this is our live, read-only process handle.
        assert_ne!(
            unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) },
            WAIT_OBJECT_0
        );
        Self {
            pid,
            created: creation_time(&handle),
        }
        // Drop observation handles before waiting for Job accounting to drain.
    }

    fn assert_exited(&self) {
        match open_process(self.pid) {
            Ok(handle) if creation_time(&handle) == self.created => {
                // SAFETY: identity and handle are pinned to the original process.
                assert_eq!(
                    unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) },
                    WAIT_OBJECT_0
                );
            }
            Ok(_) => {} // The ID now belongs to a different process; never touch it.
            Err(error) => assert_eq!(error.raw_os_error(), Some(ERROR_INVALID_PARAMETER as i32)),
        }
    }
}

#[test]
fn stop_closes_immediate_and_nested_job_descendants_but_preserves_unrelated_process_and_ack() {
    let directory = tempfile::tempdir().unwrap();
    let mut unrelated = Independent(
        fixture_command("independent", directory.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let capture = Arc::clone(&events);
    let run = start_run(
        "parent",
        directory.path(),
        Arc::new(move |event| capture.lock().unwrap().push(event)),
    );
    let identities = ["parent", "descendant", "grandchild"]
        .map(|role| Identity::observe(directory.path(), role));
    wait_for(|| {
        events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.kind == EventKind::StepStart)
    });
    run.cancel();
    let result = joined(run);
    assert_eq!(result.status, RunStatus::Cancelled);
    assert_eq!(result.accepted_session.unwrap().as_str(), "ses_owned_job");
    assert!(matches!(
        result.termination,
        Some(TerminationEvidence::WindowsJobEmpty { forced: true, .. })
    ));
    for identity in identities {
        identity.assert_exited();
    }
    assert!(unrelated.0.try_wait().unwrap().is_none());
    let heartbeat = directory.path().join("grandchild.heartbeat");
    let last = std::fs::read(&heartbeat).ok();
    thread::sleep(Duration::from_millis(80));
    assert_eq!(std::fs::read(&heartbeat).ok(), last);
    assert!(unrelated.0.try_wait().unwrap().is_none());
}

#[test]
fn stop_between_assignment_and_resume_never_executes_provider() {
    let directory = tempfile::tempdir().unwrap();
    let mut command = fixture_command("parent", directory.path());
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let stop = Cancellation::default();
    let mut owned = ChildGuard::spawn_suspended(&mut command, &stop, Arc::new(|_| {})).unwrap();
    assert_eq!(owned.job.active().unwrap(), 1);
    assert!(!directory.path().join("parent.pid").exists());
    stop.request();
    assert_eq!(owned.resume(&stop).unwrap_err(), "Stopped");
    let result = bounded(move || owned.finish(&stop, false));
    assert!(matches!(
        result.termination,
        Some(TerminationEvidence::WindowsJobEmpty { forced: true, .. })
    ));
    assert!(!directory.path().join("parent.pid").exists());
    assert!(!directory.path().join("descendant.pid").exists());
}

#[test]
fn dropping_owned_run_closes_its_native_job() {
    let directory = tempfile::tempdir().unwrap();
    let run = start_run("parent", directory.path(), Arc::new(|_| {}));
    let identities = ["parent", "descendant", "grandchild"]
        .map(|role| Identity::observe(directory.path(), role));
    bounded(move || drop(run));
    for identity in identities {
        identity.assert_exited();
    }
}

#[test]
fn normal_exit_reports_independent_job_closure_without_forced_termination() {
    let directory = tempfile::tempdir().unwrap();
    let result = joined(start_run("exit", directory.path(), Arc::new(|_| {})));
    assert_eq!(result.status, RunStatus::Completed, "{:?}", result.errors);
    assert_eq!(result.exit_code, Some(0));
    assert!(matches!(
        result.termination,
        Some(TerminationEvidence::WindowsJobEmpty { forced: false, .. })
    ));
}

#[test]
fn zero_exit_with_leftover_owned_descendant_cannot_report_completed() {
    let directory = tempfile::tempdir().unwrap();
    let run = start_run("leftover", directory.path(), Arc::new(|_| {}));
    let descendant = Identity::observe(directory.path(), "grandchild");
    let result = joined(run);
    assert_eq!(result.status, RunStatus::Failed);
    assert_eq!(result.exit_code, Some(0));
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.contains("left owned processes running"))
    );
    assert!(matches!(
        result.termination,
        Some(TerminationEvidence::WindowsJobEmpty { forced: true, .. })
    ));
    descendant.assert_exited();
}

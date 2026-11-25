use super::*;
use crate::hexcli::{Stt, SttError};
use crate::service::Launcher;
use crate::tools::Action;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

struct NoMicrophone;
impl Stt for NoMicrophone {
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

#[derive(Default)]
struct LocalActions(AtomicUsize);
impl Launcher for LocalActions {
    fn launch(&self, action: &Action) -> Result<String, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(action.describe())
    }
}

fn wait(
    service: &Service,
    predicate: impl Fn(&super::super::Snapshot) -> bool,
) -> super::super::Snapshot {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let snapshot = service.snapshot(0);
        if predicate(&snapshot) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "background state timed out: {snapshot:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn desktop_plans_and_stale_ids_cannot_be_detached() {
    let service = Service::new(
        Arc::new(NoMicrophone),
        Arc::new(LocalActions::default()),
        None,
    );
    let task = service.plan("open firefox", Mode::Actions).unwrap();
    assert!(!service.snapshot(0).can_run_in_background);
    assert!(service.approve_background(task.id + 1).is_err());
    assert!(service.approve_background(task.id).is_err());
    assert_eq!(service.snapshot(0).phase, Phase::AwaitingApproval);
    assert!(service.snapshot(0).background_runs.is_empty());
    assert!(service.cancel_run(task.id).is_err());
    service.cancel_foreground();
    assert_eq!(service.snapshot(0).phase, Phase::Cancelled);
}

#[test]
fn a_panicking_owned_worker_fails_without_stranding_its_execution_lease() {
    struct Panics;
    impl Launcher for Panics {
        fn launch(&self, _: &Action) -> Result<String, String> {
            panic!("deliberate owned-worker fixture failure")
        }
    }
    let service = Service::new(Arc::new(NoMicrophone), Arc::new(Panics), None);
    let task = service.plan("open firefox", Mode::Actions).unwrap();
    service.approve(task.id).unwrap();
    let failed = wait(&service, |snapshot| {
        snapshot.phase == Phase::Failed && service.workers_finished()
    });
    assert_eq!(failed.task.as_ref().unwrap().id, task.id);
    assert!(
        failed
            .task
            .unwrap()
            .error
            .unwrap()
            .contains("worker stopped unexpectedly")
    );
    assert_eq!(failed.pending_job_approvals, Some(0));
    let next = service.plan("open firefox", Mode::Actions).unwrap();
    assert!(next.id > task.id);
    service.cancel_foreground();
}

#[cfg(unix)]
fn fixture() -> (tempfile::TempDir, Service, Arc<LocalActions>, Vec<PathBuf>) {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("opencode");
    std::fs::write(
        &executable,
        r#"#!/bin/sh
if [ "$1" = '--version' ]; then echo 'opencode v2.2.7'; exit 0; fi
if [ "$2" = '--help' ]; then printf '%s\n' '--standalone' '--dir' '--session' '--format json'; exit 0; fi
echo $$ > worker.pid
printf '%s\n' '{"type":"step_start","sessionID":"ses_background_fixture","part":{}}'
while [ ! -f finish ]; do sleep 0.05; done
printf '%s\n' '{"type":"step_finish","sessionID":"ses_background_fixture","part":{"reason":"stop"}}'
"#,
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let roots = (0..5)
        .map(|index| {
            let path = directory.path().join(format!("project-{index}"));
            std::fs::create_dir(&path).unwrap();
            assert!(
                std::process::Command::new("git")
                    .args(["init", "-q"])
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
            path
        })
        .collect();
    let launcher = Arc::new(LocalActions::default());
    let service = Service::new(Arc::new(NoMicrophone), launcher.clone(), Some(executable));
    (directory, service, launcher, roots)
}

#[cfg(unix)]
fn start(service: &Service, path: &std::path::Path) -> u64 {
    service.set_workspace(path.to_str().unwrap()).unwrap();
    let task = service.plan("inspect the project", Mode::Work).unwrap();
    assert!(service.snapshot(0).can_run_in_background);
    service.approve_background(task.id).unwrap();
    wait(service, |s| {
        s.background_runs
            .iter()
            .any(|run| run.task.id == task.id && run.task.accepted_session.is_some())
    });
    task.id
}

#[cfg(unix)]
#[test]
fn real_background_processes_keep_independent_state_and_foreground_commands_work() {
    let (_directory, service, launcher, roots) = fixture();
    let first = start(&service, &roots[0]);
    let second = start(&service, &roots[1]);
    assert!(second > first);
    assert_eq!(service.snapshot(0).phase, Phase::Idle);
    let desktop = service.plan("open firefox", Mode::Actions).unwrap();
    assert!(desktop.id > second);
    service.approve(desktop.id).unwrap();
    wait(&service, |snapshot| snapshot.phase == Phase::Completed);
    assert_eq!(launcher.0.load(Ordering::SeqCst), 1);
    assert!(
        service
            .snapshot(0)
            .background_runs
            .iter()
            .all(|run| run.task.phase == Phase::Running)
    );

    service.cancel_run(first).unwrap();
    let state = wait(&service, |snapshot| {
        snapshot
            .background_runs
            .iter()
            .any(|run| run.task.id == first && run.task.phase == Phase::Cancelled)
    });
    assert_eq!(state.task.as_ref().unwrap().id, desktop.id);
    assert_eq!(state.phase, Phase::Completed);
    assert_eq!(
        state
            .background_runs
            .iter()
            .find(|run| run.task.id == second)
            .unwrap()
            .task
            .phase,
        Phase::Running
    );
    let pid: i32 = std::fs::read_to_string(roots[0].join("worker.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // The cancelled runner has joined its actual shell process before publishing completion.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert!(service.cancel_run(second + 100).is_err());
    let preview = service.plan("open firefox", Mode::Actions).unwrap();
    assert!(service.cancel_foreground_task(desktop.id).is_err());
    service.cancel_foreground_task(preview.id).unwrap();
    assert_eq!(
        service
            .snapshot(0)
            .background_runs
            .iter()
            .find(|run| run.task.id == second)
            .unwrap()
            .task
            .phase,
        Phase::Running
    );
    assert!(service.request_stop_from_text("stop all"));
    wait(&service, |snapshot| {
        snapshot
            .background_runs
            .iter()
            .all(|run| !run.task.phase.is_busy())
    });
    assert_eq!(service.snapshot(0).phase, Phase::Cancelled);
}

#[cfg(unix)]
#[test]
fn project_lease_refusal_preserves_the_exact_pending_preview() {
    let (_directory, service, _, roots) = fixture();
    let first = start(&service, &roots[0]);
    let pending = service
        .plan("a second approved change", Mode::Work)
        .unwrap();
    assert!(
        service
            .approve_background(pending.id)
            .unwrap_err()
            .contains("owns this project")
    );
    let snapshot = service.snapshot(0);
    assert_eq!(snapshot.phase, Phase::AwaitingApproval);
    assert_eq!(snapshot.task.as_ref().unwrap().id, pending.id);
    assert_eq!(snapshot.task.as_ref().unwrap().text, pending.text);
    assert_eq!(snapshot.background_runs.len(), 1);
    assert!(
        service
            .approve(pending.id)
            .unwrap_err()
            .contains("owns this project")
    );
    service.cancel_run(first).unwrap();
    wait(&service, |snapshot| {
        snapshot.background_runs[0].task.phase == Phase::Cancelled && !service.background_busy()
    });
    service.approve_background(pending.id).unwrap();
    wait(&service, |snapshot| {
        snapshot
            .background_runs
            .iter()
            .any(|run| run.task.id == pending.id && run.task.phase == Phase::Running)
    });
    service.shutdown();
    assert!(!service.background_busy());
}

#[cfg(unix)]
#[test]
fn background_capacity_is_bounded_and_stop_all_reaps_owned_runs() {
    let (_directory, service, _, roots) = fixture();
    for root in &roots[..4] {
        start(&service, root);
    }
    service.set_workspace(roots[4].to_str().unwrap()).unwrap();
    let pending = service.plan("one more task", Mode::Work).unwrap();
    assert!(
        service
            .approve_background(pending.id)
            .unwrap_err()
            .contains("Four background runs")
    );
    assert_eq!(service.snapshot(0).task.unwrap().id, pending.id);
    service.shutdown();
    assert_eq!(service.snapshot(0).phase, Phase::Cancelled);
    assert!(!service.background_busy());
    assert!(
        service
            .snapshot(0)
            .background_runs
            .iter()
            .all(|run| run.task.phase == Phase::Cancelled)
    );
}

#[cfg(unix)]
#[test]
fn completion_notices_survive_foreground_replacement_and_clear_keeps_owned_work() {
    use crate::service::NoticeKind;

    let (_directory, service, _, roots) = fixture();
    let running = start(&service, &roots[0]);
    let completed = service.plan("open firefox", Mode::Actions).unwrap();
    service.approve(completed.id).unwrap();
    wait(&service, |s| {
        s.phase == Phase::Completed && !service.workers_finished()
    });
    // A new foreground preview must not erase a result waiting for the daemon.
    let next = service.plan("open firefox", Mode::Actions).unwrap();
    let notices = service.take_task_notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].task_id, completed.id);
    assert_eq!(notices[0].kind, NoticeKind::Launched);
    assert!(service.take_task_notices().is_empty());
    assert_eq!(service.task_attention().unwrap().task_id, next.id);
    assert_eq!(service.clear_completed_activity(), 0);
    assert_eq!(service.snapshot(0).task.unwrap().id, next.id);
    assert_eq!(service.snapshot(0).background_runs.len(), 1);
    std::fs::write(roots[0].join("finish"), "").unwrap();
    wait(&service, |s| {
        s.background_runs[0].task.phase == Phase::Completed && !service.background_busy()
    });
    let notices = service.take_task_notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].task_id, running);
    assert_eq!(notices[0].kind, NoticeKind::Delivered);
    assert_eq!(service.clear_completed_activity(), 1);
    assert!(service.snapshot(0).background_runs.is_empty());
    assert_eq!(service.snapshot(0).task.unwrap().id, next.id);
    service.cancel_foreground();
    assert_eq!(service.clear_completed_activity(), 1);
    assert_eq!(service.snapshot(0).phase, Phase::Idle);
    assert!(service.snapshot(0).task.is_none());
}

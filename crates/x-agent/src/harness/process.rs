//! Process ownership and evidence shared by the headless coding adapters.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::evidence::{change_snapshot, diff_snapshots};
use super::{Event, EventKind, EventSink, RunResult, RunStatus, SessionId, TerminationEvidence};
use crate::cancellation::Cancellation;

const POLL: Duration = Duration::from_millis(200);
const KILL_GRACE: Duration = Duration::from_secs(5);
const PIPE_POLL: Duration = Duration::from_millis(15);

mod output;
use output::{Frame, Frames, PollPipe, read_frame};

#[cfg(any(windows, target_os = "linux", target_os = "macos", test))]
mod closure;
mod test_capture;
pub(crate) use test_capture::capture_test;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
mod unix;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix_group;
#[cfg(windows)]
mod windows;
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
use unix::ChildGuard;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use unix_group::ChildGuard;
#[cfg(windows)]
use windows::ChildGuard;

struct Finished {
    exit_code: Option<i32>,
    termination: Option<TerminationEvidence>,
    failure: Option<&'static str>,
}

pub(crate) struct Spec {
    pub name: &'static str,
    pub workspace: PathBuf,
    pub stall_after: Duration,
    pub session: Option<SessionId>,
    pub require_terminal: bool,
    pub parse: fn(&str) -> Event,
}

pub struct Run {
    canceller: Canceller,
    handle: Option<JoinHandle<RunResult>>,
}

/// A cloneable handle that can Stop a run from any thread.
#[derive(Clone)]
pub struct Canceller {
    stop: Cancellation,
}

impl Canceller {
    /// Request Stop. The supervisor that owns the child handles termination.
    pub fn cancel(&self) {
        self.stop.request();
    }
}

impl Run {
    pub(crate) fn start(
        spec: Spec,
        mut cmd: Command,
        on_event: EventSink,
        parent: &Cancellation,
    ) -> Result<Self, String> {
        let stop = parent.child();
        stop.check()?;
        let before = change_snapshot(&spec.workspace);
        cmd.current_dir(&spec.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut owned = ChildGuard::spawn(&mut cmd, &stop, Arc::clone(&on_event))
            .map_err(|error| format!("Could not start {}: {error}", spec.name))?;
        let child_stop = stop.clone();
        let handle = thread::Builder::new()
            .spawn(move || supervise(&mut owned, &spec, before, &child_stop, on_event))
            .map_err(|error| format!("Could not start supervisor: {error}"))?;
        Ok(Self {
            canceller: Canceller { stop },
            handle: Some(handle),
        })
    }

    pub fn canceller(&self) -> Canceller {
        self.canceller.clone()
    }

    /// Stop the owned Unix process group or private Windows Job.
    pub fn cancel(&self) {
        self.canceller.cancel();
    }

    pub fn join(mut self) -> RunResult {
        self.handle
            .take()
            .and_then(|handle| handle.join().ok())
            .unwrap_or(RunResult {
                status: RunStatus::Failed,
                exit_code: None,
                accepted_session: None,
                errors: vec!["supervisor thread panicked".into()],
                stderr: vec![],
                changed_files: None,
                termination: None,
            })
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.cancel();
            let _ = handle.join();
        }
    }
}

impl super::OwnedRun for Run {
    fn join(self: Box<Self>) -> RunResult {
        (*self).join()
    }
}

fn supervise(
    owned: &mut ChildGuard,
    cfg: &Spec,
    before: Option<BTreeMap<String, String>>,
    stop: &Cancellation,
    on_event: EventSink,
) -> RunResult {
    let (mut errors, mut stderr_tail): (Vec<String>, Vec<String>) = (vec![], vec![]);
    let mut stdout = owned
        .child_mut()
        .stdout
        .take()
        .and_then(|pipe| PollPipe::new(pipe).ok())
        .map(Frames::new);
    let mut stderr = owned
        .child_mut()
        .stderr
        .take()
        .and_then(|pipe| PollPipe::new(pipe).ok())
        .map(Frames::new);
    if stdout.is_none() || stderr.is_none() {
        errors.push("Could not open an owned harness output pipe".into());
    }
    let (mut last, mut stalled) = (Instant::now(), false);
    let mut accepted_session = None;
    let mut terminal = false;
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    let mut exited_at = None::<Instant>;
    'capture: while (stdout.is_some() || stderr.is_some()) && errors.is_empty() && !stop.requested()
    {
        // An exited root can leave inherited pipe handles in a live descendant.
        // EOF alone must not keep the supervisor outside its native process cleanup.
        // The running provider has no new total deadline; only its post-exit
        // output drain is bounded. Check even while descendants emit output.
        #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
        match owned.observe_exit() {
            Ok(true) => {
                let ended = exited_at.get_or_insert_with(Instant::now);
                if ended.elapsed() >= KILL_GRACE {
                    let message = format!(
                        "{} left owned processes running with open output after its exit",
                        cfg.name
                    );
                    errors.push(message.clone());
                    on_event(Event::new(EventKind::Error, message));
                    break;
                }
            }
            Ok(false) => {}
            Err(_) => {
                let message = "Could not observe the owned process exit";
                errors.push(message.into());
                on_event(Event::new(EventKind::Error, message));
                break;
            }
        }
        let mut progressed = false;
        for (is_stdout, read) in [
            (true, read_frame(&mut stdout)),
            (false, read_frame(&mut stderr)),
        ] {
            if stop.requested() {
                break 'capture;
            }
            let line = match read {
                Err(_) => {
                    let message = "Harness output exceeded its frame limit or could not be read";
                    errors.push(message.into());
                    on_event(Event::new(EventKind::Error, message));
                    break 'capture;
                }
                Ok(Frame::Pending | Frame::Closed) => continue,
                Ok(Frame::Progress) => {
                    progressed = true;
                    continue;
                }
                Ok(Frame::Line(line)) => line,
            };
            progressed = true;
            (last, stalled) = (Instant::now(), false);
            if !is_stdout {
                stderr_tail.push(line.trim().chars().take(450).collect());
                if stderr_tail.len() > 5 {
                    stderr_tail.remove(0);
                }
                continue;
            }
            let l = line.trim();
            if l.is_empty() {
                continue;
            }
            let event = (cfg.parse)(l);
            let expected = cfg.session.as_ref().or(accepted_session.as_ref());
            if let (Some(expected), Some(reported)) = (expected, &event.session)
                && expected != reported
            {
                let message = format!("{} reported a different session", cfg.name);
                errors.push(message.clone());
                on_event(Event::new(EventKind::Error, message));
                break 'capture;
            }
            terminal |= event.kind == EventKind::StepFinish && event.session.is_some();
            if accepted_session.is_none() {
                accepted_session = event
                    .accepted_session()
                    .or({
                        if cfg.require_terminal && terminal {
                            event.session.as_ref()
                        } else {
                            None
                        }
                    })
                    .cloned();
            }
            if event.kind == EventKind::Error {
                errors.push(event.text.clone());
                on_event(event);
                break 'capture;
            }
            on_event(event);
        }
        if !progressed && (stdout.is_some() || stderr.is_some()) {
            if !stalled && last.elapsed() > cfg.stall_after {
                stalled = true;
                on_event(Event::new(
                    EventKind::Stall,
                    format!(
                        "No output for {}s: the agent may be waiting on a permission or the provider. Stop to abort.",
                        cfg.stall_after.as_secs()
                    ),
                ));
            }
            thread::sleep(PIPE_POLL);
        }
    }
    drop(stdout);
    drop(stderr);
    let finished = owned.finish(stop, !errors.is_empty());
    let exit_code = finished.exit_code;
    if let Some(failure) = finished.failure {
        errors.push(format!("{} {failure}", cfg.name));
    }
    let changed_files = diff_snapshots(before.as_ref(), change_snapshot(&cfg.workspace).as_ref());
    if !stop.requested() && cfg.session.is_some() && accepted_session.is_none() && errors.is_empty()
    {
        errors.push(format!(
            "{} did not acknowledge the selected session",
            cfg.name
        ));
    }
    if !stop.requested() && cfg.require_terminal && !terminal && errors.is_empty() {
        errors.push(format!(
            "{} did not report a structured completion",
            cfg.name
        ));
    }
    let status = if stop.requested() {
        RunStatus::Cancelled
    } else if !errors.is_empty() || exit_code != Some(0) {
        RunStatus::Failed
    } else {
        RunStatus::Completed
    };
    stderr_tail.retain(|l| !l.is_empty());
    RunResult {
        status,
        exit_code,
        accepted_session,
        errors,
        stderr: stderr_tail,
        changed_files,
        termination: finished.termination,
    }
}

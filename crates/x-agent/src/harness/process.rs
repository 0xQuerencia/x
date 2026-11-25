//! Process ownership and evidence shared by the headless coding adapters.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

use super::evidence::{change_snapshot, diff_snapshots};
use super::{Event, EventKind, EventSink, RunResult, RunStatus, SessionId};
use crate::cancellation::Cancellation;

const POLL: Duration = Duration::from_millis(200);
const KILL_GRACE: Duration = Duration::from_secs(5);
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const OUTPUT_QUEUE: usize = 32;

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
        #[cfg(unix)]
        cmd.process_group(0);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        let child = crate::retry_busy(|| {
            if stop.requested() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "Stopped",
                ));
            }
            cmd.spawn()
        })
        .map_err(|error| format!("Could not start {}: {error}", spec.name))?;
        let child_stop = stop.clone();
        let mut owned = ChildGuard(child);
        let handle = thread::Builder::new()
            .spawn(move || supervise(owned.child_mut(), &spec, before, &child_stop, on_event))
            .map_err(|error| format!("Could not start supervisor: {error}"))?;
        Ok(Self {
            canceller: Canceller { stop },
            handle: Some(handle),
        })
    }

    pub fn canceller(&self) -> Canceller {
        self.canceller.clone()
    }

    /// Stop the run: SIGTERM the whole process group, SIGKILL after a grace period.
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

#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// A polite stop first, then a forced one.
#[derive(Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
fn signal_group(pgid: i32, sig: Signal) {
    if pgid > 0 {
        let sig = match sig {
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        };
        // SAFETY: plain syscall on a process group we created with process_group(0).
        unsafe { libc::killpg(pgid, sig) };
    }
}

/// Windows has no process-group signals; `taskkill /T` ends the whole process tree.
#[cfg(windows)]
fn signal_group(pgid: i32, sig: Signal) {
    if pgid > 0 {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/PID", &pgid.to_string(), "/T"]);
        if matches!(sig, Signal::Kill) {
            cmd.arg("/F");
        }
        let _ = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn terminate(child: &mut Child) {
    let pgid = child.id() as i32;
    signal_group(pgid, Signal::Term);
    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            signal_group(pgid, Signal::Kill); // reap any stragglers in the group
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    signal_group(pgid, Signal::Kill);
    let _ = child.wait();
}

// Own the spawned process even if thread creation or supervision panics.
struct ChildGuard(Child);

impl ChildGuard {
    fn child_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            terminate(&mut self.0);
        }
    }
}

enum Line {
    Out(String),
    Err(String),
    Closed,
    ReadError,
}

fn read_frame(reader: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let newline = available.iter().position(|&byte| byte == b'\n');
        let count = newline.map_or(available.len(), |index| index + 1);
        if bytes.len() + count > MAX_FRAME_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Harness output frame exceeds limit",
            ));
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_some() {
            break;
        }
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn pump(stream: impl std::io::Read + Send + 'static, out: bool, tx: mpsc::SyncSender<Line>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        loop {
            let message = match read_frame(&mut reader) {
                Ok(Some(line)) if out => Line::Out(line),
                Ok(Some(line)) => Line::Err(line),
                Ok(None) => Line::Closed,
                Err(_) => Line::ReadError,
            };
            let end = matches!(message, Line::Closed | Line::ReadError);
            if tx.send(message).is_err() || end {
                break;
            }
        }
    });
}

fn supervise(
    child: &mut Child,
    cfg: &Spec,
    before: Option<BTreeMap<String, String>>,
    stop: &Cancellation,
    on_event: EventSink,
) -> RunResult {
    let (tx, rx) = mpsc::sync_channel(OUTPUT_QUEUE);
    if let Some(s) = child.stdout.take() {
        pump(s, true, tx.clone());
    }
    if let Some(s) = child.stderr.take() {
        pump(s, false, tx.clone());
    }
    drop(tx);
    let (mut open, mut last, mut stalled) = (2, Instant::now(), false);
    let (mut errors, mut stderr_tail): (Vec<String>, Vec<String>) = (vec![], vec![]);
    let mut accepted_session = None;
    let mut terminal = false;
    while open > 0 && !stop.requested() {
        match rx.recv_timeout(POLL) {
            Err(RecvTimeoutError::Timeout) => {
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
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Ok(Line::Closed) => open -= 1,
            Ok(Line::ReadError) => {
                let message = "Harness output exceeded its frame limit or could not be read";
                errors.push(message.into());
                on_event(Event::new(EventKind::Error, message));
                break;
            }
            Ok(Line::Err(l)) => {
                (last, stalled) = (Instant::now(), false);
                stderr_tail.push(l.trim().chars().take(450).collect());
                if stderr_tail.len() > 5 {
                    stderr_tail.remove(0);
                }
            }
            Ok(Line::Out(l)) => {
                (last, stalled) = (Instant::now(), false);
                let l = l.trim();
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
                    break;
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
                    break;
                }
                on_event(event);
            }
        }
    }
    drop(rx);
    let exit_code = if stop.requested() || !errors.is_empty() {
        terminate(child);
        child.wait().ok().and_then(|s| s.code())
    } else {
        match wait_with_grace(child, stop) {
            Some(s) => s.code(),
            None => {
                if !stop.requested() {
                    errors.push(format!(
                        "{} did not exit after closing its output",
                        cfg.name
                    ));
                }
                terminate(child);
                child.wait().ok().and_then(|s| s.code())
            }
        }
    };
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
    }
}

fn wait_with_grace(child: &mut Child, stop: &Cancellation) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline && !stop.requested() {
        if let Ok(Some(s)) = child.try_wait() {
            return Some(s);
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_bounded_before_allocating_unlimited_output() {
        let mut input = std::io::Cursor::new(vec![b'a'; MAX_FRAME_BYTES + 1]);
        assert_eq!(
            read_frame(&mut input).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn frames_preserve_partial_final_lines_and_reject_invalid_utf8() {
        let mut input = std::io::Cursor::new(b"one\ntwo");
        assert_eq!(read_frame(&mut input).unwrap().as_deref(), Some("one\n"));
        assert_eq!(read_frame(&mut input).unwrap().as_deref(), Some("two"));
        assert_eq!(read_frame(&mut input).unwrap(), None);
        let mut invalid = std::io::Cursor::new([0xff]);
        assert!(read_frame(&mut invalid).is_err());
    }
}

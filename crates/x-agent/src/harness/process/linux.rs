//! Linux process group ownership with an unreaped leader. A process group
//! does not constrain providers that create new sessions or delegate work.

use std::fs;
use std::io;
use std::os::unix::process::CommandExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::{Child, Command};
#[cfg(test)]
use std::thread;
use std::time::{Duration, Instant};

use super::closure::{self, State};
use super::{Event, EventKind, EventSink, Finished, KILL_GRACE};
use crate::cancellation::Cancellation;

pub(super) struct ChildGuard {
    child: Child,
    events: EventSink,
    closed: bool,
}

impl ChildGuard {
    pub(super) fn spawn(
        cmd: &mut Command,
        stop: &Cancellation,
        events: EventSink,
    ) -> Result<Self, String> {
        cmd.process_group(0);
        let child = crate::retry_busy(|| {
            if stop.requested() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "Stopped"));
            }
            cmd.spawn()
        })
        .map_err(|error| error.to_string())?;
        Ok(Self {
            child,
            events,
            closed: false,
        })
    }

    pub(super) fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    // Do not reap the leader until cleanup has finished. Its retained PID
    // prevents a subsequent signal from targeting a recycled group identity.
    pub(super) fn observe_exit(&mut self) -> io::Result<bool> {
        // SAFETY: an all-zero siginfo_t is valid as an output buffer.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: the output points to a valid siginfo_t and the ID is the
        // direct child we own. WNOWAIT preserves its waitable exit status.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            // SAFETY: successful waitid initialized this zeroed result.
            Ok(unsafe { info.si_pid() } != 0)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn signal(&self, signal: i32) {
        // SAFETY: the group was created with process_group(0) and the direct
        // child is not reaped until after all group signals are complete.
        unsafe { libc::killpg(self.child.id() as i32, signal) };
    }

    fn close(&mut self, stop: &Cancellation, failed: bool) -> (Option<i32>, bool) {
        let start = Instant::now();
        let mut forced = failed || stop.requested();
        let mut last_kill = None::<Instant>;
        let events = self.events.clone();
        let mut empty_observations = 0;
        closure::wait_until_empty(
            KILL_GRACE,
            || {
                // A wait error does not authorize signaling an unknown group.
                let Ok(root_exited) = self.observe_exit() else {
                    empty_observations = 0;
                    return State::Unavailable;
                };
                let group = group_state(self.child.id() as i32);
                if root_exited && matches!(group, State::Empty) {
                    empty_observations += 1;
                    if empty_observations >= 2 {
                        return State::Empty;
                    }
                    return State::Active;
                }
                empty_observations = 0;
                forced |= stop.requested() || start.elapsed() >= KILL_GRACE;
                if forced
                    && last_kill.is_none_or(|last| last.elapsed() >= Duration::from_millis(250))
                {
                    self.signal(libc::SIGKILL);
                    last_kill = Some(Instant::now());
                }
                match group {
                    State::Unavailable => State::Unavailable,
                    _ => State::Active,
                }
            },
            || {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    events(Event::new(
                        EventKind::TerminationUnconfirmed,
                        "Linux has not confirmed that the owned process group has stopped. X retains the project lease while cleanup continues.",
                    ));
                }));
            },
        );
        let exit = self.child.wait().ok().and_then(|status| status.code());
        self.closed = true;
        (exit, forced)
    }

    pub(super) fn finish(&mut self, stop: &Cancellation, failed: bool) -> Finished {
        let (exit_code, forced) = self.close(stop, failed);
        Finished {
            exit_code,
            // A procfs scan is not the same kernel guarantee as a Windows Job.
            // Do not advertise Windows closure evidence on Linux.
            termination: None,
            failure: (forced && !stop.requested() && !failed)
                .then_some("did not exit cleanly: required forced process-group cleanup"),
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.closed {
            self.close(&Cancellation::default(), true);
        }
    }
}

// The comm field is parenthesized and may contain spaces or closing parens.
// Only fields after its last closing paren have the fixed numeric format.
fn stat_fields(bytes: &[u8]) -> Option<(i32, bool)> {
    let end = bytes.iter().rposition(|byte| *byte == b')')?;
    let tail = std::str::from_utf8(bytes.get(end + 1..)?).ok()?;
    let fields: Vec<_> = tail.split_whitespace().collect();
    let group = fields.get(2)?.parse().ok()?;
    let threads: u32 = fields.get(17)?.parse().ok()?;
    let dead = matches!(*fields.first()?, "Z" | "X") && threads <= 1;
    Some((group, dead))
}

fn group_state(pgid: i32) -> State {
    let Ok(entries) = fs::read_dir("/proc") else {
        return State::Unavailable;
    };
    let mut saw_leader = false;
    for entry in entries {
        let Ok(entry) = entry else {
            return State::Unavailable;
        };
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.parse::<i32>().ok()) else {
            continue;
        };
        let bytes = match fs::read(entry.path().join("stat")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return State::Unavailable,
        };
        let Some((group, dead)) = stat_fields(&bytes) else {
            return State::Unavailable;
        };
        if pid == pgid {
            saw_leader = group == pgid;
        }
        if group == pgid && !dead {
            return State::Active;
        }
    }
    if saw_leader {
        State::Empty
    } else {
        State::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn stat_parses_parentheses_and_keeps_multithreaded_zombies_active() {
        let mut fields = ["0"; 18];
        fields[0] = "Z";
        fields[2] = "99";
        fields[17] = "2";
        let stat = |fields: &[&str]| format!("100 (a ) b) {}", fields.join(" "));
        assert_eq!(stat_fields(stat(&fields).as_bytes()), Some((99, false)));
        fields[17] = "1";
        assert_eq!(stat_fields(stat(&fields).as_bytes()), Some((99, true)));
        assert_eq!(stat_fields(b"malformed"), None);
    }

    #[test]
    fn clean_root_exit_does_not_require_forced_termination() {
        let stop = Cancellation::default();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exit 0"]);
        let mut guard = ChildGuard::spawn(&mut cmd, &stop, Arc::new(|_| {})).unwrap();
        let result = guard.finish(&stop, false);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.failure.is_none());
    }

    #[test]
    fn stop_keeps_other_owned_groups_running() {
        let stop = Cancellation::default();
        let mut first = Command::new("sleep");
        first.arg("30");
        let mut second = Command::new("sleep");
        second.arg("30");
        let mut first = ChildGuard::spawn(&mut first, &stop, Arc::new(|_| {})).unwrap();
        let mut second = ChildGuard::spawn(&mut second, &stop, Arc::new(|_| {})).unwrap();
        let first_stop = stop.child();
        first_stop.request();
        let result = first.finish(&first_stop, false);
        assert!(result.failure.is_none());
        assert!(!second.observe_exit().unwrap());
        stop.request();
        second.finish(&stop, false);
    }

    #[test]
    fn zero_exit_with_surviving_group_member_is_not_successful() {
        let stop = Cancellation::default();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 </dev/null >/dev/null 2>&1 & exit 0"]);
        let mut guard = ChildGuard::spawn(&mut cmd, &stop, Arc::new(|_| {})).unwrap();
        let start = Instant::now();
        while !guard.observe_exit().unwrap() && start.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(guard.observe_exit().unwrap());
        assert!(matches!(
            group_state(guard.child.id() as i32),
            State::Active
        ));
        let result = guard.finish(&stop, false);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.failure.is_some());
    }
}

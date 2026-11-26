//! Unix process-group ownership. Windows uses a private native Job instead.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use super::{EventSink, Finished, KILL_GRACE};
use crate::cancellation::Cancellation;

pub(super) struct ChildGuard(Child);

impl ChildGuard {
    pub(super) fn spawn(
        cmd: &mut Command,
        stop: &Cancellation,
        _events: EventSink,
    ) -> Result<Self, String> {
        cmd.process_group(0);
        crate::retry_busy(|| {
            if stop.requested() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "Stopped",
                ));
            }
            cmd.spawn()
        })
        .map(Self)
        .map_err(|error| error.to_string())
    }

    pub(super) fn child_mut(&mut self) -> &mut Child {
        &mut self.0
    }

    pub(super) fn observe_exit(&mut self) -> io::Result<bool> {
        observe_exit(&self.0)
    }

    pub(super) fn finish(&mut self, stop: &Cancellation, failed: bool) -> Finished {
        let mut failure = None;
        let exit_code = if stop.requested() || failed {
            terminate(&mut self.0);
            self.0.wait().ok().and_then(|status| status.code())
        } else {
            match wait_with_grace(&mut self.0, stop) {
                Some(status) => status.code(),
                None => {
                    if !stop.requested() {
                        failure = Some("did not exit after closing its output");
                    }
                    terminate(&mut self.0);
                    self.0.wait().ok().and_then(|status| status.code())
                }
            }
        };
        Finished {
            exit_code,
            termination: None,
            failure,
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        terminate(&mut self.0);
    }
}

fn signal_group(pgid: i32, signal: i32) {
    if pgid > 0 {
        // SAFETY: this process group was created by Command::process_group(0).
        unsafe { libc::killpg(pgid, signal) };
    }
}

fn observe_exit(child: &Child) -> io::Result<bool> {
    // SAFETY: waitid initializes its output. WNOWAIT leaves the direct child
    // unreaped so it pins the process group through all group signals.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == 0 {
        // SAFETY: a successful wait initialized the zeroed siginfo_t.
        Ok(unsafe { info.si_pid() } != 0)
    } else {
        Err(io::Error::last_os_error())
    }
}

fn terminate(child: &mut Child) {
    let pgid = child.id() as i32;
    if observe_exit(child).is_err() {
        return;
    }
    signal_group(pgid, libc::SIGTERM);
    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline {
        match observe_exit(child) {
            Ok(true) => break,
            Ok(false) => thread::sleep(Duration::from_millis(50)),
            Err(_) => return,
        }
    }
    signal_group(pgid, libc::SIGKILL);
    let _ = child.wait();
}

fn wait_with_grace(child: &mut Child, stop: &Cancellation) -> Option<ExitStatus> {
    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline && !stop.requested() {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

//! Bounded fixed-suite diagnostics with the coding supervisor's process ownership.

use std::io::ErrorKind;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{ChildGuard, POLL};
use crate::cancellation::Cancellation;
use crate::desktop::capture::{OutputPipe, prepare_pipe, read_ready};

const EXCERPT_BYTES: usize = 8_192;

pub(crate) struct CapturedTest {
    pub(crate) success: bool,
    pub(crate) stdout: (String, bool),
    pub(crate) stderr: (String, bool),
}

pub(crate) fn capture_test(
    command: &mut Command,
    limit: Duration,
    stop: &Cancellation,
) -> Result<CapturedTest, String> {
    stop.check()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut owned = ChildGuard::spawn(command, stop, Arc::new(|_| {}))
        .map_err(|_| "Could not start the approved test process")?;
    let result = (|| {
        let child = owned.child_mut();
        let mut stdout = child.stdout.take().ok_or("No test stdout pipe")?;
        let mut stderr = child.stderr.take().ok_or("No test stderr pipe")?;
        prepare_pipe(&stdout).map_err(|_| "Could not prepare test stdout")?;
        prepare_pipe(&stderr).map_err(|_| "Could not prepare test stderr")?;
        let mut stdout_tail = Tail::default();
        let mut stderr_tail = Tail::default();
        let deadline = Instant::now() + limit;
        let mut root_exited = None::<Instant>;
        loop {
            stop.check()?;
            let now = Instant::now();
            if now >= deadline {
                return Err("Approved tests timed out".to_owned());
            }
            let exited = owned
                .observe_exit()
                .map_err(|_| "Could not observe the test process")?;
            if exited && root_exited.get_or_insert(now).elapsed() >= super::KILL_GRACE {
                return Err("Approved tests left inherited output open after exit".to_owned());
            }
            let read_stdout = stdout_tail.read_available(&mut stdout)?;
            let read_stderr = stderr_tail.read_available(&mut stderr)?;
            if exited && stdout_tail.closed && stderr_tail.closed {
                break;
            }
            if !read_stdout && !read_stderr {
                std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
            }
        }
        Ok((stdout_tail.finish(), stderr_tail.finish()))
    })();
    // Finish owns cleanup even on an output read error or timeout. The caller's
    // project lease remains held until the native guard returns.
    let finished = owned.finish(stop, result.is_err());
    let (stdout, stderr) = result?;
    let exit_code = finished
        .exit_code
        .filter(|_| !stop.requested() && finished.failure.is_none())
        .ok_or("The approved test process did not finish reliably")?;
    Ok(CapturedTest {
        success: exit_code == 0,
        stdout,
        stderr,
    })
}

#[derive(Default)]
struct Tail {
    bytes: Vec<u8>,
    truncated: bool,
    closed: bool,
}

impl Tail {
    fn read_available(&mut self, stream: &mut impl OutputPipe) -> Result<bool, String> {
        if self.closed {
            return Ok(false);
        }
        let mut buffer = [0; EXCERPT_BYTES];
        match read_ready(stream, &mut buffer) {
            Ok(0) => {
                self.closed = true;
                Ok(false)
            }
            Ok(read) => {
                let discard = (self.bytes.len() + read).saturating_sub(EXCERPT_BYTES);
                if discard > 0 {
                    self.bytes.drain(..discard);
                    self.truncated = true;
                }
                self.bytes.extend_from_slice(&buffer[..read]);
                Ok(true)
            }
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) =>
            {
                Ok(false)
            }
            Err(_) => Err("Could not read local test output".into()),
        }
    }

    fn finish(self) -> (String, bool) {
        let Self {
            bytes, truncated, ..
        } = self;
        let text = String::from_utf8_lossy(&bytes)
            .chars()
            .filter(|ch| !ch.is_control() || matches!(*ch, '\n' | '\t'))
            .collect();
        (text, truncated)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn fixed_test_output_keeps_only_bounded_separate_tails() {
        let mut command = Command::new("sh");
        command.args(["-c", "head -c 65536 /dev/zero; printf 'out-end'; head -c 65536 /dev/zero >&2; printf 'err-end' >&2; exit 1"]);
        let result = capture_test(
            &mut command,
            Duration::from_secs(10),
            &Cancellation::default(),
        )
        .unwrap();
        assert!(!result.success);
        assert_eq!(result.stdout, ("out-end".into(), true));
        assert_eq!(result.stderr, ("err-end".into(), true));
    }

    #[test]
    fn fixed_test_deadline_cleans_its_group_without_waiting_for_inherited_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & wait"]);
        let start = Instant::now();
        assert!(
            capture_test(
                &mut command,
                Duration::from_millis(200),
                &Cancellation::default()
            )
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(10));
    }
}

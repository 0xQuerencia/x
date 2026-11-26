//! Bounded stdout capture for local helpers. The calling thread owns the pipe
//! and the direct child, including when a descendant keeps stdout open.

use std::io::{self, Read};
use std::process::{ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use crate::cancellation::Cancellation;

const OUTPUT_CAP: usize = 4_000_000;
const POLL: Duration = Duration::from_millis(15);

pub(crate) fn capture(cmd: &mut Command, limit: Duration) -> Result<(bool, Vec<u8>), String> {
    capture_scoped(cmd, limit, None)
}

pub(crate) fn capture_scoped(
    cmd: &mut Command,
    limit: Duration,
    stop: Option<&Cancellation>,
) -> Result<(bool, Vec<u8>), String> {
    match read_output(cmd, limit, stop, OUTPUT_CAP)? {
        Output::Complete(success, bytes) => Ok((success, bytes)),
        Output::Truncated(_) => Err("Helper output exceeded the capture limit".into()),
    }
}

pub(crate) fn capture_prefix(
    cmd: &mut Command,
    limit: Duration,
    cap: usize,
) -> Result<(Vec<u8>, bool), String> {
    match read_output(cmd, limit, None, cap)? {
        Output::Complete(true, bytes) => Ok((bytes, false)),
        Output::Truncated(bytes) => Ok((bytes, true)),
        Output::Complete(false, _) => Err("The local command failed".into()),
    }
}

enum Output {
    Complete(bool, Vec<u8>),
    Truncated(Vec<u8>),
}

fn read_output(
    cmd: &mut Command,
    limit: Duration,
    stop: Option<&Cancellation>,
    cap: usize,
) -> Result<Output, String> {
    if let Some(stop) = stop {
        stop.check()?;
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = crate::retry_busy(|| cmd.spawn())
        .map_err(|e| format!("could not run {:?}: {e}", cmd.get_program()))?;
    let result = (|| {
        let mut stdout = child.stdout.take().ok_or("No helper output pipe")?;
        prepare_pipe(&stdout).map_err(|e| format!("Could not prepare helper output: {e}"))?;
        let deadline = Instant::now() + limit;
        let mut output = Vec::new();
        let mut buffer = [0; 8192];
        let mut eof = false;
        loop {
            if stop.is_some_and(Cancellation::requested) {
                return Err("Stopped the owned desktop helper".into());
            }
            if Instant::now() >= deadline {
                return Err(format!("{:?} timed out", cmd.get_program()));
            }

            if !eof {
                let remaining = (cap - output.len() + 1).min(buffer.len());
                match read_ready(&mut stdout, &mut buffer[..remaining]) {
                    Ok(0) => eof = true,
                    Ok(count) if count > cap - output.len() => {
                        output.extend_from_slice(&buffer[..cap - output.len()]);
                        return Ok(Output::Truncated(output));
                    }
                    Ok(count) => {
                        output.extend_from_slice(&buffer[..count]);
                        continue;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(format!("Could not read helper output: {error}")),
                }
            }
            if eof && let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                return Ok(Output::Complete(status.success(), output));
            }
            std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
        }
    })();
    if !matches!(result, Ok(Output::Complete(..))) {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(unix)]
fn prepare_pipe(pipe: &ChildStdout) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let fd = pipe.as_raw_fd();
    // SAFETY: the descriptor is borrowed from a live ChildStdout.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: F_SETFL expects a flag word and does not take ownership.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn read_ready(pipe: &mut ChildStdout, buffer: &mut [u8]) -> io::Result<usize> {
    pipe.read(buffer)
}

#[cfg(windows)]
fn prepare_pipe(_pipe: &ChildStdout) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn read_ready(pipe: &mut ChildStdout, buffer: &mut [u8]) -> io::Result<usize> {
    use std::os::windows::io::AsRawHandle;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED};
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    let mut available = 0;
    // SAFETY: the handle is live and available is the only non-null output.
    if unsafe {
        PeekNamedPipe(
            pipe.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            &mut available,
            null_mut(),
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        return match error.raw_os_error().map(|code| code as u32) {
            Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) => Ok(0),
            _ => Err(error),
        };
    }
    if available == 0 {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    // This is the only reader, so reading at most the peeked count cannot wait
    // for more data from the child.
    let count = buffer.len().min(available as usize);
    pipe.read(&mut buffer[..count])
}

#[cfg(all(test, unix))]
mod capture_limit_tests {
    use super::*;

    #[test]
    fn oversized_helper_output_is_not_a_successful_partial_capture() {
        let mut command = Command::new("head");
        command.args(["-c", &(OUTPUT_CAP + 1).to_string(), "/dev/zero"]);
        assert!(capture(&mut command, Duration::from_secs(5)).is_err());
    }

    #[test]
    fn capture_prefix_stops_unbounded_output_at_the_byte_cap() {
        let mut command = Command::new("yes");
        let (bytes, truncated) =
            capture_prefix(&mut command, Duration::from_secs(2), 1024).unwrap();
        assert_eq!(bytes.len(), 1024);
        assert!(truncated);
        let mut command = Command::new("printf");
        command.arg("exact");
        assert_eq!(
            capture_prefix(&mut command, Duration::from_secs(2), 5).unwrap(),
            (b"exact".to_vec(), false),
        );
    }

    #[test]
    fn complete_helper_output_is_preserved() {
        let mut command = Command::new("printf");
        command.arg("complete");
        assert_eq!(
            capture(&mut command, Duration::from_secs(5)).unwrap(),
            (true, b"complete".to_vec())
        );
    }
}

//! Speech-to-text through the running Hex's own control CLI. `stop --print` returns the
//! transcript on stdout and never pastes it, so Actions/Work input never reaches the
//! focused app.

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Eq, PartialEq)]
pub enum SttError {
    /// Hex exit code 4: the clip held no speech.
    NoSpeech,
    Failed(String),
}

impl std::fmt::Display for SttError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSpeech => f.write_str("No speech was heard"),
            Self::Failed(m) => f.write_str(m),
        }
    }
}

/// What the service needs from a speech engine (faked in tests).
pub trait Stt: Send + Sync {
    fn start(&self) -> Result<(), SttError>;
    /// Finish the recording and return the transcript without pasting it.
    fn stop_print(&self) -> Result<String, SttError>;
    /// Discard the recording; nothing is pasted.
    fn cancel(&self) -> Result<(), SttError>;
}

/// The real engine: the `hex` binary on PATH, talking to the running app.
pub struct HexCli {
    pub exe: String,
}

impl Default for HexCli {
    fn default() -> Self {
        Self { exe: "hex".into() }
    }
}

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

impl HexCli {
    fn run(&self, args: &[&str], timeout: Duration) -> Result<Output, SttError> {
        let mut child = crate::retry_busy(|| {
            Command::new(&self.exe)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        })
        .map_err(|e| {
            SttError::Failed(format!(
                "Hex CLI not found or not runnable ({e}). Install Hex and put `hex` on PATH"
            ))
        })?;
        let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
        let readers = [
            thread::spawn(move || read_all(out.as_mut())),
            thread::spawn(move || read_all(err.as_mut())),
        ];
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(s) = child
                .try_wait()
                .map_err(|e| SttError::Failed(e.to_string()))?
            {
                break s;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SttError::Failed(format!(
                    "hex {} timed out",
                    args.join(" ")
                )));
            }
            thread::sleep(Duration::from_millis(25));
        };
        let [o, e] = readers.map(|h| h.join().unwrap_or_default());
        Ok(Output {
            code: status.code().unwrap_or(-1),
            stdout: o,
            stderr: e,
        })
    }

    fn checked(&self, args: &[&str], timeout: Duration) -> Result<Output, SttError> {
        let o = self.run(args, timeout)?;
        match o.code {
            0 => Ok(o),
            4 => Err(SttError::NoSpeech),
            3 => Err(SttError::Failed(
                "Hex is not running. Start it with `hex start`".into(),
            )),
            c => Err(SttError::Failed(format!(
                "hex {} failed (code {c}): {}",
                args.join(" "),
                o.stderr.trim().chars().take(300).collect::<String>()
            ))),
        }
    }
}

fn read_all(r: Option<&mut impl Read>) -> String {
    let mut s = String::new();
    if let Some(r) = r {
        let _ = r.read_to_string(&mut s);
    }
    s
}

impl Stt for HexCli {
    fn start(&self) -> Result<(), SttError> {
        self.checked(&["dictation", "start", "--raw"], Duration::from_secs(12))
            .map(drop)
    }

    fn stop_print(&self) -> Result<String, SttError> {
        let o = self.checked(&["dictation", "stop", "--print"], Duration::from_secs(75))?;
        Ok(o.stdout.trim().to_string())
    }

    /// `dictation cancel` exists on Hex `dev`; released 3.x rejects it (exit 2). Then
    /// stop with --print and drop the text, which still never pastes.
    fn cancel(&self) -> Result<(), SttError> {
        match self.run(&["dictation", "cancel"], Duration::from_secs(12))? {
            Output { code: 0, .. } | Output { code: 4, .. } => Ok(()),
            Output { code: 2, .. } => match self.stop_print() {
                Ok(_) | Err(SttError::NoSpeech) => Ok(()),
                Err(e) => Err(e),
            },
            o => Err(SttError::Failed(format!(
                "hex dictation cancel failed (code {}): {}",
                o.code,
                o.stderr.trim()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_hex(dir: &std::path::Path, script: &str) -> HexCli {
        let p = dir.join("hex");
        std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        HexCli {
            exe: p.to_string_lossy().into_owned(),
        }
    }

    #[test]
    fn stop_print_returns_the_transcript() {
        let d = tempfile::tempdir().unwrap();
        let h = fake_hex(d.path(), "echo '  open firefox '");
        assert_eq!(h.stop_print().unwrap(), "open firefox");
    }

    #[test]
    fn exit_four_means_no_speech() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            fake_hex(d.path(), "exit 4").stop_print(),
            Err(SttError::NoSpeech)
        );
    }

    #[test]
    fn hex_not_running_is_actionable() {
        let d = tempfile::tempdir().unwrap();
        let e = fake_hex(d.path(), "exit 3")
            .start()
            .unwrap_err()
            .to_string();
        assert!(e.contains("hex start"), "{e}");
    }

    #[test]
    fn cancel_falls_back_when_the_installed_hex_lacks_it() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("log");
        let h = fake_hex(
            d.path(),
            &format!(
                "echo \"$@\" >> {}\nif [ \"$2\" = cancel ]; then exit 2; fi\nexit 4",
                log.display()
            ),
        );
        h.cancel().unwrap();
        let calls = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            ["dictation cancel", "dictation stop --print"]
        );
    }

    #[test]
    fn missing_binary_is_reported() {
        let h = HexCli {
            exe: "/nonexistent/hex".into(),
        };
        assert!(h.start().unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn a_hung_hex_times_out() {
        let d = tempfile::tempdir().unwrap();
        let h = fake_hex(d.path(), "sleep 30");
        let e = h.run(&["x"], Duration::from_millis(300)).err().unwrap();
        assert!(e.to_string().contains("timed out"));
    }
}

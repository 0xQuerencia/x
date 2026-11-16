//! Writing text to the platform clipboard on an explicit "copy this" request.
//! X never reads the clipboard except to confirm what it just wrote, and never sends keys
//! to other apps to copy for it.

#[cfg(not(windows))]
use std::io::Write;
#[cfg(not(windows))]
use std::process::{Command, Stdio};
#[cfg(not(windows))]
use std::time::Duration;

#[cfg(not(windows))]
use crate::desktop::capture;
#[cfg(not(windows))]
use crate::tools::which;

pub const MAX_COPY: usize = 8000;

pub fn validate(text: &str) -> Result<(), String> {
    if text.trim().is_empty() || text.chars().count() > MAX_COPY {
        return Err(format!("Copied text must be 1-{MAX_COPY} characters"));
    }
    if text.contains('\0') {
        return Err("Copied text cannot contain NUL".into());
    }
    Ok(())
}

/// Copies `text`, then reads back once to confirm. The previous clipboard is replaced:
/// that is what "copy" means, and the person asked for it.
#[cfg(windows)]
pub fn copy(text: &str) -> Result<String, String> {
    crate::windows_clipboard::copy(text)
}

#[cfg(not(windows))]
pub fn copy(text: &str) -> Result<String, String> {
    validate(text)?;
    let copier = which("wl-copy").ok_or("wl-copy not found (install wl-clipboard)")?;
    let mut child = Command::new(copier)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not run wl-copy: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("no pipe to wl-copy")?
        .write_all(text.as_bytes())
        .map_err(|e| format!("could not write to wl-copy: {e}"))?;
    // wl-copy stays alive serving the selection; it exits when replaced, so never wait on it.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    let paste = which("wl-paste").ok_or("wl-paste not found, so the copy cannot be confirmed")?;
    for _ in 0..10 {
        let mut cmd = Command::new(&paste);
        cmd.arg("--no-newline");
        if let Ok((true, bytes)) = capture(&mut cmd, Duration::from_secs(2))
            && bytes == text.as_bytes()
        {
            return Ok(format!("Copied {} characters", text.chars().count()));
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    Err("The clipboard did not take the text".into())
}

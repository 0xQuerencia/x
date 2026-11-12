//! User-requested pixel sampling on supported Wayland desktops.
//! Hyprpicker displays the screen sampler; X itself does not read a screenshot.
//! All subprocess arguments are hardcoded and the child is terminated on timeout.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::tools;

use super::colors;

pub(super) fn available() -> bool {
    cfg!(target_os = "linux") && tools::which("hyprpicker").is_some()
}

/// This function may block while the user chooses one pixel. Call only from
/// a worker thread in response to the explicit Pick button.
pub fn pick() -> Result<String, String> {
    if !cfg!(target_os = "linux") {
        return Err(
            "Screen color picking currently requires a supported Linux Wayland session".into(),
        );
    }
    let executable =
        tools::which("hyprpicker").ok_or("Install hyprpicker to enable screen color picking")?;
    if tools::which("wl-copy").is_none() {
        return Err("Install wl-clipboard so the selected color can be copied".into());
    }
    let mut child = Command::new(executable)
        .args(["-a", "-f", "hex", "-q"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not start screen color picker: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(format!(
                        "Color selection was cancelled or failed ({status})"
                    ));
                }
                let mut bytes = Vec::new();
                child
                    .stdout
                    .take()
                    .ok_or("Color picker did not return an output pipe")?
                    .take(256)
                    .read_to_end(&mut bytes)
                    .map_err(|e| format!("Could not read selected color: {e}"))?;
                let output = std::str::from_utf8(&bytes)
                    .map_err(|_| "Color picker returned invalid text")?;
                let formats = colors::convert(output.trim())
                    .ok_or("Color picker returned an invalid color value")?;
                return Ok(format!("Copied picked color: {}", formats.hex));
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Color picker timed out without a selection".into());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Could not monitor color picker: {error}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_offers_supported_platforms_with_the_picker_installed() {
        if !cfg!(target_os = "linux") {
            assert!(!super::available());
        }
    }
}

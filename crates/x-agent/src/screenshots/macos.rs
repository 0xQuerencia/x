//! Explicit local macOS screenshot only, never a background visual observer.
//! The system screencapture utility must be granted Screen Recording by macOS.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::Mode;

const MAX_PNG: usize = 24 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);
const PNG_HEADER: &[u8] = b"\x89PNG\r\n\x1a\n";

struct TemporaryCapture(PathBuf);

impl Drop for TemporaryCapture {
    fn drop(&mut self) {
        // Only the single random X-owned stage path is removed.
        let _ = fs::remove_file(&self.0);
    }
}

fn destination(mode: Mode, explicit: Option<&Path>) -> Result<PathBuf, String> {
    if mode != Mode::Screen {
        return Err("macOS window and region screenshots are not supported yet".into());
    }
    let chosen = match explicit {
        Some(path) => path.to_path_buf(),
        None => {
            let home = dirs::picture_dir()
                .or_else(dirs::home_dir)
                .ok_or("The current user's Pictures directory is unavailable")?;
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "The system clock is invalid")?
                .as_millis();
            home.join(format!(
                "X-screenshot-{timestamp}-{}.png",
                uuid::Uuid::new_v4()
            ))
        }
    };
    if !chosen
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
    {
        return Err("macOS screenshots must use the .png extension".into());
    }
    Ok(chosen)
}

fn require_private_stage() -> Result<PathBuf, String> {
    let root = dirs::data_local_dir()
        .ok_or("X's local application support folder is unavailable")?
        .join("X");
    let dir = root.join("screen-capture");
    fs::create_dir_all(&dir).map_err(|_| "Cannot create the screenshot staging folder")?;
    let meta = fs::symlink_metadata(&dir)
        .map_err(|_| "Cannot inspect the screenshot staging folder")?;
    if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != unsafe { libc::geteuid() } {
        return Err("Refusing a linked or unowned screenshot staging folder".into());
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|_| "Cannot protect the screenshot staging folder")?;
    Ok(dir)
}

fn check_png(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_PNG {
        return Err("macOS screenshot exceeds the 24 MiB safety limit".into());
    }
    if bytes.len() <= PNG_HEADER.len() || !bytes.starts_with(PNG_HEADER) {
        return Err("macOS returned no valid PNG; check Screen Recording permission".into());
    }
    Ok(())
}

fn wait_screencapture(mut child: std::process::Child) -> Result<(), String> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err("macOS refused the screenshot; check Screen Recording permission".into())
                };
            }
            Ok(None) if start.elapsed() >= TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("macOS screenshot timed out; no image was saved".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Could not determine the screenshot command status".into());
            }
        }
    }
}

pub(super) fn save(mode: Mode, explicit: Option<&Path>) -> Result<PathBuf, String> {
    let output = destination(mode, explicit)?;
    let stage = TemporaryCapture(
        require_private_stage()?.join(format!("capture-{}.png", uuid::Uuid::new_v4())),
    );
    let child = Command::new("/usr/sbin/screencapture")
        .args(["-x", "-m", "-t", "png"])
        .arg(&stage.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "macOS screencapture is unavailable")?;
    wait_screencapture(child)?;
    let info = fs::symlink_metadata(&stage.0)
        .map_err(|_| "macOS did not write the screenshot")?;
    if !info.file_type().is_file() || info.file_type().is_symlink()
        || info.uid() != unsafe { libc::geteuid() }
        || info.len() > MAX_PNG as u64
    {
        return Err("macOS screenshot file is unsafe or too large".into());
    }
    let png = fs::read(&stage.0).map_err(|_| "Cannot read the local screenshot")?;
    check_png(&png)?;
    // create_new refuses existing files and symbolic links; never overwrite.
    let mut dest: File = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&output)
        .map_err(|_| "Screenshot destination exists or cannot be created")?;
    let written = dest.write_all(&png).and_then(|_| dest.sync_all());
    drop(dest);
    if written.is_err() {
        let _ = fs::remove_file(&output);
        return Err("Could not save the local screenshot".into());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_path_scope_and_content_are_checked() {
        assert_eq!(
            destination(Mode::Screen, Some(Path::new("capture.png"))).unwrap(),
            PathBuf::from("capture.png")
        );
        assert!(destination(Mode::Window, None).is_err());
        assert!(destination(Mode::Region, None).is_err());
        assert!(destination(Mode::Screen, Some(Path::new("capture.jpg"))).is_err());
        assert!(check_png(b"not an image").is_err());
        assert!(check_png(PNG_HEADER).is_err());
        let mut pixels = PNG_HEADER.to_vec();
        pixels.extend_from_slice(b"image payload");
        assert!(check_png(&pixels).is_ok());
    }

    #[test]
    fn private_temporary_capture_removes_only_its_file() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("capture.png");
        fs::write(&file, PNG_HEADER).unwrap();
        let sibling = temp.path().join("keep.txt");
        fs::write(&sibling, b"private").unwrap();
        {
            let _stage = TemporaryCapture(file.clone());
        }
        assert!(!file.exists());
        assert!(sibling.exists());
    }
}

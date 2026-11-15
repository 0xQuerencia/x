//! On-demand Wayland screenshot capture. Private vision captures stay in memory;
//! CLI-requested captures are written only to a user-selected or Pictures path.
//! No hooks, polling, uploads or persistent screenshot history.
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
const MAX_PNG: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Screen,
    Window,
    Region,
}

impl Mode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "screen" => Ok(Self::Screen),
            "window" => Ok(Self::Window),
            "region" => Ok(Self::Region),
            _ => Err("Screenshot mode must be screen, window, or region".into()),
        }
    }
}

/// Private, in-memory image for one explicitly granted desktop-app decision.
/// Does not access the clipboard or write a temporary file.
pub fn capture_for_decider(app: &str, observed_title: &str) -> Result<Vec<u8>, String> {
    #[cfg(target_os = "linux")]
    {
        let before = active_window()?;
        let class = before
            .get("class")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let names = crate::tools::app_classes(app)
            .ok_or("Visual capture is restricted to a registered X app")?;
        if !names.iter().any(|name| name.eq_ignore_ascii_case(class))
            || (!observed_title.is_empty()
                && before.get("title").and_then(serde_json::Value::as_str) != Some(observed_title))
        {
            return Err(
                "Focused window changed or is not the granted app; no screenshot taken".into(),
            );
        }
        let address = before
            .get("address")
            .and_then(serde_json::Value::as_str)
            .ok_or("Focused window lacks an address")?
            .to_owned();
        let image = capture(Mode::Window)?;
        let after = active_window()?;
        if after.get("address").and_then(serde_json::Value::as_str) != Some(address.as_str()) {
            return Err("Focus changed during capture; screenshot discarded".into());
        }
        Ok(image)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (app, observed_title);
        Err("Visual context currently requires Hyprland on Linux".into())
    }
}

/// Return a PNG without leaving a private screenshot on disk.
pub fn capture(mode: Mode) -> Result<Vec<u8>, String> {
    #[cfg(target_os = "linux")]
    {
        use std::io::Read;
        use std::process::{Command, Stdio};
        let geometry = match mode {
            Mode::Screen => None,
            Mode::Window => Some(window_geometry(&active_window()?)?),
            Mode::Region => Some(region_geometry()?),
        };
        let mut command = Command::new("grim");
        if let Some(ref rect) = geometry {
            command.args(["-g", rect]);
        }
        let mut child = command
            .arg("-")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "grim is required for screenshots (pacman -S grim)".to_string())?;
        let mut png = Vec::new();
        let result = child
            .stdout
            .take()
            .ok_or("grim returned no image stream")?
            .take((MAX_PNG + 1) as u64)
            .read_to_end(&mut png);
        if result.is_err() || png.len() > MAX_PNG {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Screenshot exceeded the 4 MiB safety limit".into());
        }
        let status = child.wait().map_err(|_| "grim could not finish capture")?;
        if !status.success() || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("grim failed to produce a PNG".into());
        }
        Ok(png)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = mode;
        Err("Screenshots currently require Wayland on Linux".into())
    }
}

#[cfg(target_os = "linux")]
fn active_window() -> Result<serde_json::Value, String> {
    crate::desktop::hypr(&["activewindow", "-j"])
}

#[cfg(target_os = "linux")]
fn window_geometry(window: &serde_json::Value) -> Result<String, String> {
    let pos = window
        .get("at")
        .and_then(serde_json::Value::as_array)
        .ok_or("Window has no position")?;
    let size = window
        .get("size")
        .and_then(serde_json::Value::as_array)
        .ok_or("Window has no dimensions")?;
    let (x, y, w, h) = (
        pos.first().and_then(serde_json::Value::as_i64),
        pos.get(1).and_then(serde_json::Value::as_i64),
        size.first().and_then(serde_json::Value::as_i64),
        size.get(1).and_then(serde_json::Value::as_i64),
    );
    match (x, y, w, h) {
        (Some(x), Some(y), Some(w), Some(h))
            if (1..=7680).contains(&w) && (1..=4320).contains(&h) =>
        {
            Ok(format!("{x},{y} {w}x{h}"))
        }
        _ => Err("Focused window has invalid capture bounds".into()),
    }
}

#[cfg(target_os = "linux")]
fn region_geometry() -> Result<String, String> {
    use std::process::{Command, Stdio};
    let out = Command::new("slurp")
        .args(["-f", "%x,%y %wx%h"])
        .stdin(Stdio::null())
        .output()
        .map_err(|_| "slurp is required for region selection (pacman -S slurp)".to_string())?;
    if !out.status.success() {
        return Err("Region selection cancelled".into());
    }
    let region = std::str::from_utf8(&out.stdout)
        .map_err(|_| "slurp returned invalid coordinates")?
        .trim();
    let (location, dims) = region.split_once(' ').ok_or("Invalid region geometry")?;
    let (x, y) = location
        .split_once(',')
        .ok_or("Invalid region coordinates")?;
    let (w, h) = dims.split_once('x').ok_or("Invalid region dimensions")?;
    let _ = x.parse::<i64>().map_err(|_| "Bad x coordinate")?;
    let _ = y.parse::<i64>().map_err(|_| "Bad y coordinate")?;
    let w = w.parse::<u32>().map_err(|_| "Bad region width")?;
    let h = h.parse::<u32>().map_err(|_| "Bad region height")?;
    if !(1..=7680).contains(&w) || !(1..=4320).contains(&h) {
        return Err("Region capture is outside the supported size limit".into());
    }
    Ok(region.to_owned())
}

/// Explicitly save a user-requested screenshot, never a model's private image.
pub fn save(mode: Mode, output: Option<&Path>) -> Result<PathBuf, String> {
    let png = capture(mode)?;
    let path = if let Some(path) = output {
        path.to_path_buf()
    } else {
        let name = format!(
            "X-screenshot-{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| "System clock is not set")?
                .as_millis()
        );
        dirs::picture_dir()
            .or_else(dirs::home_dir)
            .ok_or("Cannot find Pictures directory")?
            .join(name)
    };
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    let mut file = opts
        .open(&path)
        .map_err(|e| format!("Cannot create screenshot {}: {e}", path.display()))?;
    if let Err(error) = file.write_all(&png) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(format!("Cannot save screenshot: {error}"));
    }
    Ok(path)
}

/// No dependency, clipboard access, temp file or external process for encoding.
pub fn base64_png(png: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(png.len().div_ceil(3) * 4);
    for chunk in png.chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        out.push(ALPHABET[(a >> 2) as usize] as char);
        out.push(ALPHABET[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_padding_and_modes() {
        assert_eq!(base64_png(b""), "");
        assert_eq!(base64_png(b"M"), "TQ==");
        assert_eq!(base64_png(b"Ma"), "TWE=");
        assert_eq!(base64_png(b"Man"), "TWFu");
        assert!(Mode::parse("region").is_ok());
        assert!(Mode::parse("auto").is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_unbounded_window_geometry() {
        assert!(window_geometry(&serde_json::json!({"at":[0,0],"size":[9000,100]})).is_err());
        assert!(window_geometry(&serde_json::json!({"at":[-100,4],"size":[400,200]})).is_ok());
    }
}

//! Explicit desktop screenshot capture. Private Hyprland vision stays in memory;
//! CLI-requested captures are written only to a user-selected or Pictures path.
//! Windows screen captures are BMP; macOS uses a private temporary PNG.
//! Both refuse unsupported window/region requests rather than broadening scope.
//! No hooks, polling, uploads or persistent screenshot history.
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
const MAX_PNG: usize = 4 * 1024 * 1024;

#[cfg(windows)]
mod win;

#[cfg(target_os = "macos")]
mod macos;

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
        // A rectangular grim grab can include pixels from another window or a
        // top/overlay layer. Refuse rather than send content outside this grant.
        refuse_overlaps(&before)?;
        let image = capture(Mode::Window)?;
        let after = active_window()?;
        if after.get("address").and_then(serde_json::Value::as_str) != Some(address.as_str()) {
            return Err("Focus changed during capture; screenshot discarded".into());
        }
        refuse_overlaps(&after)?;
        Ok(image)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (app, observed_title);
        Err("Visual context currently requires Hyprland on Linux".into())
    }
}

/// Return a local screenshot image without leaving it on disk.
/// Linux returns PNG; explicit Windows full-screen capture returns BMP.
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
    #[cfg(windows)]
    {
        // Windows captures are local BMP bytes. This path is never used by
        // X's automatic visual Decider, which remains Linux/Hyprland-only.
        match mode {
            Mode::Screen => win::capture_screen_bmp(),
            Mode::Window | Mode::Region => {
                Err("Windows window/region screenshot capture is not supported yet".into())
            }
        }
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = mode;
        Err("Screenshots currently require Wayland on Linux".into())
    }
}

/// The visual grant is for one app, never for other visible surfaces sharing
/// its rectangular screenshot. A malformed compositor response fails closed.
#[cfg(target_os = "linux")]
fn refuse_overlaps(window: &serde_json::Value) -> Result<(), String> {
    let clients = crate::desktop::hypr(&["clients", "-j"])?;
    let layers = crate::desktop::hypr(&["layers", "-j"])?;
    check_overlaps(window, &clients, &layers)
}

#[cfg(target_os = "linux")]
fn rectangle(value: &serde_json::Value) -> Result<(i64, i64, i64, i64), String> {
    let pos = value
        .get("at")
        .and_then(serde_json::Value::as_array)
        .ok_or("Visual capture has unknown surface position")?;
    let size = value
        .get("size")
        .and_then(serde_json::Value::as_array)
        .ok_or("Visual capture has unknown surface size")?;
    let (x, y, w, h) = (
        pos.first().and_then(serde_json::Value::as_i64),
        pos.get(1).and_then(serde_json::Value::as_i64),
        size.first().and_then(serde_json::Value::as_i64),
        size.get(1).and_then(serde_json::Value::as_i64),
    );
    match (x, y, w, h) {
        (Some(x), Some(y), Some(w), Some(h)) if w > 0 && h > 0 => Ok((x, y, w, h)),
        _ => Err("Visual capture has invalid surface geometry".into()),
    }
}

/// Hyprland uses `x/y/w/h` for layer surfaces, unlike the
/// `at/size` arrays emitted for normal client windows.
#[cfg(target_os = "linux")]
fn layer_rectangle(value: &serde_json::Value) -> Result<(i64, i64, i64, i64), String> {
    let values = ["x", "y", "w", "h"].map(|key| {
        value
            .get(key)
            .and_then(serde_json::Value::as_i64)
            .ok_or("Visual capture has unknown layer geometry")
    });
    let [x, y, w, h] = values;
    let (x, y, w, h) = (x?, y?, w?, h?);
    if w <= 0 || h <= 0 {
        return Err("Visual capture has invalid layer geometry".into());
    }
    Ok((x, y, w, h))
}

#[cfg(target_os = "linux")]
fn intersects(a: (i64, i64, i64, i64), b: (i64, i64, i64, i64)) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

#[cfg(target_os = "linux")]
fn check_overlaps(
    window: &serde_json::Value,
    clients: &serde_json::Value,
    layers: &serde_json::Value,
) -> Result<(), String> {
    let rect = rectangle(window)?;
    let address = window
        .get("address")
        .and_then(serde_json::Value::as_str)
        .ok_or("Focused window has no identity")?;
    let workspace = window
        .pointer("/workspace/id")
        .and_then(serde_json::Value::as_i64)
        .ok_or("Focused window has no workspace")?;
    for other in clients
        .as_array()
        .ok_or("Cannot inspect other windows before private capture")?
    {
        if other.get("address").and_then(serde_json::Value::as_str) == Some(address)
            || other.get("mapped").and_then(serde_json::Value::as_bool) == Some(false)
            || (other
                .pointer("/workspace/id")
                .and_then(serde_json::Value::as_i64)
                != Some(workspace)
                && other.get("pinned").and_then(serde_json::Value::as_bool) != Some(true))
        {
            continue;
        }
        if intersects(rect, rectangle(other)?) {
            return Err("Another window overlaps the granted app; no screenshot taken".into());
        }
    }

    // Layers 0/1 are wallpaper/bottom surfaces. Levels 2/3 are top/overlay
    // surfaces, including notification popups and panels.
    for monitor in layers
        .as_object()
        .ok_or("Cannot inspect overlay layers before private capture")?
        .values()
    {
        let levels = monitor
            .get("levels")
            .and_then(serde_json::Value::as_object)
            .ok_or("Cannot inspect overlay levels before private capture")?;
        for level in ["2", "3"] {
            if let Some(surfaces) = levels.get(level) {
                for surface in surfaces
                    .as_array()
                    .ok_or("Cannot inspect overlay surfaces before private capture")?
                {
                    if intersects(rect, layer_rectangle(surface)?) {
                        return Err(
                            "A panel or notification overlaps the granted app; no screenshot taken"
                                .into(),
                        );
                    }
                }
            }
        }
    }
    Ok(())
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
    #[cfg(target_os = "macos")]
    {
        return macos::save(mode, output);
    }
    #[cfg(not(target_os = "macos"))]
    {
    #[cfg(windows)]
    if output.is_some_and(|path| {
        !path
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|ext| ext.eq_ignore_ascii_case("bmp"))
    }) {
        return Err("Windows screenshots must be saved with a .bmp extension".into());
    }
    let png = capture(mode)?;
    let path = if let Some(path) = output {
        path.to_path_buf()
    } else {
        let extension = if cfg!(windows) { "bmp" } else { "png" };
        let name = format!(
            "X-screenshot-{}.{extension}",
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
    fn private_visual_capture_refuses_another_mapped_window() {
        let focused = serde_json::json!({
            "address":"0x1", "at":[10,10], "size":[600,400], "workspace":{"id":1}
        });
        let own = serde_json::json!({
            "address":"0x1", "at":[10,10], "size":[600,400],
            "workspace":{"id":1}, "mapped":true
        });
        let overlap = serde_json::json!({
            "address":"0x2", "at":[600,50], "size":[100,100],
            "workspace":{"id":1}, "mapped":true
        });
        let other_workspace = serde_json::json!({
            "address":"0x3", "at":[10,10], "size":[100,100],
            "workspace":{"id":2}, "mapped":true
        });
        let layers = serde_json::json!({
            "DP-1":{"levels":{"0":[{"at":[0,0],"size":[2000,1200]}],"2":[],"3":[]}}
        });
        assert!(
            check_overlaps(
                &focused,
                &serde_json::json!([own, other_workspace]),
                &layers
            )
            .is_ok()
        );
        assert!(
            check_overlaps(&focused, &serde_json::json!([overlap]), &layers)
                .unwrap_err()
                .contains("Another window")
        );
        assert!(check_overlaps(&focused, &serde_json::json!([]), &serde_json::json!({})).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn private_visual_capture_recognizes_real_hyprctl_layers_geometry() {
        let window = serde_json::json!({
            "address":"0x1", "at":[400,300], "size":[700,500], "workspace":{"id":1}
        });
        let clear = serde_json::json!({
            "DP-1":{"levels":{
                "0":[{"x":0,"y":0,"w":2000,"h":1200}],
                "2":[{"x":0,"y":0,"w":2000,"h":20}],
                "3":[]
            }}
        });
        assert!(check_overlaps(&window, &serde_json::json!([]), &clear).is_ok());
        let obstructed = serde_json::json!({
            "DP-1":{"levels":{
                "2":[{"x":450,"y":310,"w":800,"h":75}]
            }}
        });
        assert!(
            check_overlaps(&window, &serde_json::json!([]), &obstructed)
                .unwrap_err()
                .contains("notification")
        );
        assert!(layer_rectangle(&serde_json::json!({"x":10,"y":10,"w":0,"h":40})).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn private_visual_capture_refuses_overlays_and_unknown_geometry() {
        let focused = serde_json::json!({
            "address":"0x1", "at":[10,10], "size":[600,400], "workspace":{"id":1}
        });
        // Actual `hyprctl layers -j` fields are x/y/w/h, not at/size.
        let overlay = serde_json::json!({
            "DP-1":{"levels":{"2":[{"x":20,"y":20,"w":150,"h":50}],"3":[]}}
        });
        assert!(
            check_overlaps(&focused, &serde_json::json!([]), &overlay)
                .unwrap_err()
                .contains("panel or notification")
        );
        assert!(
            check_overlaps(
                &focused,
                &serde_json::json!([]),
                &serde_json::json!({"DP-1":{"levels":{"3":[{"x":0,"y":0,"w":500}]}}})
            )
            .is_err()
        );
        assert!(check_overlaps(&focused, &serde_json::json!({}), &overlay).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_unbounded_window_geometry() {
        assert!(window_geometry(&serde_json::json!({"at":[0,0],"size":[9000,100]})).is_err());
        assert!(window_geometry(&serde_json::json!({"at":[-100,4],"size":[400,200]})).is_ok());
    }
}

//! Windows 11 window control using the documented user32 APIs.
//!
//! There is no shell, PowerShell, title-derived command, or synthetic desktop
//! numbering. Windows virtual desktops lack a stable public ordinal/switch
//! interface, so workspace operations remain explicitly unsupported.
//! Window IDs carry the observed HWND and PID; a reused handle for a different
//! process cannot silently close the wrong window.

use std::ffi::c_void;

use super::{Backend, Cap, Cmd, Place, WinInfo, WmError};

type Hwnd = *mut c_void;
type Handle = *mut c_void;

const GW_OWNER: u32 = 4;
const GWL_EXSTYLE: i32 = -20;
const WS_EX_TOOLWINDOW: isize = 0x0000_0080;
const SW_MINIMIZE: i32 = 6;
const SW_MAXIMIZE: i32 = 3;
const SW_RESTORE: i32 = 9;
const WM_CLOSE: u32 = 0x0010;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const MAX_WINDOWS: usize = 256;

#[link(name = "user32")]
unsafe extern "system" {
    fn EnumWindows(
        callback: Option<unsafe extern "system" fn(Hwnd, isize) -> i32>,
        data: isize,
    ) -> i32;
    fn GetForegroundWindow() -> Hwnd;
    fn GetWindowThreadProcessId(window: Hwnd, process: *mut u32) -> u32;
    fn GetWindowTextW(window: Hwnd, text: *mut u16, capacity: i32) -> i32;
    fn GetClassNameW(window: Hwnd, text: *mut u16, capacity: i32) -> i32;
    fn GetWindow(window: Hwnd, command: u32) -> Hwnd;
    fn GetWindowLongPtrW(window: Hwnd, index: i32) -> isize;
    fn IsWindow(window: Hwnd) -> i32;
    fn IsWindowVisible(window: Hwnd) -> i32;
    fn IsIconic(window: Hwnd) -> i32;
    fn IsZoomed(window: Hwnd) -> i32;
    fn ShowWindow(window: Hwnd, command: i32) -> i32;
    fn SetForegroundWindow(window: Hwnd) -> i32;
    fn PostMessageW(window: Hwnd, message: u32, wparam: usize, lparam: isize) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn QueryFullProcessImageNameW(
        process: Handle,
        flags: u32,
        filename: *mut u16,
        length: *mut u32,
    ) -> i32;
}

pub struct Win11;

fn window_id(window: Hwnd, pid: u32) -> String {
    format!("win:{:x}:{pid}", window as usize)
}

/// Strictly parse only handles previously observed in the form win:HWND:PID.
/// The PID is checked again immediately before each mutating action.
pub(super) fn parse_id(id: &str) -> Option<(usize, u32)> {
    let parts: Vec<&str> = id.split(':').collect();
    if parts.len() != 3 || parts[0] != "win" {
        return None;
    }
    let raw = parts[1];
    if raw.is_empty() || raw.len() > 16 || !raw.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let handle = usize::from_str_radix(raw, 16).ok().filter(|h| *h != 0)?;
    let pid = parts[2].parse::<u32>().ok().filter(|pid| *pid != 0)?;
    Some((handle, pid))
}

fn process_id(window: Hwnd) -> u32 {
    let mut pid = 0;
    // SAFETY: user32 validates the opaque HWND. A zero PID is rejected.
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    pid
}

fn text(window: Hwnd, class: bool) -> String {
    let mut buf = [0u16; 512];
    // SAFETY: user32 receives an owned, bounded UTF-16 output buffer.
    let n = unsafe {
        if class {
            GetClassNameW(window, buf.as_mut_ptr(), buf.len() as i32)
        } else {
            GetWindowTextW(window, buf.as_mut_ptr(), buf.len() as i32)
        }
    };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn process_name(pid: u32) -> Option<String> {
    // SAFETY: the handle is closed on all paths. No process memory is read.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(process);
        if ok == 0 || len == 0 || len as usize > buf.len() {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let name = path.rsplit(['\\', '/']).next()?.to_ascii_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        (!name.is_empty() && name.len() <= 128).then(|| name.to_owned())
    }
}

fn inspect(window: Hwnd) -> Option<WinInfo> {
    // SAFETY: only top-level, visible, non-tool windows with a valid owner
    // process are exposed. UI captions are never passed to commands.
    unsafe {
        if IsWindow(window) == 0
            || IsWindowVisible(window) == 0
            || !GetWindow(window, GW_OWNER).is_null()
            || GetWindowLongPtrW(window, GWL_EXSTYLE) & WS_EX_TOOLWINDOW != 0
        {
            return None;
        }
    }
    let title = text(window, false);
    if title.trim().is_empty() {
        return None;
    }
    let pid = process_id(window);
    if pid == 0 {
        return None;
    }
    // Process executable identity gives a stable user-friendly target for
    // browser/editor commands; native class remains a bounded fallback.
    let class = process_name(pid).unwrap_or_else(|| text(window, true).to_ascii_lowercase());
    if class.is_empty() {
        return None;
    }
    let minimized = unsafe { IsIconic(window) != 0 };
    let maximized = unsafe { IsZoomed(window) != 0 };
    Some(WinInfo {
        id: window_id(window, pid),
        pid,
        class,
        title: title.chars().take(512).collect(),
        // The Windows virtual-desktop API does not expose stable numbered
        // positions. Zero is explicitly UNKNOWN, never desktop 1.
        workspace: 0,
        place: Place::N(0),
        minimized,
        maximized,
        recent: u32::MAX,
    })
}

unsafe extern "system" fn enumerate(window: Hwnd, data: isize) -> i32 {
    // SAFETY: EnumWindows invokes this synchronously while the caller's
    // Vec remains in scope; no user-controlled pointer enters this callback.
    let windows = unsafe { &mut *(data as *mut Vec<WinInfo>) };
    if windows.len() < MAX_WINDOWS
        && let Some(win) = inspect(window)
    {
        windows.push(win);
    }
    1
}

fn live_target(id: &str) -> Result<Hwnd, WmError> {
    let (handle, expected_pid) =
        parse_id(id).ok_or_else(|| WmError::Backend("Invalid Windows window identity".into()))?;
    let window = handle as Hwnd;
    // Revalidate existence, process identity, and app-window classification
    // before each action; stale/reused HWND values are refused.
    let observed = inspect(window).ok_or_else(|| WmError::NotFound("Window disappeared".into()))?;
    if observed.pid != expected_pid || observed.id != id {
        return Err(WmError::NotFound(
            "Window identity changed; repeat the command".into(),
        ));
    }
    Ok(window)
}

impl Backend for Win11 {
    fn name(&self) -> &'static str {
        "Windows 11 (Win32)"
    }

    fn caps(&self) -> Vec<Cap> {
        vec![
            Cap::List,
            Cap::Focus,
            Cap::Maximize,
            Cap::Minimize,
            Cap::Close,
        ]
    }

    fn windows(&self) -> Result<Vec<WinInfo>, WmError> {
        let mut out = Vec::new();
        // SAFETY: valid callback and borrowed Vec, used synchronously.
        let ok = unsafe { EnumWindows(Some(enumerate), &mut out as *mut _ as isize) };
        if ok == 0 {
            return Err(WmError::Backend(
                "Windows refused window enumeration".into(),
            ));
        }
        Ok(out)
    }

    fn focused(&self) -> Result<Option<String>, WmError> {
        // SAFETY: user32 returns null when the desktop has no foreground window.
        let window = unsafe { GetForegroundWindow() };
        Ok((!window.is_null())
            .then(|| inspect(window).map(|win| win.id))
            .flatten())
    }

    fn workspace(&self) -> Result<i64, WmError> {
        Err(WmError::Unsupported(
            "Windows 11 virtual desktop numbers are not yet supported".into(),
        ))
    }

    fn run(&self, cmd: &Cmd) -> Result<(), WmError> {
        match cmd {
            Cmd::Workspace(_) | Cmd::Move { .. } => Err(WmError::Unsupported(
                "Windows 11 virtual desktop switching and moves are not available".into(),
            )),
            Cmd::Focus(id) => {
                let window = live_target(id)?;
                // Restore an iconic window before giving it focus; Windows
                // decides whether the foreground-stealing policy permits this.
                if unsafe { IsIconic(window) != 0 } {
                    unsafe { ShowWindow(window, SW_RESTORE) };
                }
                if unsafe { SetForegroundWindow(window) } == 0 {
                    return Err(WmError::Backend(
                        "Windows denied foreground focus; activate X and retry".into(),
                    ));
                }
                Ok(())
            }
            Cmd::Maximize { win, on } => {
                let window = live_target(win)?;
                unsafe { ShowWindow(window, if *on { SW_MAXIMIZE } else { SW_RESTORE }) };
                Ok(()) // The shared controller verifies fresh window state.
            }
            Cmd::Minimize { win } => {
                let window = live_target(win)?;
                unsafe { ShowWindow(window, SW_MINIMIZE) };
                Ok(())
            }
            Cmd::Close(id) => {
                let window = live_target(id)?;
                if unsafe { PostMessageW(window, WM_CLOSE, 0, 0) } == 0 {
                    return Err(WmError::Backend(
                        "Windows refused a graceful close request".into(),
                    ));
                }
                Ok(()) // No termination; the app may prompt to save its work.
            }
        }
    }

    fn restore_minimized(&self, win: &WinInfo) -> Result<(), WmError> {
        let window = live_target(&win.id)?;
        unsafe { ShowWindow(window, SW_RESTORE) };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_handles_cannot_be_injected_or_rebound_to_another_process() {
        for bad in [
            "",
            "win:0:1",
            "win:123:0",
            "win:123",
            "win:123:4:5",
            "win:123:7;echo",
            "win:-12:4",
            "win:0x12:4",
            "win:12345678901234567:4",
            "win:12:4294967296",
        ] {
            assert!(parse_id(bad).is_none(), "{bad}");
        }
        assert_eq!(parse_id("win:7ffabcdef:1024"), Some((0x7ffabcdef, 1024)));
        assert_eq!(parse_id("win:0001:42"), Some((1, 42)));
    }

    #[test]
    fn win32_refuses_undocumented_virtual_desktop_actions() {
        let backend = Win11;
        assert!(!backend.caps().contains(&Cap::SwitchWorkspace));
        assert!(!backend.caps().contains(&Cap::MoveToWorkspace));
        assert!(matches!(backend.workspace(), Err(WmError::Unsupported(_))));
        assert!(matches!(
            backend.run(&Cmd::Workspace(2)),
            Err(WmError::Unsupported(_))
        ));
    }
}

//! EWMH window management for supported X11 window managers.
//!
//! wmctrl and xprop communicate with the session's EWMH/NetWM implementation.
//! IDs only come from the current managed-window list and are strictly parsed
//! before being placed in argv. Window titles are never command arguments.

use std::process::{Command, Stdio};

use super::{Backend, Cap, Cmd, Place, WinInfo, WmError};

pub struct X11;

const MAX_OUTPUT: usize = 256 * 1024;
const MAX_WINDOWS: usize = 150;

fn output(program: &str, args: &[&str]) -> Result<String, WmError> {
    let value = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| WmError::Backend(format!("{program} is unavailable: {e}")))?;
    if !value.status.success() {
        return Err(WmError::Backend(format!(
            "{program} returned a failed X11 operation"
        )));
    }
    if value.stdout.len() > MAX_OUTPUT {
        return Err(WmError::Backend(
            "X11 window listing exceeded its size limit".into(),
        ));
    }
    String::from_utf8(value.stdout)
        .map_err(|_| WmError::Backend("X11 window response is not UTF-8".into()))
}

fn window_id(value: &str) -> Result<String, WmError> {
    let digits = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or_else(|| WmError::Backend("Invalid X11 window id".into()))?;
    if digits.is_empty() || digits.len() > 8 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(WmError::Backend("Invalid X11 window id".into()));
    }
    let number = u32::from_str_radix(digits, 16)
        .map_err(|_| WmError::Backend("Invalid X11 window id".into()))?;
    if number == 0 {
        return Err(WmError::Backend("No active X11 window".into()));
    }
    Ok(format!("0x{number:08x}"))
}

fn parse_windows(text: &str) -> Vec<WinInfo> {
    text.lines()
        .take(MAX_WINDOWS)
        .filter_map(|line| {
            // wmctrl -l -G -x -p: id desk pid x y w h instance.class host title.
            // Refuse windows missing a trustworthy process identity.
            let mut cols = line.split_whitespace();
            let id = window_id(cols.next()?).ok()?;
            let desk = cols.next()?.parse::<i64>().ok()?;
            let pid = cols.next()?.parse::<u32>().ok().filter(|pid| *pid != 0)?;
            for _ in 0..4 {
                cols.next()?.parse::<i64>().ok()?;
            }
            let class = cols.next()?;
            if class.len() > 256 || class == "N/A" {
                return None;
            }
            let host = cols.next()?;
            if host.len() > 256 {
                return None;
            }
            // Skip windows pinned on all desktops: an exact Undo location
            // cannot be recovered as a numbered workspace.
            if !(0..10).contains(&desk) {
                return None;
            }
            let class = class.rsplit('.').next()?.to_ascii_lowercase();
            if class.is_empty() {
                return None;
            }
            let title: String = cols.collect::<Vec<_>>().join(" ");
            let workspace = desk + 1;
            Some(WinInfo {
                id,
                pid,
                class,
                title,
                workspace,
                place: Place::N(workspace),
                minimized: false,
                maximized: false,
                recent: u32::MAX,
            })
        })
        .collect()
}

fn property_state(output: &str) -> (bool, bool) {
    let maximized = output.contains("_NET_WM_STATE_MAXIMIZED_VERT")
        && output.contains("_NET_WM_STATE_MAXIMIZED_HORZ");
    let minimized = output.contains("_NET_WM_STATE_HIDDEN");
    (minimized, maximized)
}

fn parse_active(text: &str) -> Option<String> {
    let (_, value) = text.split_once('#')?;
    window_id(value.trim()).ok()
}

fn parse_workspace(text: &str) -> Option<i64> {
    text.lines().find_map(|line| {
        let mut cols = line.split_whitespace();
        let desk = cols.next()?.parse::<i64>().ok()?;
        (cols.next()? == "*" && (0..10).contains(&desk)).then_some(desk + 1)
    })
}

impl X11 {
    fn command(args: &[&str]) -> Result<(), WmError> {
        output("wmctrl", args).map(|_| ())
    }

    fn state(id: &str) -> Result<(bool, bool), WmError> {
        let reply = output("xprop", &["-id", id, "_NET_WM_STATE"])?;
        Ok(property_state(&reply))
    }
}

impl Backend for X11 {
    fn name(&self) -> &'static str {
        "X11 EWMH"
    }

    fn caps(&self) -> Vec<Cap> {
        let mut caps = vec![
            Cap::List,
            Cap::Focus,
            Cap::SwitchWorkspace,
            Cap::MoveToWorkspace,
            Cap::Maximize,
            Cap::Close,
        ];
        if crate::tools::which("xdotool").is_some() {
            caps.push(Cap::Minimize);
        }
        caps
    }

    fn windows(&self) -> Result<Vec<WinInfo>, WmError> {
        let listing = output("wmctrl", &["-l", "-G", "-x", "-p"])?;
        let mut windows = parse_windows(&listing);
        // A window can vanish between commands. Exclude it instead of
        // treating an uncertain state as verified.
        windows.retain_mut(|win| match Self::state(&win.id) {
            Ok((minimized, maximized)) => {
                win.minimized = minimized;
                win.maximized = maximized;
                true
            }
            Err(_) => false,
        });
        Ok(windows)
    }

    fn focused(&self) -> Result<Option<String>, WmError> {
        Ok(parse_active(&output(
            "xprop",
            &["-root", "_NET_ACTIVE_WINDOW"],
        )?))
    }

    fn workspace(&self) -> Result<i64, WmError> {
        parse_workspace(&output("wmctrl", &["-d"])?)
            .ok_or_else(|| WmError::Backend("Cannot read current X11 workspace".into()))
    }

    fn run(&self, cmd: &Cmd) -> Result<(), WmError> {
        match cmd {
            Cmd::Focus(id) => Self::command(&["-i", "-a", &window_id(id)?]),
            Cmd::Workspace(ws) if (1..=10).contains(ws) => {
                Self::command(&["-s", &(ws - 1).to_string()])
            }
            Cmd::Workspace(_) => Err(WmError::Backend("Workspace must be 1-10".into())),
            Cmd::Move {
                win,
                to: Place::N(ws),
            } if (1..=10).contains(ws) => {
                Self::command(&["-i", "-r", &window_id(win)?, "-t", &(ws - 1).to_string()])
            }
            Cmd::Move { .. } => Err(WmError::Backend(
                "Only numbered X11 workspaces are supported".into(),
            )),
            Cmd::Maximize { win, on } => Self::command(&[
                "-i",
                "-r",
                &window_id(win)?,
                "-b",
                if *on {
                    "add,maximized_vert,maximized_horz"
                } else {
                    "remove,maximized_vert,maximized_horz"
                },
            ]),
            Cmd::Minimize { win } => {
                if crate::tools::which("xdotool").is_none() {
                    return Err(WmError::Unsupported(
                        "xdotool is required for X11 minimize".into(),
                    ));
                }
                let id = window_id(win)?;
                let num = u32::from_str_radix(&id[2..], 16)
                    .map_err(|_| WmError::Backend("Bad X11 window identity".into()))?;
                output("xdotool", &["windowminimize", &num.to_string()]).map(|_| ())
            }
            Cmd::Close(id) => Self::command(&["-i", "-c", &window_id(id)?]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This is run only inside a disposable Xvfb + Openbox session in CI.
    /// No other window is touched or closed.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires an isolated Xvfb display, Openbox, wmctrl, xprop and xterm"]
    fn live_x11_ewmh() {
        use std::time::Duration;
        let mut child = Command::new("xterm")
            .args([
                "-class",
                "XWMLive",
                "-title",
                "XWMLiveTest",
                "-e",
                "sleep",
                "90",
            ])
            .stdin(Stdio::null())
            .spawn()
            .expect("xterm available in isolated test session");
        let backend = X11;
        let own = || {
            backend
                .windows()
                .unwrap()
                .into_iter()
                .find(|w| w.title == "XWMLiveTest")
        };
        for _ in 0..50 {
            if own().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let original = own().expect("isolated test window never appeared");
        assert!(original.pid > 0);
        backend.run(&Cmd::Focus(original.id.clone())).unwrap();
        assert_eq!(backend.focused().unwrap(), Some(original.id.clone()));

        backend
            .run(&Cmd::Move {
                win: original.id.clone(),
                to: Place::N(2),
            })
            .unwrap();
        for _ in 0..20 {
            if own().is_some_and(|w| w.workspace == 2) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(own().unwrap().workspace, 2);
        backend
            .run(&Cmd::Move {
                win: original.id.clone(),
                to: original.place.clone(),
            })
            .unwrap();
        for _ in 0..20 {
            if own().is_some_and(|w| w.workspace == original.workspace) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(own().unwrap().workspace, original.workspace);

        backend
            .run(&Cmd::Maximize {
                win: original.id.clone(),
                on: true,
            })
            .unwrap();
        for _ in 0..20 {
            if own().is_some_and(|w| w.maximized) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(own().unwrap().maximized);
        backend
            .run(&Cmd::Maximize {
                win: original.id.clone(),
                on: false,
            })
            .unwrap();
        backend.run(&Cmd::Close(original.id.clone())).unwrap();
        for _ in 0..40 {
            if own().is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(own().is_none(), "Close must be verified, never assumed");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn parses_stable_ewmh_window_identity_and_workspace() {
        let listing = concat!(
            "0x02000007  0 4291 10 10 800 600 Navigator.Firefox host Read this document\n",
            "0x04400002  2 9928 0 0 500 400 foot.Foot localhost Terminal\n",
            "0x04500001 -1 200 0 0 700 400 Editor.Editor host sticky\n",
            "0x04600001  1 0 0 0 700 400 Other.Other host no_pid\n",
        );
        let windows = parse_windows(listing);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].id, "0x02000007");
        assert_eq!(windows[0].workspace, 1);
        assert_eq!(windows[0].pid, 4291);
        assert_eq!(windows[0].class, "firefox");
        assert_eq!(windows[0].title, "Read this document");
        assert_eq!(windows[1].workspace, 3);
    }

    #[test]
    fn rejects_hostile_or_unknown_window_id() {
        for id in ["0x1;rm -rf /", "0x", "", "123", "0x0000000000001", "0x0"] {
            assert!(window_id(id).is_err(), "{id}");
        }
        assert_eq!(
            parse_active("_NET_ACTIVE_WINDOW(WINDOW): window id # 0x2000007"),
            Some("0x02000007".into())
        );
        assert_eq!(
            parse_active("_NET_ACTIVE_WINDOW(WINDOW): window id # 0x0"),
            None
        );
    }

    #[test]
    fn detects_state_and_one_based_workspace() {
        let state =
            "_NET_WM_STATE(ATOM) = _NET_WM_STATE_MAXIMIZED_HORZ, _NET_WM_STATE_MAXIMIZED_VERT";
        assert_eq!(property_state(state), (false, true));
        assert_eq!(
            property_state("_NET_WM_STATE(ATOM) = _NET_WM_STATE_HIDDEN"),
            (true, false)
        );
        assert_eq!(
            parse_workspace("0  - DG: 1000x600\n1  * DG: 1000x600"),
            Some(2)
        );
        assert_eq!(parse_workspace("0  - DG: 1000x600"), None);
    }
}

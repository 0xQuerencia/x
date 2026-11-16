//! Experimental KDE Plasma 6 Wayland backend.
//!
//! kdotool bridges to KWin's built-in scripting API over D-Bus. No external
//! JS may reach this API: all scripts are static templates, and a window id
//! is permitted only after validation as a KWin internal UUID.
//! The shared Controller does all target matching, readback verification,
//! ambiguity handling and bounded Undo.

use std::process::{Command, Stdio};

use serde_json::Value;

use super::{Backend, Cap, Cmd, Place, WinInfo, WmError};

pub struct Kde;

const MAX_OUTPUT: usize = 256 * 1024;
const MAX_WINDOWS: usize = 200;

/// Only the identity returned by KWin may reach a subsequent action.
fn valid_id(id: &str) -> Result<&str, WmError> {
    let bare = id.trim_start_matches('{').trim_end_matches('}');
    let parts: Vec<&str> = bare.split('-').collect();
    let sizes = [8, 4, 4, 4, 12];
    if parts.len() != sizes.len()
        || !parts
            .iter()
            .zip(sizes)
            .all(|(part, len)| part.len() == len && part.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(WmError::Backend("Invalid KWin window UUID".into()));
    }
    Ok(id)
}

fn script(source: &str) -> Result<Value, WmError> {
    // KWin scripts execute inside the compositor, so never use arguments
    // sourced from utterances, window titles or desktop entries.
    let child = Command::new("kdotool")
        .args(["kwinscript", "--inline", source])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| WmError::Backend("kdotool is required on KDE Plasma 6".into()))?;
    let output = child
        .wait_with_output()
        .map_err(|_| WmError::Backend("KWin script could not finish".into()))?;
    if !output.status.success() || output.stdout.len() > MAX_OUTPUT {
        return Err(WmError::Backend("KWin script failed or output exceeded limit".into()));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| WmError::Backend("KWin script did not return valid JSON".into()))
}

const READ: &str = r#"
var desktops = workspace.desktops;
var wins = workspace.windowList().filter(function (w) {
    return w.normalWindow && !w.deleted;
}).slice(0, 200);
var stack = workspace.stackingOrder;
output_result(JSON.stringify({
    windows: wins.map(function (w) {
        var d = (w.desktops && w.desktops.length) ? w.desktops[0] : null;
        return {
            id: String(w.internalId),
            pid: Number(w.pid),
            class: String(w.resourceClass || w.desktopFileName || ""),
            title: String(w.caption || ""),
            workspace: d ? Number(d.x11DesktopNumber) : 0,
            minimized: Boolean(w.minimized),
            maximized: Number(w.maximizeMode) === 3,
            recent: Math.max(0, stack.length - 1 - stack.indexOf(w))
        };
    }),
    focused: workspace.activeWindow ? String(workspace.activeWindow.internalId) : null,
    workspace: workspace.currentDesktop ? Number(workspace.currentDesktop.x11DesktopNumber) : 0
}));
"#;

fn parse_window(row: &Value) -> Option<WinInfo> {
    let id = row.get("id")?.as_str()?;
    valid_id(id).ok()?;
    let pid = u32::try_from(row.get("pid")?.as_u64()?).ok().filter(|p| *p != 0)?;
    let class = row.get("class")?.as_str()?.trim();
    if class.is_empty() || class.len() > 256 {
        return None;
    }
    let ws = row.get("workspace")?.as_i64()?.clamp(0, 10);
    Some(WinInfo {
        id: id.to_string(),
        pid,
        class: class.into(),
        title: row.get("title")?.as_str()?.chars().take(512).collect(),
        workspace: ws,
        place: Place::N(ws),
        minimized: row.get("minimized")?.as_bool()?,
        maximized: row.get("maximized")?.as_bool()?,
        recent: u32::try_from(row.get("recent")?.as_u64()?).unwrap_or(u32::MAX),
    })
}

fn request(cmd: &Cmd) -> Result<String, WmError> {
    let select = |id: &str| -> Result<String, WmError> {
        let id = valid_id(id)?;
        let literal = serde_json::to_string(id).map_err(|_| WmError::Backend("Invalid id".into()))?;
        Ok(format!(
            "var target = workspace.windowList().find(function(w) {{ return String(w.internalId) === {literal}; }});"
        ))
    };
    let (prefix, operation) = match cmd {
        Cmd::Focus(id) => (
            select(id)?,
            "workspace.activeWindow = target;".to_string(),
        ),
        Cmd::Move { win, to: Place::N(n) } if (1..=10).contains(n) => (
            select(win)?,
            format!(
                "var d = workspace.desktops.find(function(x) {{ return Number(x.x11DesktopNumber) === {n}; }}); \
                 if (!d) {{ output_error('Desktop not found'); }} else {{ target.desktops = [d]; }}"
            ),
        ),
        Cmd::Move { .. } => return Err(WmError::Unsupported("KDE backend cannot restore special desktops".into())),
        Cmd::Workspace(n) if (1..=10).contains(n) => (
            String::new(),
            format!(
                "var d = workspace.desktops.find(function(x) {{ return Number(x.x11DesktopNumber) === {n}; }}); \
                 if (!d) {{ output_error('Desktop not found'); }} else {{ workspace.currentDesktop = d; }}"
            ),
        ),
        Cmd::Workspace(_) => return Err(WmError::Unsupported("Desktop must be 1-10".into())),
        Cmd::Maximize { win, on } => (
            select(win)?,
            format!("target.setMaximize({}, {});", on, on),
        ),
        Cmd::Minimize { win } => (select(win)?, "target.minimized = true;".to_string()),
        Cmd::Close(id) => (select(id)?, "target.closeWindow();".to_string()),
    };
    // A no-op or an absent window must never be reported as success.
    let guard = if prefix.is_empty() {
        operation
    } else {
        format!("{prefix}\nif (!target) {{ output_error('Window is gone'); }} else {{ {operation} }}")
    };
    Ok(format!(
        "{guard}\noutput_result(JSON.stringify({{ok:true}}));"
    ))
}

impl Backend for Kde {
    fn name(&self) -> &'static str {
        "KDE Plasma 6"
    }

    fn caps(&self) -> Vec<Cap> {
        vec![
            Cap::List,
            Cap::Focus,
            Cap::SwitchWorkspace,
            Cap::MoveToWorkspace,
            Cap::Maximize,
            Cap::Minimize,
            Cap::Close,
        ]
    }

    fn windows(&self) -> Result<Vec<WinInfo>, WmError> {
        let reply = script(READ)?;
        Ok(reply.get("windows")
            .and_then(Value::as_array)
            .ok_or_else(|| WmError::Backend("KWin has no window list".into()))?
            .iter()
            .take(MAX_WINDOWS)
            .filter_map(parse_window)
            .collect())
    }

    fn focused(&self) -> Result<Option<String>, WmError> {
        let reply = script(READ)?;
        Ok(reply.get("focused")
            .and_then(Value::as_str)
            .and_then(|id| valid_id(id).ok())
            .map(String::from))
    }

    fn workspace(&self) -> Result<i64, WmError> {
        script(READ)?
            .get("workspace")
            .and_then(Value::as_i64)
            .filter(|ws| (1..=10).contains(ws))
            .ok_or_else(|| WmError::Backend("No active KWin desktop 1-10".into()))
    }

    fn run(&self, cmd: &Cmd) -> Result<(), WmError> {
        let reply = script(&request(cmd)?)?;
        if reply.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(WmError::Backend("KWin declined the window action".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "{12345678-1234-1234-1234-123456789abc}";

    #[test]
    fn only_kwin_internal_uuids_can_enter_scripts() {
        for bad in ["", "0x1", "123", "{abc}; close()", "{12345678-1234-1234-1234-123456789abc};",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa\n"
        ] {
            assert!(valid_id(bad).is_err(), "{bad:?}");
            assert!(request(&Cmd::Close(bad.into())).is_err());
        }
        assert!(valid_id(ID).is_ok());
        let code = request(&Cmd::Close(ID.into())).unwrap();
        assert!(code.contains("closeWindow"));
        assert!(!code.contains("eval("));
    }

    #[test]
    fn readback_rejects_missing_pid_or_invalid_id() {
        let good = serde_json::json!({
            "id":ID, "pid":1234, "class":"org.kde.konsole",
            "title":"Shell", "workspace":2, "minimized":false,
            "maximized":false, "recent":0
        });
        assert_eq!(parse_window(&good).unwrap().workspace, 2);
        let mut bad = good.clone();
        bad["pid"] = serde_json::json!(0);
        assert!(parse_window(&bad).is_none());
        bad["pid"] = serde_json::json!(1234);
        bad["id"] = serde_json::json!("0x1");
        assert!(parse_window(&bad).is_none());
    }

    #[test]
    fn refuses_special_desktops_and_unsupported_numbers() {
        assert!(request(&Cmd::Workspace(11)).is_err());
        assert!(request(&Cmd::Move { win: ID.into(), to: Place::Special("special:test".into()) }).is_err());
        assert!(request(&Cmd::Maximize { win: ID.into(), on: true }).unwrap().contains("setMaximize"));
    }
}

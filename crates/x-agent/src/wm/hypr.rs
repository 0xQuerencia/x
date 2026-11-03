//! Hyprland over `hyprctl`. 0.55+ takes Lua dispatchers, older builds named ones; both are
//! built here from validated ids and numbers only.

use std::process::{Command, Stdio};

use super::{Backend, Cap, Cmd, MINIMIZED, Place, WinInfo, WmError};
use crate::desktop::hypr;

pub struct Hypr;

fn valid_id(id: &str) -> Result<&str, WmError> {
    let hex = id.strip_prefix("0x").unwrap_or("");
    if !hex.is_empty() && hex.len() <= 16 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(id)
    } else {
        Err(WmError::Backend(format!("not a window id: {id:?}")))
    }
}

fn dispatch(lua: &str, legacy: Option<Vec<String>>) -> Result<(), WmError> {
    let mut last = String::new();
    let attempts = std::iter::once(vec!["dispatch".to_string(), lua.to_string()]).chain(legacy);
    for args in attempts {
        let out = Command::new("hyprctl")
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| WmError::Backend(format!("could not run hyprctl: {e}")))?;
        last = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && last.eq_ignore_ascii_case("ok") {
            return Ok(());
        }
    }
    Err(WmError::Backend(format!("Hyprland refused: {last}")))
}

fn parse(client: &serde_json::Value) -> Option<WinInfo> {
    let ws = client.get("workspace")?;
    let ws_id = ws.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
    let minimized = ws.get("name").and_then(|v| v.as_str()) == Some(MINIMIZED);
    Some(WinInfo {
        id: client.get("address")?.as_str()?.to_string(),
        pid: u32::try_from(client.get("pid")?.as_u64()?).ok()?,
        class: client.get("class")?.as_str()?.to_string(),
        title: client
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        workspace: if ws_id > 0 { ws_id } else { 0 },
        place: place_of(ws),
        minimized,
        maximized: client.get("fullscreen").and_then(|v| v.as_i64()) == Some(1),
    })
    .filter(|w| {
        client
            .get("mapped")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
            || w.minimized
    })
}

fn place_of(ws: &serde_json::Value) -> Place {
    let id = ws.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
    match ws.get("name").and_then(|v| v.as_str()) {
        Some(name) if id <= 0 && name.starts_with("special:") => Place::Special(name.into()),
        _ => Place::N(id),
    }
}

/// `7` or `special:name`; anything else never reaches the compositor.
fn place_arg(place: &Place) -> Result<String, WmError> {
    match place {
        Place::N(n) if (1..=10).contains(n) => Ok(n.to_string()),
        Place::Special(s)
            if s.strip_prefix("special:").is_some_and(|n| {
                !n.is_empty()
                    && n.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            }) =>
        {
            Ok(s.clone())
        }
        other => Err(WmError::Backend(format!("not a workspace: {other:?}"))),
    }
}

impl Backend for Hypr {
    fn name(&self) -> &'static str {
        "Hyprland"
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
        let clients = hypr(&["clients", "-j"]).map_err(WmError::Backend)?;
        Ok(clients
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(parse)
            .collect())
    }

    fn focused(&self) -> Result<Option<String>, WmError> {
        let active = hypr(&["activewindow", "-j"]).map_err(WmError::Backend)?;
        Ok(active
            .get("address")
            .and_then(|v| v.as_str())
            .map(String::from))
    }

    fn workspace(&self) -> Result<i64, WmError> {
        hypr(&["activeworkspace", "-j"])
            .map_err(WmError::Backend)?
            .get("id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| WmError::Backend("no active workspace".into()))
    }

    fn run(&self, cmd: &Cmd) -> Result<(), WmError> {
        match cmd {
            Cmd::Focus(id) => {
                let id = valid_id(id)?;
                dispatch(
                    &format!("hl.dsp.focus({{ window = \"address:{id}\" }})"),
                    Some(vec![
                        "dispatch".into(),
                        "focuswindow".into(),
                        format!("address:{id}"),
                    ]),
                )
            }
            Cmd::Workspace(n) => dispatch(
                &format!("hl.dsp.focus({{ workspace = \"{n}\" }})"),
                Some(vec!["dispatch".into(), "workspace".into(), n.to_string()]),
            ),
            Cmd::Move { win, to } => {
                let id = valid_id(win)?;
                let ws = place_arg(to)?;
                dispatch(
                    &format!(
                        "hl.dsp.window.move({{ workspace = \"{ws}\", follow = false, window = \"address:{id}\" }})"
                    ),
                    Some(vec![
                        "dispatch".into(),
                        "movetoworkspacesilent".into(),
                        format!("{ws},address:{id}"),
                    ]),
                )
            }
            Cmd::Minimize { win } => {
                let id = valid_id(win)?;
                dispatch(
                    &format!(
                        "hl.dsp.window.move({{ workspace = \"{MINIMIZED}\", follow = false, window = \"address:{id}\" }})"
                    ),
                    Some(vec![
                        "dispatch".into(),
                        "movetoworkspacesilent".into(),
                        format!("{MINIMIZED},address:{id}"),
                    ]),
                )
            }
            Cmd::Maximize { win, on } => {
                let id = valid_id(win)?;
                dispatch(
                    &format!(
                        "hl.dsp.window.fullscreen({{ mode = 1, action = \"{}\", window = \"address:{id}\" }})",
                        if *on { "set" } else { "unset" }
                    ),
                    None,
                )
            }
            Cmd::Close(id) => {
                let id = valid_id(id)?;
                dispatch(
                    &format!("hl.dsp.window.close({{ window = \"address:{id}\" }})"),
                    Some(vec![
                        "dispatch".into(),
                        "closewindow".into(),
                        format!("address:{id}"),
                    ]),
                )
            }
        }
    }
}

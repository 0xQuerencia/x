//! Sway and i3 share one IPC dialect: `swaymsg` / `i3-msg` print the same JSON tree and
//! take the same command language. Window ids are numeric container ids and nothing else.
//! Not yet exercised on a real Sway or i3 session.

use std::process::{Command, Stdio};

use serde_json::Value;

use super::{Backend, Cap, Cmd, Place, WinInfo, WmError};

pub struct I3 {
    pub bin: &'static str,
    pub label: &'static str,
}

impl I3 {
    fn call(&self, args: &[&str]) -> Result<Vec<u8>, WmError> {
        let out = Command::new(self.bin)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| WmError::Backend(format!("could not run {}: {e}", self.bin)))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(WmError::Backend(format!("{} failed", self.bin)))
        }
    }

    fn json(&self, kind: &str) -> Result<Value, WmError> {
        serde_json::from_slice(&self.call(&["-t", kind])?)
            .map_err(|_| WmError::Backend(format!("{} gave no usable answer", self.bin)))
    }

    fn command(&self, command: &str) -> Result<(), WmError> {
        let reply: Value = serde_json::from_slice(&self.call(&["-t", "run_command", command])?)
            .map_err(|_| WmError::Backend("unreadable command reply".into()))?;
        let ok = reply.as_array().is_some_and(|r| {
            !r.is_empty() && r.iter().all(|e| e["success"].as_bool() == Some(true))
        });
        if ok {
            Ok(())
        } else {
            Err(WmError::Backend(format!(
                "{} refused the command",
                self.label
            )))
        }
    }
}

fn numeric(id: &str) -> Result<&str, WmError> {
    if !id.is_empty() && id.len() <= 12 && id.chars().all(|c| c.is_ascii_digit()) {
        Ok(id)
    } else {
        Err(WmError::Backend(format!("not a window id: {id:?}")))
    }
}

fn collect(node: &Value, ws: i64, out: &mut Vec<WinInfo>, focus: &mut Option<String>) {
    let ws = if node["type"] == "workspace" {
        node["num"].as_i64().unwrap_or(0).max(0)
    } else {
        ws
    };
    let class = node["app_id"]
        .as_str()
        .or_else(|| node["window_properties"]["class"].as_str());
    if let (Some(class), Some(id), Some(pid)) =
        (class, node["id"].as_i64(), node["pid"].as_u64().or(Some(0)))
        && node["type"] == "con"
        && node["nodes"].as_array().is_none_or(|n| n.is_empty())
    {
        if node["focused"] == true {
            *focus = Some(id.to_string());
        }
        out.push(WinInfo {
            id: id.to_string(),
            pid: u32::try_from(pid).unwrap_or(0),
            class: class.to_string(),
            title: node["name"].as_str().unwrap_or("").to_string(),
            workspace: ws,
            place: Place::N(ws),
            minimized: false,
            maximized: node["fullscreen_mode"].as_i64().unwrap_or(0) > 0,
        });
    }
    for key in ["nodes", "floating_nodes"] {
        for child in node[key].as_array().into_iter().flatten() {
            collect(child, ws, out, focus);
        }
    }
}

impl Backend for I3 {
    fn name(&self) -> &'static str {
        self.label
    }

    fn caps(&self) -> Vec<Cap> {
        vec![
            Cap::List,
            Cap::Focus,
            Cap::SwitchWorkspace,
            Cap::MoveToWorkspace,
            Cap::Maximize,
            Cap::Close,
        ]
    }

    fn windows(&self) -> Result<Vec<WinInfo>, WmError> {
        let mut out = Vec::new();
        collect(&self.json("get_tree")?, 0, &mut out, &mut None);
        Ok(out)
    }

    fn focused(&self) -> Result<Option<String>, WmError> {
        let mut focus = None;
        collect(&self.json("get_tree")?, 0, &mut Vec::new(), &mut focus);
        Ok(focus)
    }

    fn workspace(&self) -> Result<i64, WmError> {
        self.json("get_workspaces")?
            .as_array()
            .into_iter()
            .flatten()
            .find(|w| w["focused"] == true)
            .and_then(|w| w["num"].as_i64())
            .ok_or_else(|| WmError::Backend("no focused workspace".into()))
    }

    fn run(&self, cmd: &Cmd) -> Result<(), WmError> {
        match cmd {
            Cmd::Focus(id) => self.command(&format!("[con_id={}] focus", numeric(id)?)),
            Cmd::Workspace(n) => self.command(&format!("workspace number {n}")),
            Cmd::Move {
                win,
                to: Place::N(ws),
            } if (1..=10).contains(ws) => self.command(&format!(
                "[con_id={}] move container to workspace number {ws}",
                numeric(win)?
            )),
            Cmd::Move { .. } => Err(WmError::Backend("only numbered workspaces here".into())),
            Cmd::Maximize { win, on } => self.command(&format!(
                "[con_id={}] fullscreen {}",
                numeric(win)?,
                if *on { "enable" } else { "disable" }
            )),
            Cmd::Close(id) => self.command(&format!("[con_id={}] kill", numeric(id)?)),
            Cmd::Minimize { .. } => {
                Err(WmError::Backend(format!("{} has no minimize", self.label)))
            }
        }
    }
}

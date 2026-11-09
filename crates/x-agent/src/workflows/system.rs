//! The real desktop behind a workflow run: the window controller for everything that moves
//! windows (each change verified there), and X's own launcher for starting apps.

use std::path::Path;

use super::engine::Host;
use crate::tools::{self, Action};
use crate::wm::{self, Cap, WinInfo};

pub struct SystemHost;

fn text(e: wm::WmError) -> String {
    e.to_string()
}

impl Host for SystemHost {
    fn windows(&self) -> Result<Vec<WinInfo>, String> {
        wm::view().map(|v| v.windows).map_err(text)
    }

    fn focused(&self) -> Result<Option<WinInfo>, String> {
        wm::view().map(|v| v.focused).map_err(text)
    }

    fn workspace(&self) -> Result<i64, String> {
        wm::view()
            .map_err(text)?
            .workspace
            .ok_or_else(|| "This desktop does not report the current workspace".into())
    }

    fn launch(&self, action: &Action) -> Result<String, String> {
        action.execute()
    }

    fn installed(&self, action: &Action) -> Result<(), String> {
        action.argv(&|name| tools::which(name)).map(drop)
    }

    fn can(&self, cap: Cap) -> bool {
        wm::view().is_ok_and(|v| v.caps.contains(&cap))
    }

    fn focus(&self, w: &WinInfo) -> Result<(), String> {
        wm::focus_exact(&w.id, w.pid, &w.class).map_err(text)
    }

    fn switch_workspace(&self, n: i64) -> Result<(), String> {
        let ws = u8::try_from(n).map_err(|_| "Workspace must be 1-10".to_string())?;
        wm::run(&wm::Request {
            op: wm::Op::Workspace,
            target: wm::Target::default(),
            ws: Some(wm::Ws::N(ws)),
        })
        .map(drop)
        .map_err(text)
    }

    fn move_window(&self, w: &WinInfo, to: i64) -> Result<(), String> {
        wm::move_exact(&w.id, w.pid, &w.class, to).map_err(text)
    }

    fn close_window(&self, w: &WinInfo) -> Result<(), String> {
        wm::close_exact(&w.id, w.pid, &w.class).map_err(text)
    }

    fn opencode_ready(&self) -> Result<(), String> {
        let home_bin = dirs::home_dir().map(|h| h.join(".opencode/bin/opencode"));
        (tools::which("opencode").is_some() || home_bin.is_some_and(|p| p.is_file()))
            .then_some(())
            .ok_or_else(|| "OpenCode not found".into())
    }

    fn opencode(&self, _goal: &str, _dir: &Path) -> Result<String, String> {
        Err("OpenCode steps run through X's supervised runner".into())
    }

    fn cancelled(&self) -> bool {
        false
    }
}

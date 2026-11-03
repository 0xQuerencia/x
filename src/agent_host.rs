//! X, the agent layer: the one task service in this Hex process and the `hex agent`
//! control commands that drive it. The logic lives in `crates/x-agent`; this file only
//! connects it to Hex's Labs switch and control protocol.
//!
//! Speech goes through Hex's own control CLI (`dictation start --raw` / `stop --print`),
//! so a transcript never pastes into the focused app.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::thread;

use x_agent::hexcli::HexCli;
use x_agent::service::{Mode, RealLauncher, Service};

use crate::control::{AgentAction, ControlReply};

const DISABLED: &str = "X is off. Turn it on with `hex labs on x-agent`";

/// OpenCode's installer puts the real binary in `~/.opencode/bin`, which shell aliases
/// and wrappers on PATH can shadow, so look there too.
fn opencode_exe() -> Option<PathBuf> {
    x_agent::tools::which("opencode")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".opencode/bin/opencode")).filter(|p| p.is_file()))
}

pub fn service() -> &'static Service {
    static SERVICE: OnceLock<Service> = OnceLock::new();
    SERVICE.get_or_init(|| {
        let exe = std::env::current_exe().map_or_else(|_| "hex".into(), |p| p.display().to_string());
        Service::new(Arc::new(HexCli { exe }), Arc::new(RealLauncher), opencode_exe())
    })
}

fn snapshot(since: u64) -> ControlReply {
    match serde_json::to_value(service().snapshot(since)) {
        Ok(snapshot) => ControlReply::Agent { snapshot },
        Err(error) => ControlReply::error("failed", format!("couldn't read X's state: {error}")),
    }
}

/// Runs `work` off the control thread: voice calls re-enter Hex's own control socket
/// (`hex dictation …`), which would deadlock a handler that waited on them.
fn background(work: impl FnOnce(&Service) + Send + 'static) {
    thread::spawn(move || work(service()));
}

pub fn handle(action: AgentAction) -> ControlReply {
    if !crate::labs::x_agent() {
        return ControlReply::error("disabled", DISABLED);
    }
    let svc = service();
    match action {
        AgentAction::Status { since } => snapshot(since),
        AgentAction::Toggle { mode } => match Mode::parse(&mode) {
            Err(error) => ControlReply::error("usage", error),
            Ok(mode) => {
                let listening = svc.snapshot(0).phase == x_agent::phase::Phase::Listening;
                background(move |svc| {
                    let result = if listening { svc.voice_stop() } else { svc.voice_start(mode) };
                    if let Err(error) = result {
                        svc.emit("error", &error);
                    }
                });
                ControlReply::done(if listening { "Transcribing" } else { "Listening" })
            }
        },
        AgentAction::Say { text, mode } => match Mode::parse(&mode) {
            Err(error) => ControlReply::error("usage", error),
            Ok(mode) => match svc.plan(&text, mode) {
                Ok(_) => snapshot(0),
                Err(error) => ControlReply::error("refused", error),
            },
        },
        AgentAction::Approve { id } => match svc.approve(id) {
            Ok(()) => ControlReply::done("Approved; running"),
            Err(error) => ControlReply::error("refused", error),
        },
        AgentAction::Stop => {
            background(Service::cancel);
            ControlReply::done("Stop requested")
        }
        AgentAction::Workspace { path } => match svc.set_workspace(&path) {
            Ok(shown) => ControlReply::done(format!("Workspace is {shown}")),
            Err(error) => ControlReply::error("refused", error),
        },
    }
}

/// Hex is quitting: stop any run and drop any recording first. Bounded, so a stuck child
/// can never keep Hex from exiting (the run's process group has already been signalled).
pub fn shutdown() {
    if !crate::labs::x_agent() {
        return;
    }
    let (done, wait) = std::sync::mpsc::channel();
    thread::spawn(move || {
        service().shutdown();
        let _ = done.send(());
    });
    let _ = wait.recv_timeout(std::time::Duration::from_secs(3));
}

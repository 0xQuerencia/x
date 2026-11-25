//! Linux desktop attention and completion notices with no task content.
//! The review window is the only approval surface: a notification daemon cannot
//! grant authority, deny another task, or stop unrelated background work.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use x_agent::phase::Phase;
use x_agent::service::Service;

use crate::control::ControlReply;

fn setting_file() -> Option<PathBuf> {
    crate::app_paths::support_dir()
        .ok()
        .map(|dir| dir.join("x-notifications-off"))
}

fn setting() -> &'static AtomicBool {
    static ENABLED: OnceLock<AtomicBool> = OnceLock::new();
    ENABLED.get_or_init(|| AtomicBool::new(setting_file().is_some_and(|path| !path.exists())))
}

pub(super) fn configure(mode: &str, service: &Service) -> ControlReply {
    let enabled = match mode {
        "status" => None,
        "on" => Some(true),
        "off" => Some(false),
        _ => return ControlReply::error("usage", "Notifications must be on, off or status"),
    };
    if let Some(enabled) = enabled {
        let Some(path) = setting_file() else {
            return ControlReply::error("failed", "The settings directory is unavailable");
        };
        let result = if enabled {
            std::fs::remove_file(path).or_else(super::ignore_missing)
        } else {
            super::write_private(&path, b"off\n")
        };
        if let Err(error) = result {
            return ControlReply::error("failed", format!("Could not save notifications: {error}"));
        }
        let _ = service.take_task_notices();
        setting().store(enabled, Ordering::Release);
    }
    let state = if setting().load(Ordering::Acquire) {
        "on"
    } else {
        "off"
    };
    let platform = if cfg!(target_os = "linux") {
        "Linux desktop notifications require notify-send and a notification daemon."
    } else {
        "This platform has no X desktop notification adapter; use the task activity view."
    };
    ControlReply::done(format!(
        "Task notifications are {state}. Popups contain only task number and outcome; review details in X. {platform}"
    ))
}

fn toast(title: &str, body: &str) {
    if !cfg!(target_os = "linux") {
        return;
    }
    if let Ok(mut child) = Command::new("notify-send")
        .args(["-a", "X", "-t", "5000", "--"])
        .arg(title)
        .arg(body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        thread::spawn(move || {
            // A broken notification daemon must not leave unbounded children.
            for _ in 0..50 {
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Err(_) => break,
                    Ok(None) => thread::sleep(Duration::from_millis(100)),
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        });
    }
}

fn show_review() {
    if let Ok(exe) = std::env::current_exe()
        && let Ok(mut child) = Command::new(exe)
            .args(["window", "show", "agent"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    {
        thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

pub(super) fn watch(service: &Service) {
    let mut previous_attention = None;
    loop {
        thread::sleep(Duration::from_millis(200));
        let notices = service.take_task_notices();
        let attention = service.task_attention();
        let enabled = crate::labs::x_agent() && setting().load(Ordering::Acquire);
        // Drain while disabled, so turning notices on cannot reveal a backlog.
        if enabled {
            for notice in notices {
                toast("X task result", &notice.message());
            }
            if attention != previous_attention
                && let Some(attention) = &attention
            {
                let body = if attention.phase == Phase::WaitingForInput {
                    format!(
                        "Task {} needs your answer. Open X to review the question.",
                        attention.task_id
                    )
                } else {
                    format!(
                        "Task {} needs review. Inspect the exact effects and approve in X.",
                        attention.task_id
                    )
                };
                toast("X needs your attention", &body);
                show_review();
            }
        }
        previous_attention = attention;
    }
}

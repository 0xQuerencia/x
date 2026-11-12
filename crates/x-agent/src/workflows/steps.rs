//! What each workflow step does, and how it is checked. A launch is verified by watching a
//! new window of that app appear; window moves, focus and workspace changes are verified by
//! the window controller; opening a URL or running an action cannot be, and says so.

use std::time::{Duration, Instant};

use super::engine::{Config, Host, Reversal, StepResult, StepStatus};
use super::model::{DEFAULT_TIMEOUT, Step, StepAction, Workflow};
use super::paths;
use crate::tools::{self, Action};
use crate::wm::{self, Cap, WinInfo};

fn windows_of(host: &dyn Host, app: &str) -> Result<Vec<WinInfo>, String> {
    let all = host.windows()?;
    let mut found = wm::matching(app, &all).map_err(|e| e.to_string())?;
    found.sort_by(|a, b| (a.workspace, &a.id).cmp(&(b.workspace, &b.id)));
    Ok(found)
}

/// The one window of `app` that is meant, or why that is unclear. Never guesses.
fn pick_window(host: &dyn Host, app: &str, pick: Option<u8>) -> Result<WinInfo, String> {
    let mut found = windows_of(host, app)?;
    match (found.len(), pick) {
        (0, _) => Err(format!("No open {app} window")),
        (1, None) => Ok(found.remove(0)),
        (n, Some(p)) if (1..=n).contains(&usize::from(p)) => Ok(found.remove(usize::from(p) - 1)),
        (n, Some(p)) => Err(format!(
            "Window pick {p} is no longer valid: {n} {app} window(s) are open; choose again"
        )),
        (n, None) => Err(format!(
            "{n} {app} windows are open; set pick to choose one"
        )),
    }
}

/// Polls until `found` returns a window, the deadline passes, or Stop is pressed.
fn wait_for<T>(
    host: &dyn Host,
    cfg: &Config,
    timeout: Duration,
    on_wait: &mut dyn FnMut(bool),
    mut found: impl FnMut() -> Option<T>,
) -> Result<Option<T>, ()> {
    let end = Instant::now() + timeout;
    on_wait(true);
    let outcome = loop {
        if let Some(hit) = found() {
            break Ok(Some(hit));
        }
        if host.cancelled() {
            break Err(());
        }
        if Instant::now() >= end {
            break Ok(None);
        }
        std::thread::sleep(cfg.poll);
    };
    on_wait(false);
    outcome
}

fn timeout_of(step: &Step) -> Duration {
    Duration::from_secs(step.timeout_secs.unwrap_or(DEFAULT_TIMEOUT))
}

fn move_to(host: &dyn Host, win: &WinInfo, ws: u8) -> Result<Option<Reversal>, String> {
    if win.workspace == i64::from(ws) {
        return Ok(None);
    }
    host.move_window(win, i64::from(ws))?;
    Ok(Some(Reversal::MoveBack(win.clone())))
}

/// Launches `action` and waits for a new window of `app` to appear, then optionally places it.
fn launch_new(
    step: &Step,
    host: &dyn Host,
    cfg: &Config,
    action: &Action,
    app: &str,
    workspace: Option<u8>,
    on_wait: &mut dyn FnMut(bool),
) -> StepResult {
    let before: Vec<String> = match windows_of(host, app) {
        Ok(w) => w.into_iter().map(|w| w.id).collect(),
        Err(e) => return StepResult::failed(e),
    };
    if let Err(e) = host.launch(action) {
        return StepResult::failed(e);
    }
    let appeared = wait_for(host, cfg, timeout_of(step), on_wait, || {
        windows_of(host, app)
            .ok()?
            .into_iter()
            .find(|w| !before.contains(&w.id))
    });
    let win = match appeared {
        Err(()) => return StepResult::cancelled(),
        Ok(Some(w)) => w,
        Ok(None) if workspace.is_some() => {
            return StepResult::failed(format!(
                "Launched, but no {app} window appeared within {}s to place it",
                timeout_of(step).as_secs()
            ));
        }
        Ok(None) => {
            return StepResult::unverified(
                "Launched; no new window appeared (it may have opened in a window that was already open)",
                vec![],
            );
        }
    };
    // The window is left open on undo: it may hold work typed since the run.
    let mut reversals: Vec<Reversal> = vec![];
    if let Some(ws) = workspace {
        match move_to(host, &win, ws) {
            Ok(Some(r)) => reversals.push(r),
            Ok(None) => {}
            Err(e) => {
                return StepResult {
                    reversals,
                    ..StepResult::failed(format!("Opened, but could not place it: {e}"))
                };
            }
        }
        return StepResult::done(
            format!("Window appeared and is on workspace {ws}"),
            reversals,
        );
    }
    StepResult::done("Window appeared", reversals)
}

pub(super) fn run_step(
    step: &Step,
    host: &dyn Host,
    cfg: &Config,
    on_wait: &mut dyn FnMut(bool),
) -> StepResult {
    let result = run_action(step, host, cfg, on_wait);
    on_wait(false);
    result
}

fn run_action(
    step: &Step,
    host: &dyn Host,
    cfg: &Config,
    on_wait: &mut dyn FnMut(bool),
) -> StepResult {
    match &step.action {
        StepAction::OpenApp { app, workspace } => {
            let existing = match windows_of(host, app) {
                Ok(w) => w,
                Err(e) => return StepResult::failed(e),
            };
            // Running it again must not pile up a second copy: reuse what is open.
            if let Some(win) = existing.into_iter().next() {
                let Some(ws) = workspace else {
                    return StepResult::done("Already open, reused", vec![]);
                };
                return match move_to(host, &win, *ws) {
                    Ok(r) => StepResult::done(
                        format!("Already open, now on workspace {ws}"),
                        r.into_iter().collect(),
                    ),
                    Err(e) => StepResult::failed(e),
                };
            }
            launch_new(
                step,
                host,
                cfg,
                &Action::App {
                    target: app.clone(),
                },
                app,
                *workspace,
                on_wait,
            )
        }
        StepAction::OpenTerminal {
            directory,
            workspace,
        } => {
            let dir = match paths::existing(directory, &cfg.home) {
                Ok(d) => d,
                Err(e) => return StepResult::failed(e),
            };
            let action = Action::Terminal {
                directory: dir.display().to_string(),
            };
            launch_new(step, host, cfg, &action, "terminal", *workspace, on_wait)
        }
        StepAction::OpenDirectory { directory } => {
            let dir = match paths::existing(directory, &cfg.home) {
                Ok(d) => d,
                Err(e) => return StepResult::failed(e),
            };
            let action = Action::Folder {
                directory: dir.display().to_string(),
            };
            launch_new(step, host, cfg, &action, "files", None, on_wait)
        }
        StepAction::OpenUrl { url } => {
            let action = Action::Url {
                target: url.clone(),
                browser: String::new(),
            };
            match host.launch(&action) {
                Ok(_) => StepResult::unverified(
                    "Sent to your browser; X does not check that the page loaded",
                    vec![],
                ),
                Err(e) => StepResult::failed(e),
            }
        }
        StepAction::FocusWindow { app, pick } => {
            let win = match pick_window(host, app, *pick) {
                Ok(w) => w,
                Err(e) => return StepResult::failed(e),
            };
            let before = host.focused().ok().flatten();
            match host.focus(&win) {
                Ok(()) => StepResult::done(
                    format!("{app} has focus"),
                    before
                        .filter(|b| b.id != win.id)
                        .map(Reversal::FocusBack)
                        .into_iter()
                        .collect(),
                ),
                Err(e) => StepResult::failed(e),
            }
        }
        StepAction::SwitchWorkspace { workspace } => {
            let from = match host.workspace() {
                Ok(n) => n,
                Err(e) => return StepResult::failed(e),
            };
            if from == i64::from(*workspace) {
                return StepResult::done(format!("Already on workspace {workspace}"), vec![]);
            }
            match host.switch_workspace(i64::from(*workspace)) {
                Ok(()) => StepResult::done(
                    format!("On workspace {workspace}"),
                    vec![Reversal::SwitchBack(from)],
                ),
                Err(e) => StepResult::failed(e),
            }
        }
        StepAction::MoveWindow {
            app,
            workspace,
            pick,
        } => {
            let win = match pick_window(host, app, *pick) {
                Ok(w) => w,
                Err(e) => return StepResult::failed(e),
            };
            match move_to(host, &win, *workspace) {
                Ok(r) => StepResult::done(
                    format!("{app} is on workspace {workspace}"),
                    r.into_iter().collect(),
                ),
                Err(e) => StepResult::failed(e),
            }
        }
        StepAction::WaitForWindow { app } => {
            let hit = wait_for(host, cfg, timeout_of(step), on_wait, || {
                windows_of(host, app).ok().filter(|w| !w.is_empty())
            });
            match hit {
                Err(()) => StepResult::cancelled(),
                Ok(Some(_)) => StepResult::done(format!("{app} window is open"), vec![]),
                Ok(None) => StepResult::failed(format!(
                    "No {app} window appeared within {}s",
                    timeout_of(step).as_secs()
                )),
            }
        }
        StepAction::RunAction { phrase } => run_phrase(phrase, host),
        StepAction::OpencodeTask { goal, directory } => {
            let dir = match paths::existing(directory, &cfg.home) {
                Ok(d) => d,
                Err(e) => return StepResult::failed(e),
            };
            match host.opencode(goal, &dir) {
                Ok(note) => StepResult::done(note, vec![]),
                Err(e) if e == "Stopped" => StepResult::cancelled(),
                Err(e) => StepResult::failed(e),
            }
        }
    }
}

fn run_phrase(phrase: &str, host: &dyn Host) -> StepResult {
    let Some(actions) = tools::plan_rules(phrase) else {
        return StepResult::failed("X no longer understands that phrase");
    };
    let mut verified = true;
    for action in &actions {
        if let Err(e) = action.clone().validate() {
            return StepResult::failed(e);
        }
        if !matches!(
            action,
            Action::Window { .. } | Action::Workspace { .. } | Action::Focus { .. }
        ) {
            verified = false;
        }
        if let Err(e) = host.launch(action) {
            return StepResult::failed(e);
        }
    }
    if verified {
        StepResult::done("Done and checked by the window controller", vec![])
    } else {
        StepResult::unverified("Launched; X does not check what it opened", vec![])
    }
}

/// Everything that can be known before the first launch. Returns the problems found.
pub fn preflight(wf: &Workflow, host: &dyn Host, cfg: &Config) -> Vec<String> {
    let mut problems = Vec::new();
    let mut opens: Vec<&str> = Vec::new(); // apps an earlier step brings up
    for step in &wf.steps {
        let id = &step.id;
        let mut need = |ok: Result<(), String>| {
            if let Err(e) = ok {
                problems.push(format!("{id}: {e}"));
            }
        };
        let cap = |c: Cap, what: &str| -> Result<(), String> {
            host.can(c)
                .then_some(())
                .ok_or_else(|| format!("this desktop cannot {what}"))
        };
        let has_window =
            |app: &str| windows_of(host, app).is_ok_and(|w| !w.is_empty()) || opens.contains(&app);
        match &step.action {
            StepAction::OpenApp { app, workspace } => {
                if windows_of(host, app).map_or(true, |w| w.is_empty()) {
                    need(host.installed(&Action::App {
                        target: app.clone(),
                    }));
                }
                if workspace.is_some() {
                    need(cap(Cap::MoveToWorkspace, "move windows between workspaces"));
                }
                opens.push(app);
            }
            StepAction::OpenTerminal {
                directory,
                workspace,
            } => {
                need(paths::existing(directory, &cfg.home).and_then(|d| {
                    host.installed(&Action::Terminal {
                        directory: d.display().to_string(),
                    })
                }));
                if workspace.is_some() {
                    need(cap(Cap::MoveToWorkspace, "move windows between workspaces"));
                }
                opens.push("terminal");
            }
            StepAction::OpenDirectory { directory } => {
                need(paths::existing(directory, &cfg.home).and_then(|d| {
                    host.installed(&Action::Folder {
                        directory: d.display().to_string(),
                    })
                }));
            }
            StepAction::OpenUrl { url } => {
                need(host.installed(&Action::Url {
                    target: url.clone(),
                    browser: String::new(),
                }));
            }
            StepAction::FocusWindow { app, .. } => {
                need(cap(Cap::Focus, "focus windows"));
                if !has_window(app) {
                    need(Err(format!(
                        "no {app} window is open and no earlier step opens one"
                    )));
                }
            }
            StepAction::MoveWindow { app, .. } => {
                need(cap(Cap::MoveToWorkspace, "move windows between workspaces"));
                if !has_window(app) {
                    need(Err(format!(
                        "no {app} window is open and no earlier step opens one"
                    )));
                }
            }
            StepAction::SwitchWorkspace { .. } => {
                need(cap(Cap::SwitchWorkspace, "switch workspaces"))
            }
            StepAction::WaitForWindow { app } => {
                if !has_window(app) {
                    need(
                        host.installed(&Action::App {
                            target: app.clone(),
                        })
                        .map_err(|_| format!("{app} is not installed and nothing opens it")),
                    );
                }
            }
            StepAction::RunAction { phrase } => match tools::plan_rules(phrase) {
                None => need(Err("X does not understand that phrase".into())),
                Some(actions) => {
                    for a in actions.iter().filter(|a| a.launches_window()) {
                        need(host.installed(a));
                    }
                }
            },
            StepAction::OpencodeTask { directory, .. } => {
                need(paths::existing(directory, &cfg.home).map(drop));
                need(host.opencode_ready());
            }
        }
    }
    problems
}

pub(crate) fn status_word(s: StepStatus) -> &'static str {
    match s {
        StepStatus::Pending => "pending",
        StepStatus::Running => "running",
        StepStatus::Waiting => "waiting",
        StepStatus::Done => "done",
        StepStatus::Unverified => "launched",
        StepStatus::Failed => "failed",
        StepStatus::Skipped => "pending",
        StepStatus::Cancelled => "failed",
    }
}

//! Runs a workflow as an explicit state machine against a [`Host`] (the real desktop in
//! the app, a fake in tests).
//!
//! idle -> preparing -> running <-> waiting -> completed | failed | cancelled
//!
//! Preparing checks every step (apps installed, folders present, window-manager features
//! available) before anything is launched, so a workflow that cannot finish does not start
//! and leave a half-built session behind. Launches are never retried; only the polling for
//! a window to appear repeats, and it has a deadline.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::model::{Step, StepAction, Workflow};
use super::steps;
use crate::tools::Action;
use crate::wm::{Cap, WinInfo};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Idle,
    Preparing,
    Running,
    /// Polling for a window to appear.
    Waiting,
    NeedsPermission,
    Completed,
    Failed,
    Cancelled,
}

impl RunState {
    pub fn can_move_to(self, next: Self) -> bool {
        use RunState::*;
        matches!(
            (self, next),
            (Idle, Preparing)
                | (Preparing, Running | NeedsPermission | Failed | Cancelled)
                | (NeedsPermission, Running | Failed | Cancelled)
                | (
                    Running,
                    Waiting | NeedsPermission | Completed | Failed | Cancelled
                )
                | (Waiting, Running | Failed | Cancelled | Completed)
        )
    }

    pub fn is_final(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Waiting,
    /// Done, and the result was checked on the desktop.
    Done,
    /// Done as far as X can tell, but it could not be checked.
    Unverified,
    Failed,
    Skipped,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepReport {
    pub id: String,
    pub title: String,
    pub status: StepStatus,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub workflow: String,
    pub name: String,
    pub state: RunState,
    pub steps: Vec<StepReport>,
    pub summary: String,
    /// How many finished steps can be undone.
    pub undoable: usize,
}

/// How to put one thing back.
#[derive(Clone, Debug)]
pub enum Reversal {
    CloseWindow(WinInfo),
    MoveBack(WinInfo),
    SwitchBack(i64),
    FocusBack(WinInfo),
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub report: Report,
    pub reversals: Vec<Reversal>,
}

pub struct Config {
    pub poll: Duration,
    pub home: std::path::PathBuf,
}

/// Everything a run needs from the machine. Window changes verify themselves.
pub trait Host: Send + Sync {
    fn windows(&self) -> Result<Vec<WinInfo>, String>;
    fn focused(&self) -> Result<Option<WinInfo>, String>;
    fn workspace(&self) -> Result<i64, String>;
    fn launch(&self, action: &Action) -> Result<String, String>;
    fn installed(&self, action: &Action) -> Result<(), String>;
    fn can(&self, cap: Cap) -> bool;
    fn focus(&self, window: &WinInfo) -> Result<(), String>;
    fn switch_workspace(&self, n: i64) -> Result<(), String>;
    fn move_window(&self, window: &WinInfo, to: i64) -> Result<(), String>;
    fn close_window(&self, window: &WinInfo) -> Result<(), String>;
    fn opencode_ready(&self) -> Result<(), String>;
    fn opencode(&self, goal: &str, dir: &Path) -> Result<String, String>;
    /// Whether this step may run. The person approved the workflow as a whole; this is the
    /// per-step gate for anything sensitive.
    fn permit(&self, _action: &StepAction) -> bool {
        true
    }
    fn cancelled(&self) -> bool;
    /// Live progress for a UI; ignored by default.
    fn progress(&self, _index: usize, _status: StepStatus, _detail: &str) {}
}

pub(super) struct StepResult {
    pub status: StepStatus,
    pub detail: String,
    pub reversals: Vec<Reversal>,
}

impl StepResult {
    pub fn done(detail: impl Into<String>, reversals: Vec<Reversal>) -> Self {
        Self {
            status: StepStatus::Done,
            detail: detail.into(),
            reversals,
        }
    }
    pub fn unverified(detail: impl Into<String>, reversals: Vec<Reversal>) -> Self {
        Self {
            status: StepStatus::Unverified,
            detail: detail.into(),
            reversals,
        }
    }
    pub fn failed(detail: impl Into<String>) -> Self {
        Self {
            status: StepStatus::Failed,
            detail: detail.into(),
            reversals: vec![],
        }
    }
    pub fn cancelled() -> Self {
        Self {
            status: StepStatus::Cancelled,
            detail: "Stopped".into(),
            reversals: vec![],
        }
    }
}

struct Run<'a> {
    wf: &'a Workflow,
    host: &'a dyn Host,
    cfg: &'a Config,
    state: RunState,
    steps: Vec<StepReport>,
    reversals: Vec<Reversal>,
}

impl Run<'_> {
    fn go(&mut self, next: RunState) {
        debug_assert!(
            self.state == next || self.state.can_move_to(next),
            "{:?} -> {next:?}",
            self.state
        );
        self.state = next;
    }

    fn set(&mut self, i: usize, status: StepStatus, detail: &str) {
        self.steps[i].status = status;
        self.steps[i].detail = detail.to_string();
        self.host.progress(i, status, detail);
    }

    fn mark_rest(&mut self, from: usize, status: StepStatus, why: &str) {
        for i in from..self.steps.len() {
            if self.steps[i].status == StepStatus::Pending {
                self.set(i, status, why);
            }
        }
    }

    fn satisfied(&self, step: &Step) -> Result<(), String> {
        for need in &step.requires {
            let ok = self
                .steps
                .iter()
                .find(|s| s.id == *need)
                .is_some_and(|s| matches!(s.status, StepStatus::Done | StepStatus::Unverified));
            if !ok {
                return Err(format!("needs step {need:?}, which did not succeed"));
            }
        }
        Ok(())
    }

    fn execute(&mut self) {
        self.go(RunState::Preparing);
        let problems = steps::preflight(self.wf, self.host, self.cfg);
        if !problems.is_empty() {
            self.go(RunState::Failed);
            let why = format!("Nothing was started: {}", problems.join("; "));
            self.mark_rest(0, StepStatus::Skipped, "not started");
            if let Some(first) = self.steps.first_mut() {
                first.detail = why;
            }
            return;
        }
        self.go(RunState::Running);
        let mut stopped_by: Option<String> = None;
        for (i, step) in self.wf.steps.iter().enumerate() {
            if self.host.cancelled() {
                self.mark_rest(i, StepStatus::Skipped, "stopped before this step");
                self.go(RunState::Cancelled);
                return;
            }
            if let Some(by) = &stopped_by {
                self.set(i, StepStatus::Skipped, &format!("not run: {by} failed"));
                continue;
            }
            if let Err(why) = self.satisfied(step) {
                self.set(i, StepStatus::Skipped, &why);
                continue;
            }
            if matches!(step.action, StepAction::OpencodeTask { .. }) {
                self.go(RunState::NeedsPermission);
                if !self.host.permit(&step.action) {
                    self.set(i, StepStatus::Failed, "Permission denied");
                    self.go(RunState::Failed);
                    self.mark_rest(i + 1, StepStatus::Skipped, "not run: permission was denied");
                    return;
                }
                self.go(RunState::Running);
            }
            self.set(i, StepStatus::Running, "");
            let result = steps::run_step(step, self.host, self.cfg, &mut |waiting| {
                let state = if waiting {
                    RunState::Waiting
                } else {
                    RunState::Running
                };
                if self.state != state && self.state.can_move_to(state) {
                    self.state = state;
                }
            });
            self.reversals.extend(result.reversals);
            self.set(i, result.status, &result.detail);
            match result.status {
                StepStatus::Cancelled => {
                    self.mark_rest(i + 1, StepStatus::Skipped, "stopped before this step");
                    self.go(RunState::Cancelled);
                    return;
                }
                StepStatus::Failed if step.on_failure == super::model::OnFailure::Stop => {
                    stopped_by = Some(format!("step {:?}", step.id));
                }
                _ => {}
            }
        }
        let failed = self.steps.iter().any(|s| s.status == StepStatus::Failed);
        self.go(if failed {
            RunState::Failed
        } else {
            RunState::Completed
        });
    }
}

fn summary(name: &str, state: RunState, steps: &[StepReport]) -> String {
    let count = |s: StepStatus| steps.iter().filter(|x| x.status == s).count();
    let (done, unverified, failed) = (
        count(StepStatus::Done),
        count(StepStatus::Unverified),
        count(StepStatus::Failed),
    );
    let skipped =
        count(StepStatus::Skipped) + count(StepStatus::Pending) + count(StepStatus::Cancelled);
    let total = steps.len();
    match state {
        RunState::Completed if unverified == 0 => {
            format!("{name}: all {total} steps done and checked")
        }
        RunState::Completed => {
            format!("{name}: {total} steps ran; {unverified} could not be verified")
        }
        RunState::Cancelled => format!(
            "{name}: stopped after {} of {total} steps; the rest did not run",
            done + unverified
        ),
        RunState::Failed if done + unverified + failed == 0 => {
            let why = steps.first().map_or("", |s| s.detail.as_str());
            format!("{name}: {why}")
        }
        _ => {
            let first = steps.iter().find(|s| s.status == StepStatus::Failed);
            let why = first.map_or(String::new(), |s| format!(" at {:?}: {}", s.id, s.detail));
            format!(
                "{name}: failed{why}. {done} done, {unverified} unverified, {failed} failed, {skipped} not run"
            )
        }
    }
}

/// Runs `wf` to the end (or until Stop). Never panics on a bad desktop: every error is a
/// step result.
pub fn run(wf: &Workflow, host: &dyn Host, cfg: &Config) -> Outcome {
    let steps = wf
        .steps
        .iter()
        .map(|s| StepReport {
            id: s.id.clone(),
            title: s.action.describe(),
            status: StepStatus::Pending,
            detail: String::new(),
        })
        .collect();
    let mut r = Run {
        wf,
        host,
        cfg,
        state: RunState::Idle,
        steps,
        reversals: vec![],
    };
    r.execute();
    let undoable = r.reversals.len();
    let summary = summary(&wf.name, r.state, &r.steps);
    Outcome {
        report: Report {
            workflow: wf.id.clone(),
            name: wf.name.clone(),
            state: r.state,
            steps: r.steps,
            summary,
            undoable,
        },
        reversals: r.reversals,
    }
}

/// Puts back what a run changed, newest first, checking each window is still there.
/// Returns what was undone and what could not be.
pub fn undo(reversals: &[Reversal], host: &dyn Host) -> (usize, Vec<String>) {
    let mut done = 0;
    let mut problems = Vec::new();
    let current = |w: &WinInfo| {
        host.windows().ok().and_then(|all| {
            all.into_iter()
                .find(|x| x.id == w.id && x.pid == w.pid && x.class == w.class)
        })
    };
    for rev in reversals.iter().rev() {
        let result = match rev {
            Reversal::SwitchBack(n) => host.switch_workspace(*n),
            Reversal::CloseWindow(w) => match current(w) {
                Some(now) => host.close_window(&now),
                None => Ok(()), // already closed: nothing to put back
            },
            Reversal::MoveBack(w) => match current(w) {
                Some(now) => host.move_window(&now, w.workspace.max(1)),
                None => Err(format!("{} is gone", w.class)),
            },
            Reversal::FocusBack(w) => match current(w) {
                Some(now) => host.focus(&now),
                None => Err(format!("{} is gone", w.class)),
            },
        };
        match result {
            Ok(()) => done += 1,
            Err(e) => problems.push(e),
        }
    }
    (done, problems)
}

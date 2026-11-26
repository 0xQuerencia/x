//! Task-scoped background Work using the same service executor as foreground work.
//!
//! Only coding and fixed-suite test plans may leave the foreground. Desktop input
//! stays on its one foreground state. Project leases span every run, and remain
//! held until the owned runner returns, including after a Stop request.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use super::{Cancellation, Event, Grants, Mode, Phase, Review, Service, State, Task, emit, now};

const MAX_BACKGROUND: usize = 4;
const MAX_RETAINED: usize = 16;
const SHOWN_EVENTS: usize = 20;

#[derive(Clone, Debug, Serialize)]
pub struct BackgroundSnapshot {
    pub task: Task,
    pub started_at: u64,
    pub events: Vec<Event>,
    pub cancellable_coding_jobs: Vec<usize>,
    #[serde(skip)]
    pub permission_waits: Vec<super::PermissionWait>,
}

#[derive(Clone)]
struct Run {
    id: u64,
    started_at: u64,
    state: Arc<Mutex<State>>,
    grants: Arc<Grants>,
}

#[derive(Default)]
pub(super) struct Coordinator {
    runs: Vec<Run>,
    leases: BTreeMap<u64, Vec<PathBuf>>,
}

impl Coordinator {
    pub(super) fn owns(&self, id: u64) -> bool {
        self.leases.contains_key(&id)
    }

    pub(super) fn clear_finished(&mut self) -> usize {
        let before = self.runs.len();
        self.runs.retain(|run| self.leases.contains_key(&run.id));
        before - self.runs.len()
    }
}

pub(super) struct Lease {
    task_id: u64,
    coordinator: Arc<Mutex<Coordinator>>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.coordinator
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .leases
            .remove(&self.task_id);
    }
}

impl State {
    /// Preserve foreground configuration and undo history, without copying a
    /// pending approval or cancellation token into the next task.
    fn empty_foreground(&self) -> Self {
        Self {
            phase: Phase::Idle,
            task: None,
            events: self.events.clone(),
            notices: Arc::clone(&self.notices),
            seq: self.seq,
            serial: self.serial,
            actions: vec![],
            stop: Cancellation::default(),
            model_stop: None,
            voice_mode: self.voice_mode,
            voice_route: self.voice_route.clone(),
            workspace: self.workspace.clone(),
            selected_session: self.selected_session.clone(),
            session_list: self.session_list.clone(),
            claude_session: self.claude_session.clone(),
            autonomy_mode: self.autonomy_mode,
            review: Review::Required,
            ledger: self.ledger.clone(),
            grants: vec![],
            computer_apps: self.computer_apps.clone(),
            computer_remote: self.computer_remote,
            computer_dialogue: None,
            window_dialogue: None,
            guard: None,
            edit: None,
            workflow: None,
            graph_workflow: None,
            active_graph_stops: None,
            pending_tests: None,
            pending_integration: None,
            project_command: None,
            create_workspace: false,
            undo: self.undo.clone(),
        }
    }

    pub(super) fn can_background(&self) -> bool {
        self.phase == Phase::AwaitingApproval
            && !self.create_workspace
            && self.model_stop.is_none()
            && (self
                .graph_workflow
                .as_ref()
                .is_some_and(|g| g.background_capable())
                || self.pending_tests.is_some()
                || self
                    .task
                    .as_ref()
                    .is_some_and(|task| task.mode == Mode::Work))
            && self.workflow.is_none()
            && self.pending_integration.is_none()
            && self.project_command.is_none()
            && self.edit.is_none()
            && self.actions.is_empty()
    }

    fn work_roots(&self) -> Vec<PathBuf> {
        if let Some(graph) = &self.graph_workflow {
            return graph.project_roots();
        }
        if let Some(tests) = &self.pending_tests {
            return vec![tests.project_root()];
        }
        if let Some(integration) = &self.pending_integration {
            return vec![integration.project_root()];
        }
        self.task
            .as_ref()
            .filter(|task| task.mode == Mode::Work)
            .and_then(|task| task.workspace.as_ref())
            .map(|path| vec![PathBuf::from(path)])
            .unwrap_or_default()
    }
}

impl Service {
    /// Approve this exact preview and retain its execution in a background slot.
    /// This never reconstructs a plan from its display text or grants desktop
    /// access to a background worker.
    pub fn approve_background(&self, task_id: u64) -> Result<(), String> {
        let mut foreground = self.lock();
        if foreground
            .task
            .as_ref()
            .is_none_or(|task| task.id != task_id)
            || !foreground.can_background()
        {
            return Err(
                "Only the current reviewed coding or test plan can run in the background".into(),
            );
        }
        let mut coordinator = self
            .coordinator
            .lock()
            .map_err(|_| "Task coordinator unavailable")?;
        let active = coordinator
            .runs
            .iter()
            .filter(|run| coordinator.leases.contains_key(&run.id))
            .count();
        if active >= MAX_BACKGROUND {
            return Err("Four background runs are already active; stop or finish one first".into());
        }
        while coordinator.runs.len() >= MAX_RETAINED {
            let Some(index) = coordinator
                .runs
                .iter()
                .position(|run| !coordinator.leases.contains_key(&run.id))
            else {
                return Err("Background run history is full".into());
            };
            coordinator.runs.remove(index);
        }
        let replacement = foreground.empty_foreground();
        let detached = std::mem::replace(&mut *foreground, replacement);
        let run = Run {
            id: task_id,
            started_at: now(),
            state: Arc::new(Mutex::new(detached)),
            grants: Arc::new(Grants::default()),
        };
        coordinator.runs.push(run.clone());
        drop(coordinator);
        let worker = self.for_run(&run);
        // Holding the foreground lock keeps a competing new plan out until
        // approval either succeeds or is restored intact after refusal.
        if let Err(error) = worker.approve(task_id) {
            self.coordinator
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .runs
                .retain(|entry| entry.id != task_id);
            let mut state = worker.lock();
            let replacement = state.empty_foreground();
            *foreground = std::mem::replace(&mut *state, replacement);
            return Err(error);
        }
        emit(
            &mut foreground,
            "info",
            &format!("Task {task_id} is running in the background"),
        );
        Ok(())
    }

    pub(super) fn reserve_work(&self, state: &State, task_id: u64) -> Result<Lease, String> {
        self.reserve_roots(task_id, state.work_roots())
    }

    pub(super) fn reserve_worktree_review(&self, root: PathBuf) -> Result<Lease, String> {
        // Task IDs start at one. A review uses the same project exclusion
        // without binding a foreground or background task.
        self.reserve_roots(0, vec![root])
    }

    fn reserve_roots(&self, task_id: u64, roots: Vec<PathBuf>) -> Result<Lease, String> {
        let mut coordinator = self
            .coordinator
            .lock()
            .map_err(|_| "Task coordinator unavailable")?;
        if coordinator.leases.contains_key(&task_id) {
            return Err("This task already owns an execution lease".into());
        }
        if roots.iter().any(|root| {
            coordinator
                .leases
                .values()
                .flatten()
                .any(|held| root.starts_with(held) || held.starts_with(root))
        }) {
            return Err(
                "Another run owns this project; wait for its worker to finish or stop it first"
                    .into(),
            );
        }
        coordinator.leases.insert(task_id, roots);
        Ok(Lease {
            task_id,
            coordinator: Arc::clone(&self.coordinator),
        })
    }

    pub(super) fn release_idle_graph_projects(&self, task_id: u64, active: &[&std::path::Path]) {
        if let Ok(mut coordinator) = self.coordinator.lock()
            && let Some(roots) = coordinator.leases.get_mut(&task_id)
        {
            roots.retain(|root| active.contains(&root.as_path()));
        }
    }

    /// None means another run owns this project. True means a released
    /// reservation was reacquired and the recorded project must be rechecked.
    pub(super) fn try_reacquire_graph_project(
        &self,
        task_id: u64,
        root: &std::path::Path,
    ) -> Option<bool> {
        let mut coordinator = self.coordinator.lock().ok()?;
        if coordinator
            .leases
            .get(&task_id)?
            .iter()
            .any(|path| path == root)
        {
            return Some(false);
        }
        if coordinator.leases.iter().any(|(owner, held)| {
            *owner != task_id
                && held
                    .iter()
                    .any(|path| root.starts_with(path) || path.starts_with(root))
        }) {
            return None;
        }
        coordinator
            .leases
            .get_mut(&task_id)?
            .push(root.to_path_buf());
        Some(true)
    }

    pub(super) fn release_graph_project(&self, task_id: u64, root: &std::path::Path) {
        if let Ok(mut coordinator) = self.coordinator.lock()
            && let Some(roots) = coordinator.leases.get_mut(&task_id)
        {
            roots.retain(|held| held != root);
        }
    }

    fn for_run(&self, run: &Run) -> Self {
        let mut service = self.clone();
        service.state = Arc::clone(&run.state);
        service.scoped_grants = Arc::clone(&run.grants);
        service
    }

    fn runs(&self) -> Vec<Run> {
        self.coordinator
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .runs
            .clone()
    }

    /// Exact current foreground or retained background task, without fallback.
    pub(super) fn service_for_task(&self, task_id: u64) -> Result<Self, String> {
        if self
            .lock()
            .task
            .as_ref()
            .is_some_and(|task| task.id == task_id)
        {
            return Ok(self.clone());
        }
        self.runs()
            .into_iter()
            .find(|run| run.id == task_id)
            .map(|run| self.for_run(&run))
            .ok_or_else(|| "No task with that exact ID".into())
    }

    pub(super) fn background_snapshots(&self) -> Vec<BackgroundSnapshot> {
        self.runs()
            .into_iter()
            .filter_map(|run| {
                let state = run.state.lock().unwrap_or_else(|p| p.into_inner());
                let task = state.task.clone()?;
                let events = state
                    .events
                    .iter()
                    .rev()
                    .take(SHOWN_EVENTS)
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                Some(BackgroundSnapshot {
                    cancellable_coding_jobs: cancellable_jobs(&state),
                    permission_waits: super::harness_workflow::permission_waits(&state),
                    task,
                    started_at: run.started_at,
                    events,
                })
            })
            .collect()
    }

    /// Stop only a named background run. A stale or unknown ID never falls
    /// back to the foreground or another run.
    pub fn cancel_run(&self, task_id: u64) -> Result<(), String> {
        let run = self
            .runs()
            .into_iter()
            .find(|run| run.id == task_id)
            .ok_or("No background run with that task ID")?;
        self.for_run(&run).cancel_foreground();
        Ok(())
    }

    pub fn cancel_background_job(&self, task_id: u64, index: usize) -> Result<(), String> {
        let run = self
            .runs()
            .into_iter()
            .find(|run| run.id == task_id)
            .ok_or("No background run with that task ID")?;
        self.for_run(&run).cancel_coding_job(task_id, index)
    }

    pub(super) fn cancel_background_runs(&self) {
        for run in self.runs() {
            self.for_run(&run).cancel_foreground();
        }
    }

    pub(super) fn workers_finished(&self) -> bool {
        self.coordinator
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .leases
            .is_empty()
    }

    pub(super) fn background_busy(&self) -> bool {
        let coordinator = self.coordinator.lock().unwrap_or_else(|p| p.into_inner());
        coordinator
            .runs
            .iter()
            .any(|run| coordinator.leases.contains_key(&run.id))
    }

    pub(super) fn sync_background_computer_policy(
        &self,
        apps: Option<&[String]>,
        remote: Option<bool>,
    ) {
        for run in self.runs() {
            let mut state = run.state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(apps) = apps {
                state.computer_apps = apps.to_vec();
            }
            if let Some(remote) = remote {
                state.computer_remote = remote;
            }
        }
    }
}

pub(super) fn cancellable_jobs(state: &State) -> Vec<usize> {
    if state.phase != Phase::Running || state.stop.requested() || state.active_graph_stops.is_none()
    {
        return vec![];
    }
    state.task.as_ref().map_or_else(Vec::new, |task| {
        task.steps
            .iter()
            .enumerate()
            .filter(|(_, step)| {
                matches!(
                    step.status.as_str(),
                    "pending" | "running" | "waiting_for_permission"
                )
            })
            .map(|(index, _)| index)
            .collect()
    })
}

#[cfg(test)]
mod tests;

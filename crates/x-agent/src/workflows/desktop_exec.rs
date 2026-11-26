//! Reviewed desktop graph nodes reuse the sequential workflow step contract.
//! The shared native input lease is acquired only around mutations and readback.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::dag::{Graph, Job, Node};
use super::engine::{Config, Host, Reversal, StepStatus};
use super::model::{Step, StepAction, Workflow};
use super::recovery::{Begin, Journal};
use super::scheduler::{Executor, Outcome};
use super::{paths, steps};
use crate::cancellation::Cancellation;
use crate::grants::{Grants, Permission, Receipt, Scope};
use crate::tools::{self, Action, ExecutionOutcome};
use crate::wm::{Cap, WinInfo};

#[cfg(test)]
mod tests;

pub(crate) type AcquireInput =
    Arc<dyn Fn(&Cancellation) -> Result<Box<dyn Send>, String> + Send + Sync>;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct WindowKey {
    id: String,
    pid: u32,
    class: String,
}

impl WindowKey {
    fn new(win: &WinInfo) -> Result<Self, String> {
        if win.pid == 0
            || win.id.is_empty()
            || win.id.len() > 200
            || win.class.len() > 200
            || win
                .id
                .chars()
                .chain(win.class.chars())
                .any(char::is_control)
        {
            return Err("The desktop did not provide a stable window identity".into());
        }
        Ok(Self {
            id: win.id.clone(),
            pid: win.pid,
            class: win.class.clone(),
        })
    }

    fn current(&self, host: &dyn Host) -> Result<WinInfo, String> {
        host.windows()?
            .into_iter()
            .find(|win| win.id == self.id && win.pid == self.pid && win.class == self.class)
            .ok_or_else(|| "The exact reviewed window is gone or changed identity".into())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "binding", rename_all = "snake_case")]
enum Target {
    None,
    Absent { app: String },
    Existing { window: WindowKey },
    FromNode { node: String },
}

#[derive(Clone)]
pub(crate) struct Prepared {
    node: Node,
    target: Target,
    directory: Option<PathBuf>,
    actions: Vec<Action>,
}

impl Prepared {
    pub(crate) fn id(&self) -> &str {
        &self.node.id
    }

    pub(crate) fn description(&self) -> String {
        let Job::Desktop { action } = &self.node.job else {
            unreachable!()
        };
        let target = match &self.target {
            Target::None => String::new(),
            Target::Absent { app } => {
                format!(" · new {app} window; changed window state requires review")
            }
            Target::Existing { window } => format!(
                " · exact window {} / process {} / {}",
                window.id, window.pid, window.class
            ),
            Target::FromNode { node } => format!(" · exact window observed by predecessor {node}"),
        };
        format!(
            "{} · {}{target} · 15-second window wait limit",
            self.node.id,
            action.describe()
        )
    }

    pub(crate) fn scope(&self, run: Uuid, resolved: Option<&WinInfo>) -> Result<Scope, String> {
        let target = resolved.map(WindowKey::new).transpose()?;
        let intent = serde_json::to_string(&serde_json::json!({
            "node": self.node,
            "binding": self.target,
            "directory": self.directory,
            "actions": self.actions,
            "resolved_window": target,
        }))
        .map_err(|_| "Cannot represent the reviewed desktop operation")?;
        Ok(Scope::Desktop {
            run_id: run,
            node_id: self.node.id.clone(),
            intent,
        })
    }

    fn resolve(
        &self,
        host: &dyn Host,
        context: &Context,
        cfg: &Config,
    ) -> Result<Option<WinInfo>, String> {
        let Job::Desktop { action } = &self.node.job else {
            return Err("Not a desktop node".into());
        };
        if let StepAction::RunAction { phrase } = action
            && tools::plan_rules(phrase).as_ref() != Some(&self.actions)
        {
            return Err("The reviewed action phrase changed its executable actions".into());
        }
        if let StepAction::OpenDirectory { directory } | StepAction::OpenTerminal { directory, .. } =
            action
            && paths::existing(directory, &cfg.home).ok().as_ref() != self.directory.as_ref()
        {
            return Err("The reviewed desktop directory changed".into());
        }
        match &self.target {
            Target::None => Ok(None),
            Target::Absent { app } => {
                if steps::windows_of(host, app)?.is_empty() {
                    Ok(None)
                } else {
                    Err(
                        "An app window appeared after review; choose the current target again"
                            .into(),
                    )
                }
            }
            Target::Existing { window } => window.current(host).map(Some),
            Target::FromNode { node } => {
                let win = context
                    .windows
                    .get(node)
                    .ok_or("The required predecessor did not prove a window")?;
                WindowKey::new(win)?.current(host).map(Some)
            }
        }
    }
}

fn step(node: &Node) -> Result<Step, String> {
    let Job::Desktop { action } = &node.job else {
        return Err("Not a desktop node".into());
    };
    Ok(Step {
        id: node.id.clone(),
        action: action.clone(),
        requires: vec![],
        timeout_secs: None,
        on_failure: node.on_failure,
    })
}

fn opens_app(action: &StepAction) -> Option<&str> {
    match action {
        StepAction::OpenApp { app, .. } => Some(app),
        StepAction::OpenTerminal { .. } => Some("terminal"),
        StepAction::OpenDirectory { .. } => Some("files"),
        _ => None,
    }
}

pub(crate) fn prepare(
    graph: &Graph,
    host: &dyn Host,
    cfg: &Config,
) -> Result<Vec<Prepared>, String> {
    graph.validate_with_home(&cfg.home)?;
    let order = graph.order()?;
    let ordered = order
        .iter()
        .filter_map(|id| graph.nodes.iter().find(|node| &node.id == id))
        .filter(|node| matches!(node.job, Job::Desktop { .. }))
        .collect::<Vec<_>>();
    if ordered.is_empty() {
        return Ok(vec![]);
    }
    let desktop = Workflow {
        id: graph.id.clone(),
        name: graph.name.clone(),
        triggers: vec![],
        steps: ordered
            .iter()
            .map(|node| step(node))
            .collect::<Result<_, _>>()?,
    };
    let problems = steps::preflight(&desktop, host, cfg);
    if !problems.is_empty() {
        return Err(format!("Nothing started: {}", problems.join("; ")));
    }
    ordered.into_iter().map(|node| {
        let Job::Desktop { action } = &node.job else { unreachable!() };
        let directory = match action {
            StepAction::OpenDirectory { directory } | StepAction::OpenTerminal { directory, .. } => Some(paths::existing(directory, &cfg.home)?),
            _ => None,
        };
        let actions = match action {
            StepAction::OpenApp { app, .. } => vec![Action::App { target: app.clone() }],
            StepAction::OpenDirectory { .. } => vec![Action::Folder { directory: directory.as_ref().unwrap().display().to_string() }],
            StepAction::OpenTerminal { .. } => vec![Action::Terminal { directory: directory.as_ref().unwrap().display().to_string() }],
            StepAction::OpenUrl { url } => vec![Action::Url { target: url.clone(), browser: String::new() }],
            StepAction::RunAction { phrase } => {
                let actions = tools::plan_rules(phrase).ok_or("The action phrase is no longer understood")?;
                if actions.iter().any(|action| matches!(action,
                    Action::Window { .. } | Action::Computer { .. } | Action::Type { .. }
                        | Action::Opencode | Action::Session { .. })) {
                    return Err("Use a typed window or harness node; interactive computer actions require their own scoped service plan".into());
                }
                actions
            }
            StepAction::OpencodeTask { .. } => return Err("Use a typed harness node for coding work in a graph".into()),
            _ => vec![],
        };
        let target = match action {
            StepAction::FocusWindow { app, pick } | StepAction::MoveWindow { app, pick, .. } => {
                let producers = graph.nodes.iter().filter(|source| graph.depends_on(&node.id, &source.id))
                    .filter(|source| matches!(&source.job, Job::Desktop { action } if opens_app(action) == Some(app.as_str())))
                    .collect::<Vec<_>>();
                match producers.as_slice() {
                    [] => Target::Existing { window: WindowKey::new(&steps::pick_window(host, app, *pick)?)? },
                    [source] if pick.is_none() => Target::FromNode { node: source.id.clone() },
                    _ => return Err("The desktop target has multiple possible predecessor windows; review an unambiguous opener".into()),
                }
            }
            StepAction::OpenApp { app, .. } => {
                let existing = steps::windows_of(host, app)?;
                match existing.as_slice() {
                    [] => Target::Absent { app: app.clone() },
                    [win] => Target::Existing { window: WindowKey::new(win)? },
                    _ => return Err("More than one app window is open; choose an exact window step".into()),
                }
            }
            _ => Target::None,
        };
        Ok(Prepared { node: node.clone(), target, directory, actions })
    }).collect()
}

#[derive(Default)]
struct Context {
    windows: HashMap<String, WinInfo>,
    opened: HashMap<String, WinInfo>,
    reversals: Vec<Reversal>,
}

struct Assignment {
    prepared: Prepared,
    receipt: Receipt,
    scope: Scope,
    fingerprint: String,
}

pub(crate) struct DesktopExecutor {
    run: Uuid,
    assignments: HashMap<String, Assignment>,
    host: Arc<dyn Host>,
    cfg: Config,
    acquire: AcquireInput,
    grants: Arc<Grants>,
    journal: Arc<Journal>,
    context: Mutex<Context>,
    reasons: Mutex<HashMap<String, String>>,
    execution: Mutex<HashMap<String, ExecutionOutcome>>,
}

impl DesktopExecutor {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        run: Uuid,
        plans: Vec<(Prepared, Receipt)>,
        host: Arc<dyn Host>,
        cfg: Config,
        acquire: AcquireInput,
        grants: Arc<Grants>,
        journal: Arc<Journal>,
    ) -> Result<Self, String> {
        let mut assignments = HashMap::new();
        for (prepared, receipt) in plans {
            let scope = prepared.scope(run, None)?;
            grants.check(&receipt, Permission::QueueDesktop, &scope)?;
            let Scope::Desktop { intent, .. } = &scope else {
                unreachable!()
            };
            let fingerprint = Sha256::digest(intent)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let id = prepared.id().to_owned();
            if assignments
                .insert(
                    id,
                    Assignment {
                        prepared,
                        receipt,
                        scope,
                        fingerprint,
                    },
                )
                .is_some()
            {
                return Err("Duplicate desktop assignments".into());
            }
        }
        Ok(Self {
            run,
            assignments,
            host,
            cfg,
            acquire,
            grants,
            journal,
            context: Mutex::new(Context::default()),
            reasons: Mutex::new(HashMap::new()),
            execution: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn reversals(&self) -> Vec<Reversal> {
        self.context
            .lock()
            .map(|context| context.reversals.clone())
            .unwrap_or_default()
    }

    pub(crate) fn execution(&self, node: &str) -> Option<ExecutionOutcome> {
        self.execution.lock().ok()?.get(node).cloned()
    }

    fn note(&self, node: &str, reason: String) {
        if let Ok(mut notes) = self.reasons.lock() {
            notes.insert(node.into(), reason);
        }
    }

    fn run(
        &self,
        assignment: &Assignment,
        stop: &Cancellation,
    ) -> Result<(Outcome, super::engine::StepResult), String> {
        stop.check()?;
        let context = self
            .context
            .lock()
            .map_err(|_| "Desktop evidence unavailable")?;
        let target = assignment
            .prepared
            .resolve(self.host.as_ref(), &context, &self.cfg)?;
        let opened = context.opened.clone();
        drop(context);
        let scope = assignment.prepared.scope(self.run, target.as_ref())?;
        self.grants.activate_job(
            &assignment.receipt,
            &assignment.scope,
            &scope,
            Permission::UseDesktop,
        )?;
        let step = step(&assignment.prepared.node)?;
        let host = NodeHost {
            inner: self.host.as_ref(),
            prepared: &assignment.prepared,
            stop,
            acquire: &self.acquire,
            target: target.as_ref(),
            launched: AtomicBool::new(false),
            consumed: AtomicBool::new(false),
            grants: &self.grants,
            receipt: &assignment.receipt,
            scope: &scope,
        };
        if !host.permit(&step.action) {
            return Err("The exact desktop action is no longer permitted".into());
        }
        if matches!(step.action, StepAction::WaitForWindow { .. }) {
            host.consume_dispatch()?;
        }
        let result = steps::run_step_bound(
            &step,
            &host,
            &self.cfg,
            &opened,
            target.as_ref(),
            &mut |_| {},
        );
        if !host.consumed.load(Ordering::SeqCst)
            && matches!(result.status, StepStatus::Failed | StepStatus::Cancelled)
        {
            return Err(result.detail);
        }
        host.consume_dispatch()?;
        let outcome = if stop.requested() {
            Outcome::Cancelled
        } else {
            match result.status {
                StepStatus::Done => Outcome::Verified,
                StepStatus::Unverified if result.launched => Outcome::Launched,
                StepStatus::Unverified => Outcome::Unverified,
                StepStatus::Cancelled => Outcome::Cancelled,
                _ => Outcome::Failed,
            }
        };
        Ok((outcome, result))
    }
}

impl Executor for DesktopExecutor {
    fn authorized(&self, node: &Node) -> bool {
        self.assignments.get(&node.id).is_some_and(|assignment| {
            assignment.prepared.node == *node
                && self
                    .grants
                    .check(
                        &assignment.receipt,
                        Permission::QueueDesktop,
                        &assignment.scope,
                    )
                    .is_ok()
        })
    }

    fn outcome_reason(&self, node: &Node) -> Option<String> {
        self.reasons.lock().ok()?.get(&node.id).cloned()
    }

    fn execute(&self, node: Node, stop: Cancellation) -> Outcome {
        if !self.authorized(&node) {
            return Outcome::Failed;
        }
        if stop.requested() {
            return Outcome::Cancelled;
        }
        let assignment = &self.assignments[&node.id];
        let claim = match self
            .journal
            .begin_desktop(self.run, &node.id, &assignment.fingerprint)
        {
            Ok(Begin::Fresh(claim)) => claim,
            _ => return Outcome::Unverified,
        };
        let (outcome, result) = match self.run(assignment, &stop) {
            Ok((outcome, result)) => (outcome, Some(result)),
            Err(reason) => {
                self.note(&node.id, reason);
                (
                    if stop.requested() {
                        Outcome::Cancelled
                    } else {
                        Outcome::Failed
                    },
                    None,
                )
            }
        };
        if self.journal.settle_desktop(&claim, outcome).is_err() {
            return Outcome::Unverified;
        }
        if let Some(result) = result {
            let execution = result.execution.clone().unwrap_or_else(|| match outcome {
                Outcome::Verified => match node.job {
                    Job::Desktop {
                        action: StepAction::WaitForWindow { .. },
                    } => ExecutionOutcome::observed(
                        &result.detail,
                        tools::EffectEvidence::NativeWindowObservation,
                    ),
                    _ => ExecutionOutcome::confirmed(
                        &result.detail,
                        tools::EffectEvidence::NativeWindowReadback,
                    ),
                },
                Outcome::Launched => ExecutionOutcome::launched(&result.detail),
                Outcome::Failed => ExecutionOutcome::failed(&result.detail),
                _ => ExecutionOutcome::unverified(&result.detail),
            });
            if let Ok(mut records) = self.execution.lock() {
                records.insert(node.id.clone(), execution);
            }
            self.note(&node.id, result.detail);
            let Ok(mut context) = self.context.lock() else {
                return Outcome::Unverified;
            };
            context.reversals.extend(result.reversals);
            if outcome == Outcome::Verified {
                if let Some(win) = result.window {
                    context.windows.insert(node.id.clone(), win);
                }
                if let Some(win) = result.opened {
                    context.opened.insert(node.id, win);
                }
            }
        }
        outcome
    }
}

impl Drop for DesktopExecutor {
    fn drop(&mut self) {
        for assignment in self.assignments.values() {
            let _ = self.grants.revoke(&assignment.receipt);
        }
    }
}

struct NodeHost<'a> {
    inner: &'a dyn Host,
    prepared: &'a Prepared,
    stop: &'a Cancellation,
    acquire: &'a AcquireInput,
    target: Option<&'a WinInfo>,
    launched: AtomicBool,
    consumed: AtomicBool,
    grants: &'a Grants,
    receipt: &'a Receipt,
    scope: &'a Scope,
}

impl NodeHost<'_> {
    fn consume_dispatch(&self) -> Result<(), String> {
        self.stop.check()?;
        if !self.consumed.load(Ordering::SeqCst) {
            self.grants
                .consume(self.receipt, Permission::UseDesktop, self.scope)?;
            self.consumed.store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    fn mutation<T>(&self, action: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        self.stop.check()?;
        let _lease = (self.acquire)(self.stop)?;
        self.stop.check()?;
        self.consume_dispatch()?;
        action()
    }

    fn execute(&self, action: &Action) -> Result<ExecutionOutcome, String> {
        if !self.prepared.actions.contains(action) {
            return Err("The action changed after desktop review".into());
        }
        self.mutation(|| {
            if let Target::Absent { app } = &self.prepared.target
                && !steps::windows_of(self.inner, app)?.is_empty()
            {
                return Err("An app window appeared before dispatch; nothing was launched".into());
            }
            self.stop.check()?;
            let result = self
                .inner
                .execute_action_scoped(action, self.stop)
                .map(ExecutionOutcome::checked)?;
            if result.state != tools::EffectState::Failed {
                self.launched.store(true, Ordering::SeqCst);
            }
            Ok(result)
        })
    }
}

impl Host for NodeHost<'_> {
    fn windows(&self) -> Result<Vec<WinInfo>, String> {
        self.stop.check()?;
        self.inner.windows()
    }
    fn focused(&self) -> Result<Option<WinInfo>, String> {
        self.inner.focused()
    }
    fn workspace(&self) -> Result<i64, String> {
        self.inner.workspace()
    }
    fn launch(&self, action: &Action) -> Result<String, String> {
        let result = self.execute(action)?;
        if result.state == tools::EffectState::Failed {
            Err(result.message)
        } else {
            Ok(result.message)
        }
    }
    fn execute_action(&self, action: &Action) -> Result<ExecutionOutcome, String> {
        self.execute(action)
    }
    fn installed(&self, action: &Action) -> Result<(), String> {
        self.inner.installed(action)
    }
    fn can(&self, cap: Cap) -> bool {
        self.inner.can(cap)
    }
    fn focus(&self, win: &WinInfo) -> Result<(), String> {
        self.mutation(|| {
            let key = WindowKey::new(win)?;
            let current = key.current(self.inner)?;
            self.stop.check()?;
            self.inner.focus_scoped(&current, self.stop)?;
            if self
                .inner
                .focused()?
                .as_ref()
                .is_some_and(|focused| WindowKey::new(focused).ok().as_ref() == Some(&key))
            {
                Ok(())
            } else {
                Err("The exact window did not gain focus".into())
            }
        })
    }
    fn switch_workspace(&self, workspace: i64) -> Result<(), String> {
        self.mutation(|| {
            self.inner.switch_workspace_scoped(workspace, self.stop)?;
            if self.inner.workspace()? == workspace {
                Ok(())
            } else {
                Err("The workspace switch was not observed".into())
            }
        })
    }
    fn move_window(&self, win: &WinInfo, workspace: i64) -> Result<(), String> {
        self.mutation(|| {
            let key = WindowKey::new(win)?;
            if self.target.is_none() && !self.launched.load(Ordering::SeqCst) {
                return Err("No approved launch established this new window target".into());
            }
            let current = key.current(self.inner)?;
            self.stop.check()?;
            self.inner
                .move_window_scoped(&current, workspace, self.stop)?;
            if key.current(self.inner)?.workspace == workspace {
                Ok(())
            } else {
                Err("The exact window did not move to its approved workspace".into())
            }
        })
    }
    fn close_window(&self, win: &WinInfo) -> Result<(), String> {
        self.mutation(|| {
            let key = WindowKey::new(win)?;
            let current = key.current(self.inner)?;
            self.stop.check()?;
            self.inner.close_window_scoped(&current, self.stop)?;
            if self
                .inner
                .windows()?
                .iter()
                .any(|win| WindowKey::new(win).ok().as_ref() == Some(&key))
            {
                Err("The exact opened window remains present".into())
            } else {
                Ok(())
            }
        })
    }
    fn opencode_ready(&self) -> Result<(), String> {
        Err("Desktop graph nodes cannot start coding".into())
    }
    fn opencode(&self, _: &str, _: &Path) -> Result<String, String> {
        Err("Desktop graph nodes cannot start coding".into())
    }
    fn permit(&self, action: &StepAction) -> bool {
        !self.cancelled()
            && matches!(&self.prepared.node.job, Job::Desktop { action: approved } if approved == action)
            && self.inner.permit(action)
    }
    fn cancelled(&self) -> bool {
        self.stop.requested() || self.inner.cancelled()
    }
}

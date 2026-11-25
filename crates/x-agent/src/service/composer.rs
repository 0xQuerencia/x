//! Native Work drafts use the same project identities, approval gate and graph
//! runner as imported workflows. A draft is a proposal, never a job grant.

use std::io::Write;

use crate::harness::{Capability, Descriptor, Kind, Session, WorkAccess};
use crate::projects::Project;
use crate::verification::test_runner::Suite;
use crate::workflows::dag::{self, ArtifactInput, Graph, HarnessKind, InputProof, Job, Node};
use crate::workflows::model::OnFailure;

use super::{Mode, PlanOptions, Service, State, Task, busy};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    WorkspaceWrite,
    Analysis,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::WorkspaceWrite => "Workspace writer",
            Self::Analysis => "Tool-free analysis",
        }
    }

    pub fn supported_by(self, provider: &Descriptor) -> bool {
        provider.capabilities.contains(&Capability::OwnedRun)
            && match self {
                Self::WorkspaceWrite => provider.supports_workspace_write(),
                Self::Analysis => provider
                    .capabilities
                    .contains(&Capability::ToolFreeAnalysis),
            }
    }
}

#[derive(Clone, Debug)]
pub struct Draft {
    pub title: String,
    pub goal: String,
    pub acceptance: String,
    pub provider: Kind,
    pub role: Role,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dependency {
    Independent,
    FirstArtifact,
}

#[derive(Clone, Debug)]
pub enum SessionChoice {
    Fresh,
    /// Pinned by the native picker, then compared to the live selected session.
    Selected(Session),
}

#[derive(Clone, Debug)]
pub struct Request {
    /// The opaque identity and directory from the registered project picker.
    pub project: Project,
    pub first: Draft,
    pub second: Option<Draft>,
    pub dependency: Dependency,
    pub worker_limit: usize,
    pub test_suite: Option<Suite>,
    pub session: SessionChoice,
}

/// Checked again while the standard Work planner holds its State lock.
pub(super) struct Selection {
    project: Project,
    provider: Kind,
    session: Option<Session>,
}

impl Service {
    pub(super) fn check_composer_selection(
        &self,
        selection: &Selection,
        state: &State,
    ) -> Result<(), String> {
        if state.workspace.as_ref() != Some(&selection.project.root) {
            return Err(
                "The selected project changed. Select the project and preview again.".into(),
            );
        }
        let registry = self
            .session_store()
            .map(|(registry, _)| registry)
            .ok_or("Project storage is not configured")?;
        let project = registry.validated_resolve(&selection.project.id.to_string())?;
        if project.root != selection.project.root
            || project.git_common_dir != selection.project.git_common_dir
        {
            return Err(
                "The registered project changed. Select it again before previewing.".into(),
            );
        }
        if let Some(session) = &selection.session {
            let selected = match selection.provider {
                Kind::OpenCode => &state.selected_session,
                Kind::ClaudeCode => &state.claude_session,
            };
            if !selected.as_ref().is_some_and(|current| {
                current.id == session.id && current.workspace == session.workspace
            }) || session.workspace != project.root
                || session.id.kind() != selection.provider
            {
                return Err(
                    "The selected session changed. Choose a session and preview again.".into(),
                );
            }
        }
        Ok(())
    }

    fn check_composer_provider(&self, draft: &Draft) -> Result<(), String> {
        let descriptor = self
            .work_adapter(draft.provider)
            .ok_or_else(|| format!("{} is unavailable", draft.provider.label()))?
            .descriptor();
        if descriptor.kind != draft.provider || !draft.role.supported_by(&descriptor) {
            return Err(format!(
                "{} cannot be used as {}. Choose a provider with that capability.",
                draft.provider.label(),
                draft.role.label().to_lowercase()
            ));
        }
        Ok(())
    }

    /// Save a validated two-task workflow and preview it, or preview ordinary
    /// single-task Work. Neither path launches providers, tests or worktrees.
    pub fn plan_work_composer(&self, request: &Request) -> Result<Task, String> {
        let session = match &request.session {
            SessionChoice::Fresh => None,
            SessionChoice::Selected(session) => Some(session.clone()),
        };
        let selection = Selection {
            project: request.project.clone(),
            provider: request.first.provider,
            session: session.clone(),
        };
        {
            let state = self.lock();
            if state.phase.is_busy() {
                return Err(busy(state.phase));
            }
            self.check_composer_selection(&selection, &state)?;
        }
        self.check_composer_provider(&request.first)?;
        if let Some(second) = &request.second {
            self.check_composer_provider(second)?;
            let graph = graph_from_request(request)?;
            // Configuration is required before writing a saved graph. The
            // existing graph planner uses these same storage and home roots.
            let home = self
                .workflow_dirs
                .lock()
                .map_err(|_| "Workflow storage is unavailable")?
                .as_ref()
                .map(|(_, home)| home.clone())
                .ok_or("Workflow storage is not configured")?;
            graph.validate_with_home(&home)?;
            let mut staged = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
            serde_json::to_writer(&mut staged, &graph).map_err(|error| error.to_string())?;
            staged.flush().map_err(|error| error.to_string())?;
            self.workflow_store().import_graph(staged.path())?;
            return self
                .plan_graph_workflow_pinned(&graph.id, Some(&request.project))
                .map_err(|error| {
                    format!(
                        "Saved {} in Workflows, but could not preview it: {error}",
                        graph.name
                    )
                });
        }
        if request.worker_limit != 1
            || request.test_suite.is_some()
            || request.dependency != Dependency::Independent
        {
            return Err(
                "A single task uses one worker. Choose two tasks for dependencies and tests."
                    .into(),
            );
        }
        let goal = goal(&request.first)?;
        self.plan_task(
            &goal,
            Mode::Work,
            None,
            PlanOptions {
                session,
                work_harness: request.first.provider,
                work_access: match request.first.role {
                    Role::WorkspaceWrite => WorkAccess::Workspace,
                    Role::Analysis => WorkAccess::SelectionOnly,
                },
                composer_selection: Some(selection),
                ..PlanOptions::default()
            },
        )
    }
}

fn goal(draft: &Draft) -> Result<String, String> {
    fn field(value: &str) -> Result<String, String> {
        if value
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
        {
            return Err("Task text contains unsupported control characters".into());
        }
        Ok(value.split_whitespace().collect::<Vec<_>>().join(" "))
    }
    let title = field(&draft.title)?;
    let goal = field(&draft.goal)?;
    let acceptance = field(&draft.acceptance)?;
    if title.is_empty() || title.chars().count() > 80 || goal.is_empty() {
        return Err("Enter a task name of 1–80 characters and a goal for each task".into());
    }
    let text = if acceptance.is_empty() {
        format!("{title}: {goal}")
    } else {
        format!("{title}: {goal} Acceptance criteria: {acceptance}")
    };
    if text.chars().count() > 4_096 {
        return Err("Keep each task's name, goal and acceptance within 4,096 characters".into());
    }
    Ok(text)
}

fn graph_from_request(request: &Request) -> Result<Graph, String> {
    let second = request.second.as_ref().ok_or("Enter the second task")?;
    if !(1..=2).contains(&request.worker_limit) {
        return Err("Choose a worker limit of one or two".into());
    }
    if !matches!(request.session, SessionChoice::Fresh) {
        return Err("Two-task workflows start fresh sessions in separate owned worktrees".into());
    }
    if [request.first.role, second.role]
        .iter()
        .any(|role| *role != Role::WorkspaceWrite)
    {
        return Err(
            "Both tasks need a workspace writer; tool-free analysis cannot produce code artifacts"
                .into(),
        );
    }
    let writer = |id: &str, draft: &Draft| -> Result<Node, String> {
        Ok(Node {
            id: id.into(),
            after: vec![],
            requires: vec![],
            requires_verified: vec![],
            required_change: None,
            required_commit: true,
            inputs: vec![],
            job: Job::Harness {
                harness: match draft.provider {
                    Kind::OpenCode => HarnessKind::OpenCode,
                    Kind::ClaudeCode => HarnessKind::ClaudeCode,
                },
                project_id: request.project.id.to_string(),
                goal: goal(draft)?,
            },
            on_failure: OnFailure::Stop,
        })
    };
    let first = writer("task-1", &request.first)?;
    let mut second = writer("task-2", second)?;
    let input = |node: &str| ArtifactInput {
        node: node.into(),
        proof: InputProof::Committed,
    };
    match request.dependency {
        Dependency::FirstArtifact => second.inputs.push(input("task-1")),
        Dependency::Independent if request.worker_limit == 1 => second.after.push("task-1".into()),
        Dependency::Independent => {}
    }
    let mut nodes = vec![first, second];
    if let Some(suite) = request.test_suite {
        nodes.push(Node {
            id: "test-results".into(),
            after: vec![],
            requires: vec![],
            requires_verified: vec![],
            required_change: None,
            required_commit: false,
            inputs: match request.dependency {
                Dependency::Independent => vec![input("task-1"), input("task-2")],
                Dependency::FirstArtifact => vec![input("task-2")],
            },
            job: Job::Test {
                project_id: request.project.id.to_string(),
                suite,
            },
            on_failure: OnFailure::Stop,
        });
    }
    let graph = Graph {
        version: dag::ARTIFACT_VERSION,
        id: format!("work-{}", uuid::Uuid::new_v4().simple()),
        name: format!("{} · two tasks", request.project.name)
            .chars()
            .take(80)
            .collect(),
        triggers: vec![],
        roadmap: None,
        nodes,
    };
    graph.validate()?;
    Ok(graph)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::cancellation::Cancellation;
    use crate::harness::{self, Adapter};
    use crate::hexcli::{Stt, SttError};
    use crate::phase::Phase;
    use crate::projects::Registry;
    use crate::service::Launcher;
    use crate::tools::Action;

    struct NoEffects;
    impl Stt for NoEffects {
        fn start(&self) -> Result<(), SttError> {
            panic!("preview must not record")
        }
        fn stop_print(&self) -> Result<String, SttError> {
            panic!("preview must not transcribe")
        }
        fn cancel(&self) -> Result<(), SttError> {
            Ok(())
        }
    }
    impl Launcher for NoEffects {
        fn launch(&self, _: &Action) -> Result<String, String> {
            panic!("preview must not launch")
        }
    }

    struct Provider {
        starts: Arc<AtomicUsize>,
    }
    impl Adapter for Provider {
        fn descriptor(&self) -> Descriptor {
            Descriptor {
                kind: Kind::OpenCode,
                event_schema: 1,
                version: Some("fixture".into()),
                capabilities: &[Capability::OwnedRun, Capability::WorkspaceWrite],
            }
        }
        fn start(
            &self,
            _: harness::Request,
            _: harness::EventSink,
            _: &Cancellation,
        ) -> Result<Box<dyn harness::OwnedRun>, String> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Err("fixture never starts providers".into())
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        service: Service,
        request: Request,
        starts: Arc<AtomicUsize>,
        registry: Registry,
        home: PathBuf,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = home.join("app");
        let support = home.join("support");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@example.invalid"],
            vec!["config", "user.name", "Fixture"],
            vec!["commit", "--allow-empty", "-qm", "initial"],
        ] {
            let output = Command::new("git")
                .current_dir(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let registry = Registry::new(support.clone(), home.clone());
        let (project, _) = registry
            .register("Example app", repo.to_str().unwrap())
            .unwrap();
        let starts = Arc::new(AtomicUsize::new(0));
        let service = Service::with_harness(
            Arc::new(NoEffects),
            Arc::new(NoEffects),
            Some(Arc::new(Provider {
                starts: starts.clone(),
            })),
        );
        service.configure_session_storage(support.clone(), home.clone());
        service.set_workflow_dirs(support.join("workflows"), home.clone());
        service
            .select_registered_project(&project.id.to_string())
            .unwrap();
        let draft = |title: &str, goal: &str| Draft {
            title: title.into(),
            goal: goal.into(),
            acceptance: "Tests pass".into(),
            provider: Kind::OpenCode,
            role: Role::WorkspaceWrite,
        };
        let request = Request {
            project,
            first: draft("UI", "Implement the form"),
            second: Some(draft("Backend", "Implement the endpoint")),
            dependency: Dependency::Independent,
            worker_limit: 2,
            test_suite: Some(Suite::CargoAgent),
            session: SessionChoice::Fresh,
        };
        Fixture {
            _temp: temp,
            service,
            request,
            starts,
            registry,
            home,
        }
    }

    #[test]
    fn preview_saves_exact_artifact_recipe_without_dispatch_or_grants() {
        let f = fixture();
        f.service.set_trust_actions(true);
        let task = f.service.plan_work_composer(&f.request).unwrap();
        assert_eq!(task.phase, Phase::AwaitingApproval);
        assert_eq!(task.steps.len(), 3);
        assert!(
            task.steps[2]
                .title
                .contains("cargo test --locked -p x-agent --all-targets")
        );
        assert_eq!(f.starts.load(Ordering::SeqCst), 0);
        assert_eq!(f.service.snapshot(0).pending_job_approvals, Some(0));
        assert!(!f.home.join("X Worktrees").exists());
        let entries = f.service.workflow_store().list_graphs();
        assert_eq!(entries.len(), 1);
        let graph = f
            .service
            .workflow_store()
            .get_graph(&entries[0].id)
            .unwrap();
        assert_eq!(graph.version, dag::ARTIFACT_VERSION);
        assert!(graph.nodes[0].inputs.is_empty() && graph.nodes[1].inputs.is_empty());
        assert_eq!(
            graph.nodes[2]
                .inputs
                .iter()
                .map(|input| input.node.as_str())
                .collect::<Vec<_>>(),
            ["task-1", "task-2"]
        );
    }

    #[test]
    fn dependency_uses_first_commit_and_tests_the_descendant() {
        let mut f = fixture();
        f.request.dependency = Dependency::FirstArtifact;
        let graph = graph_from_request(&f.request).unwrap();
        assert_eq!(
            graph.nodes[1].inputs,
            [ArtifactInput {
                node: "task-1".into(),
                proof: InputProof::Committed
            }]
        );
        assert_eq!(
            graph.nodes[2].inputs,
            [ArtifactInput {
                node: "task-2".into(),
                proof: InputProof::Committed
            }]
        );
        graph.validate().unwrap();
    }

    #[test]
    fn one_worker_orders_independent_tasks_without_importing_the_first_artifact() {
        let mut f = fixture();
        f.request.worker_limit = 1;
        let graph = graph_from_request(&f.request).unwrap();
        assert_eq!(graph.nodes[1].after, ["task-1"]);
        assert!(graph.nodes[1].inputs.is_empty());
        assert_eq!(graph.nodes[2].inputs.len(), 2);
    }

    #[test]
    fn invalid_drafts_and_tool_free_writers_do_not_save_or_plan() {
        let mut f = fixture();
        f.request.second.as_mut().unwrap().goal.clear();
        assert!(
            f.service
                .plan_work_composer(&f.request)
                .unwrap_err()
                .contains("goal")
        );
        f.request.second.as_mut().unwrap().goal = "Implement endpoint".into();
        f.request.second.as_mut().unwrap().role = Role::Analysis;
        assert!(
            f.service
                .plan_work_composer(&f.request)
                .unwrap_err()
                .contains("capability")
        );
        f.request.second.as_mut().unwrap().role = Role::WorkspaceWrite;
        f.request.worker_limit = 3;
        assert!(
            f.service
                .plan_work_composer(&f.request)
                .unwrap_err()
                .contains("worker limit")
        );
        assert!(f.service.workflow_store().list_graphs().is_empty());
        assert_eq!(f.service.snapshot(0).phase, Phase::Idle);
        assert_eq!(f.starts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn single_task_rechecks_the_exact_project_and_selected_session() {
        let mut f = fixture();
        f.request.second = None;
        f.request.worker_limit = 1;
        f.request.test_suite = None;
        let session = Session::from_open_code(
            "ses_selected".into(),
            "Design work",
            &f.request.project.root,
            &f.request.project.root,
            1,
        )
        .unwrap();
        f.request.session = SessionChoice::Selected(session);
        assert!(
            f.service
                .plan_work_composer(&f.request)
                .unwrap_err()
                .contains("session changed")
        );
        f.request.session = SessionChoice::Fresh;
        let other = f.home.join("other");
        std::fs::create_dir(&other).unwrap();
        f.service.set_workspace(other.to_str().unwrap()).unwrap();
        assert!(
            f.service
                .plan_work_composer(&f.request)
                .unwrap_err()
                .contains("project changed")
        );
        f.service
            .select_registered_project(&f.request.project.id.to_string())
            .unwrap();
        f.registry
            .forget(&f.request.project.id.to_string())
            .unwrap();
        assert!(f.service.plan_work_composer(&f.request).is_err());
        assert!(f.service.snapshot(0).task.is_none());
    }

    #[test]
    fn single_task_preserves_acceptance_and_waits_for_approval() {
        let mut f = fixture();
        f.request.second = None;
        f.request.worker_limit = 1;
        f.request.test_suite = None;
        f.request.first.goal = "Implement the form\nwith keyboard support".into();
        let task = f.service.plan_work_composer(&f.request).unwrap();
        assert!(task.text.contains("form with keyboard support"));
        assert!(task.text.contains("Acceptance criteria: Tests pass"));
        assert_eq!(task.workspace.as_deref(), f.request.project.root.to_str());
        assert_eq!(task.phase, Phase::AwaitingApproval);
        assert_eq!(f.starts.load(Ordering::SeqCst), 0);
        assert!(f.service.workflow_store().list_graphs().is_empty());
    }

    #[test]
    fn chosen_session_is_pinned_and_never_falls_back_to_fresh_work() {
        let mut f = fixture();
        f.service = f.service.with_session_verifier(|_| Ok(()));
        f.request.second = None;
        f.request.worker_limit = 1;
        f.request.test_suite = None;
        let session = Session::from_open_code(
            "ses_selected".into(),
            "Design work",
            &f.request.project.root,
            &f.request.project.root,
            1,
        )
        .unwrap();
        f.service.select_session(session.clone()).unwrap();
        f.request.session = SessionChoice::Selected(session.clone());
        let task = f.service.plan_work_composer(&f.request).unwrap();
        assert_eq!(task.session, Some(session));
        assert_eq!(f.starts.load(Ordering::SeqCst), 0);
        f.service.cancel();
        let replacement = Session::from_open_code(
            "ses_replacement".into(),
            "Other work",
            &f.request.project.root,
            &f.request.project.root,
            2,
        )
        .unwrap();
        f.service.select_session(replacement).unwrap();
        assert!(
            f.service
                .plan_work_composer(&f.request)
                .unwrap_err()
                .contains("session changed")
        );
        assert_eq!(f.starts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn tool_free_claude_can_preview_supplied_analysis_but_not_write() {
        let mut f = fixture();
        f.service = f
            .service
            .with_claude(Some("not-executed-during-preview".into()));
        f.request.second = None;
        f.request.worker_limit = 1;
        f.request.test_suite = None;
        f.request.first.provider = Kind::ClaudeCode;
        assert!(
            f.service
                .plan_work_composer(&f.request)
                .unwrap_err()
                .contains("capability")
        );
        f.request.first.role = Role::Analysis;
        f.request.first.goal =
            "Compare these two supplied API descriptions: A is synchronous, B returns a job ID"
                .into();
        let task = f.service.plan_work_composer(&f.request).unwrap();
        assert_eq!(task.work_access, WorkAccess::SelectionOnly);
        assert_eq!(task.work_harness, Some(Kind::ClaudeCode));
        assert_eq!(task.phase, Phase::AwaitingApproval);
        assert_eq!(f.starts.load(Ordering::SeqCst), 0);
    }
}

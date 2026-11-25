//! Bounded spoken mixed proposals; all steps go through the existing graph review.

use std::io::Write;

use crate::harness::Kind;
use crate::projects::Project;
use crate::tools::{self, Action};
use crate::verification::test_runner::Suite;
use crate::workflows::dag::{self, ArtifactInput, Graph, HarnessKind, InputProof, Job, Node};
use crate::workflows::model::{OnFailure, StepAction};

use super::{Service, Task, session_targets};

fn text(input: &str) -> &str {
    tools::strip_wake_word(input).trim()
}

pub(super) fn recognizes(input: &str) -> bool {
    let lower = text(input).to_ascii_lowercase();
    lower.starts_with("plan mixed ")
        || ((lower.starts_with("open ") || lower.starts_with("focus "))
            && lower.contains(" then ")
            && (lower.contains("ask opencode to ")
                || lower.contains("ask open code to ")
                || lower.contains("ask claude code to ")))
}

fn app(phrase: &str) -> Result<String, String> {
    match tools::plan_rules(phrase).as_deref() {
        Some([Action::App { target }]) => Ok(target.clone()),
        _ => Err("Choose a supported app by name in the mixed proposal".into()),
    }
}

fn graph(input: &str, project: &Project) -> Result<Graph, String> {
    let input = text(input);
    let lower = input.to_ascii_lowercase();
    let input = if lower.starts_with("plan mixed ") {
        &input["plan mixed ".len()..]
    } else {
        input
    };
    if input.chars().count() > 4_096
        || input.chars().any(char::is_control)
        || input.contains('"')
    {
        return Err("Use a short, unquoted sequence of explicit steps".into());
    }

    // ASCII lowercasing preserves byte offsets in the original utterance.
    let lower = input.to_ascii_lowercase();
    let mut start = 0;
    let mut clauses = Vec::new();
    for (end, separator) in lower.match_indices(" then ") {
        clauses.push(input[start..end].trim().trim_end_matches(',').trim());
        start = end + separator.len();
    }
    clauses.push(input[start..].trim());
    if !(2..=8).contains(&clauses.len()) || clauses.iter().any(|clause| clause.is_empty()) {
        return Err("Mixed Work needs two to eight explicit steps separated by then".into());
    }

    let mut nodes = Vec::<Node>::new();
    let mut writer = None::<String>;
    let mut tested = false;
    let mut has_desktop = false;
    for (index, clause) in clauses.into_iter().enumerate() {
        let id = format!("step-{}", index + 1);
        let lower = clause.to_ascii_lowercase();
        let mut inputs = vec![];
        let mut required_commit = false;
        let job = if let Some((kind, goal)) = session_targets::spoken_work(clause) {
            if kind != Kind::OpenCode || writer.is_some() || goal.is_empty() {
                return Err("Mixed Work supports one fresh workspace-capable OpenCode task".into());
            }
            required_commit = true;
            writer = Some(id.clone());
            Job::Harness {
                harness: HarnessKind::OpenCode,
                project_id: project.id.to_string(),
                goal,
            }
        } else if lower.starts_with("open ") {
            has_desktop = true;
            Job::Desktop {
                action: StepAction::OpenApp {
                    app: app(clause)?,
                    workspace: None,
                },
            }
        } else if lower.starts_with("focus ") {
            has_desktop = true;
            Job::Desktop {
                action: StepAction::FocusWindow {
                    app: app(&format!("open {}", &clause["focus ".len()..]))?,
                    pick: None,
                },
            }
        } else {
            let suite = match lower.as_str() {
                "run cargo tests" | "run cargo workspace tests" => Suite::CargoWorkspace,
                "run x-agent tests" => Suite::CargoAgent,
                "run npm tests" => Suite::Npm,
                "run pnpm tests" => Suite::Pnpm,
                "run pytest" => Suite::Pytest,
                "run go tests" => Suite::Go,
                _ => return Err(format!("Unsupported mixed step: {clause}")),
            };
            if tested {
                return Err("Choose one explicit test suite for this proposal".into());
            }
            let producer = writer.as_ref().ok_or("Tests need a preceding coding artifact")?;
            tested = true;
            inputs.push(ArtifactInput {
                node: producer.clone(),
                proof: InputProof::Committed,
            });
            Job::Test {
                project_id: project.id.to_string(),
                suite,
            }
        };
        nodes.push(Node {
            id,
            after: vec![],
            requires: vec![],
            requires_verified: nodes.last().map(|node| node.id.clone()).into_iter().collect(),
            required_change: None,
            required_commit,
            inputs,
            job,
            on_failure: OnFailure::Stop,
        });
    }
    if !has_desktop || writer.is_none() {
        return Err("Mixed Work needs both a desktop step and a coding task".into());
    }
    let graph = Graph {
        version: dag::ARTIFACT_VERSION,
        id: format!("mixed-{}", uuid::Uuid::new_v4().simple()),
        name: "Mixed Work".into(),
        triggers: vec![],
        roadmap: None,
        nodes,
    };
    graph.validate()?;
    Ok(graph)
}

impl Service {
    pub(super) fn plan_mixed_proposal(&self, input: &str) -> Result<Task, String> {
        let workspace = self.workspace_path().ok_or("Select a registered project first")?;
        let registry = self.session_store().map(|(registry, _)| registry)
            .ok_or("Project storage is not configured")?;
        let projects: Vec<_> = registry.list()?.into_iter()
            .filter(|project| project.root == workspace).collect();
        let [project] = projects.as_slice() else {
            return Err("Select one unambiguous registered project first".into());
        };
        let project = registry.validated_resolve(&project.id.to_string())?;
        let graph = graph(input, &project)?;
        let descriptor = self.work_adapter(Kind::OpenCode)
            .ok_or("OpenCode is unavailable")?.descriptor();
        if !descriptor.supports_workspace_write() {
            return Err("OpenCode must support owned workspace writes".into());
        }
        let home = self.workflow_dirs.lock()
            .map_err(|_| "Workflow storage is unavailable")?
            .as_ref().map(|(_, home)| home.clone())
            .ok_or("Workflow storage is not configured")?;
        graph.validate_with_home(&home)?;
        // The saved graph is only a proposal. The normal planner binds exact
        // project, Git base and desktop targets and requires fresh approval.
        let mut staged = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
        serde_json::to_writer(&mut staged, &graph).map_err(|error| error.to_string())?;
        staged.flush().map_err(|error| error.to_string())?;
        self.workflow_store().import_graph(staged.path())?;
        self.plan_graph_workflow_pinned(&graph.id, Some(&project))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_explicit_mixed_sequences() {
        assert!(recognizes("open VS Code then ask OpenCode to fix it then run cargo tests"));
        assert!(!recognizes("open VS Code"));
        assert!(!recognizes("ask OpenCode to fix it then explain the code"));
        assert!(!recognizes("I will open VS Code then ask OpenCode to fix it"));
    }

    #[test]
    fn mixed_graph_requires_proof_before_following_steps() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let registry = crate::projects::Registry::new(temp.path().join("state"), temp.path().into());
        let (project, _) = registry.register("Example", root.to_str().unwrap()).unwrap();
        let plan = graph(
            "open VS Code then ask OpenCode to fix the UI then run cargo tests then focus VS Code",
            &project,
        ).unwrap();
        assert_eq!(plan.nodes.len(), 4);
        assert!(plan.nodes[1].required_commit);
        assert_eq!(plan.nodes[2].inputs[0].node, "step-2");
        assert_eq!(plan.nodes[3].requires_verified, ["step-3"]);
        assert!(graph("open VS Code then ask OpenCode to fix it then delete everything", &project).is_err());
        assert!(graph("open VS Code then run cargo tests then ask OpenCode to fix it", &project).is_err());
    }
}

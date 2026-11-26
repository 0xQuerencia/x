//! Live local failure review and bounded owner-reviewed correction chains.
//! Test output is untrusted, is not persisted, and is never sent automatically.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;

use crate::harness::Kind;
use crate::projects::{Project, Registry};
use crate::verification::test_runner::{CorrectionDraft, Suite};
use crate::workflows::dag::{
    ARTIFACT_VERSION, ArtifactInput, Graph, HarnessKind, InputProof, Job, Node,
};
use crate::workflows::model::OnFailure;
use crate::workflows::recovery::{Journal, State};
use crate::worktrees::Manager;

use super::{Service, Task};

const MAX_RECORDS: usize = 16;
const MAX_CORRECTIONS: u8 = 2;
const RETENTION: Duration = Duration::from_secs(30 * 60);

/// Native and CLI views share the same bounded local diagnostic review.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CorrectionReview {
    pub failure: CorrectionDraft,
    pub next_attempt: Option<u8>,
    pub max_attempts: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Binding {
    pub(super) source_run: Uuid,
    pub(super) source_node: String,
    pub(super) project_id: Uuid,
    pub(super) commit: String,
    suite: Suite,
    root: Uuid,
    pub(super) attempt: u8,
}

impl Binding {
    pub(super) fn validate_source(
        &self,
        manager: &Manager,
        project: &Project,
        journal: &Journal,
    ) -> Result<(), String> {
        if project.id != self.project_id {
            return Err("The correction project changed".into());
        }
        let claim = journal
            .snapshot()?
            .into_iter()
            .find(|claim| claim.run_id == self.source_run && claim.node_id == self.source_node)
            .ok_or("The recorded test failure is unavailable")?;
        if claim.state != State::Failed
            || !claim.test_evidence.as_ref().is_some_and(|test| {
                test.passed == Some(false) && test.commit == self.commit && test.suite == self.suite
            })
            || !claim.worktree.as_ref().is_some_and(|reservation| {
                reservation.project_id() == self.project_id
                    && reservation.resolved_commit() == Some(self.commit.as_str())
            })
        {
            return Err("The recorded failure no longer matches this correction".into());
        }
        let reservation = claim.worktree.as_ref().unwrap();
        // Independently confirm the original X-owned path and Git identity.
        manager.inspect_reserved(project, self.source_run, &self.source_node, reservation)?;
        let source = manager.reattach(project, self.source_run, &self.source_node, &self.commit)?;
        manager.verify_unchanged(project, &source)
    }
}

struct Failure {
    draft: CorrectionDraft,
    root: Uuid,
    depth: u8,
    seen: Instant,
    spent: bool,
}

impl Failure {
    fn binding(&self) -> Result<Binding, String> {
        if self.spent || self.depth >= MAX_CORRECTIONS {
            return Err(
                "The correction source was already used or reached its two-attempt limit".into(),
            );
        }
        Ok(Binding {
            source_run: self.draft.run_id,
            source_node: self.draft.node_id.clone(),
            project_id: self.draft.project_id,
            commit: self.draft.commit.clone(),
            suite: self.draft.suite,
            root: self.root,
            attempt: self.depth + 1,
        })
    }
}

#[derive(Default)]
pub(super) struct History {
    records: VecDeque<Failure>,
}

impl History {
    fn prune(&mut self) {
        self.records
            .retain(|entry| entry.seen.elapsed() < RETENTION);
    }

    fn record(&mut self, draft: CorrectionDraft, parent: Option<&Binding>) {
        self.prune();
        if self
            .records
            .iter()
            .any(|entry| entry.draft.run_id == draft.run_id && entry.draft.node_id == draft.node_id)
        {
            return;
        }
        let (root, depth) = if let Some(parent) = parent {
            if draft.project_id != parent.project_id
                || draft.suite != parent.suite
                || draft.node_id != "correction-retest"
                || !(1..=MAX_CORRECTIONS).contains(&parent.attempt)
            {
                return;
            }
            (parent.root, parent.attempt)
        } else {
            (Uuid::new_v4(), 0)
        };
        if self.records.len() >= MAX_RECORDS {
            self.records.pop_front();
        }
        self.records.push_back(Failure {
            draft,
            root,
            depth,
            seen: Instant::now(),
            spent: false,
        });
    }

    fn find(&mut self, run: Uuid, node: &str) -> Result<&mut Failure, String> {
        self.prune();
        self.records
            .iter_mut()
            .find(|entry| entry.draft.run_id == run && entry.draft.node_id == node)
            .ok_or_else(|| "No live local failure review for that exact run/node".into())
    }

    pub(super) fn consume(&mut self, binding: &Binding) -> Result<(), String> {
        let entry = self.find(binding.source_run, &binding.source_node)?;
        if entry.binding()? != *binding {
            return Err("The correction review changed".into());
        }
        // Consumed at actual approval/dispatch, never while merely previewing.
        entry.spent = true;
        Ok(())
    }
}

fn identity(source: &str) -> Result<(Uuid, &str), String> {
    let (run, node) = source
        .split_once('/')
        .ok_or("Use the exact run UUID/node ID")?;
    let run = Uuid::parse_str(run)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or("Invalid run UUID")?;
    if node.is_empty()
        || node.len() > 40
        || !node.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err("Invalid node identity".into());
    }
    Ok((run, node))
}

fn correction_graph(binding: &Binding, goal: &str) -> Result<Graph, String> {
    let goal = goal.trim();
    if goal.is_empty() || goal.chars().any(char::is_control) {
        return Err("Enter an explicit single-line correction goal".into());
    }
    let project_id = binding.project_id.to_string();
    let writer = Node {
        id: "correction-write".into(),
        after: vec![],
        requires: vec![],
        requires_verified: vec![],
        required_change: None,
        required_commit: true,
        inputs: vec![],
        job: Job::Harness {
            harness: HarnessKind::OpenCode,
            project_id: project_id.clone(),
            goal: format!(
                "Correct the observed {} failure at {}. Owner goal: {goal}",
                binding.suite.label(),
                binding.commit
            ),
        },
        on_failure: OnFailure::Stop,
    };
    let retest = Node {
        id: "correction-retest".into(),
        after: vec![],
        requires: vec![],
        requires_verified: vec![],
        required_change: None,
        required_commit: false,
        inputs: vec![ArtifactInput {
            node: writer.id.clone(),
            proof: InputProof::Committed,
        }],
        job: Job::Test {
            project_id,
            suite: binding.suite,
        },
        on_failure: OnFailure::Stop,
    };
    let graph = Graph {
        version: ARTIFACT_VERSION,
        id: format!("fix-{}", Uuid::new_v4().simple()),
        name: format!("Reviewed correction {}/2", binding.attempt),
        triggers: vec![],
        roadmap: None,
        nodes: vec![writer, retest],
    };
    graph.validate()?;
    Ok(graph)
}

impl Service {
    pub(super) fn remember_correction(&self, draft: CorrectionDraft, parent: Option<&Binding>) {
        if let Ok(mut history) = self.corrections.lock() {
            history.record(draft, parent);
        }
    }

    /// Explicit operator-only lookup, not part of the automatic task snapshot.
    pub fn review_correction_failure(&self, source: &str) -> Result<CorrectionReview, String> {
        let (run, node) = identity(source)?;
        let mut history = self
            .corrections
            .lock()
            .map_err(|_| "Correction history is unavailable")?;
        let entry = history.find(run, node)?;
        Ok(CorrectionReview {
            failure: entry.draft.clone(),
            next_attempt: entry.binding().ok().map(|binding| binding.attempt),
            max_attempts: MAX_CORRECTIONS,
        })
    }

    pub fn correction_preview(&self, source: &str) -> Result<Value, String> {
        let review = self.review_correction_failure(source)?;
        Ok(json!({
            "failure": review.failure,
            "next_attempt": review.next_attempt,
            "executed": false,
            "note": "Local untrusted test output. Only your explicit correction goal is sent after separate approval.",
        }))
    }

    /// Build an ephemeral reviewed graph at the exact failed test commit. No
    /// provider dispatch or artifact integration is authorized by preview.
    pub fn plan_correction(&self, source: &str, goal: &str) -> Result<Task, String> {
        let (run, node) = identity(source)?;
        let binding = self
            .corrections
            .lock()
            .map_err(|_| "Correction history is unavailable")?
            .find(run, node)?
            .binding()?;
        let (dir, home) = self
            .workflow_dirs
            .lock()
            .map_err(|_| "Workflow storage is unavailable")?
            .clone()
            .ok_or("Workflow storage is not configured")?;
        let support = dir.parent().ok_or("Local data directory unavailable")?;
        let registry = Registry::new(support.to_path_buf(), home.clone());
        let project = registry.validated_resolve(&binding.project_id.to_string())?;
        let manager = Manager::new(home.join("X Worktrees"), home);
        let journal = Journal::new(support.join("workflow-journal"));
        binding.validate_source(&manager, &project, &journal)?;
        let provider = self
            .work_adapter(Kind::OpenCode)
            .ok_or("OpenCode is unavailable")?;
        if !provider.descriptor().supports_workspace_write() {
            return Err("A workspace-capable OpenCode provider is required".into());
        }
        let graph = correction_graph(&binding, goal)?;
        self.plan_graph_review(graph, Some(&project), Some(binding))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verification::test_runner::FailureExcerpt;

    fn failed(project: Uuid, node: &str, commit: &str) -> CorrectionDraft {
        CorrectionDraft {
            project_id: project,
            run_id: Uuid::new_v4(),
            node_id: node.into(),
            commit: commit.into(),
            suite: Suite::CargoAgent,
            excerpt: FailureExcerpt {
                stdout: "untrusted stdout".into(),
                stderr: "private stderr".into(),
                stdout_truncated: false,
                stderr_truncated: false,
            },
        }
    }

    #[test]
    fn correction_chain_allows_two_exact_reviewed_attempts_without_replay() {
        let mut history = History::default();
        let project = Uuid::new_v4();
        let initial = failed(project, "test", &"a".repeat(40));
        let run = initial.run_id;
        history.record(initial, None);
        let first = history.find(run, "test").unwrap().binding().unwrap();
        assert_eq!(first.attempt, 1);
        let mut wrong = first.clone();
        wrong.commit = "b".repeat(40);
        assert!(history.consume(&wrong).is_err());
        history.consume(&first).unwrap();
        assert!(history.consume(&first).is_err());

        let failure = failed(project, "correction-retest", &"b".repeat(40));
        let run = failure.run_id;
        history.record(failure, Some(&first));
        let second = history
            .find(run, "correction-retest")
            .unwrap()
            .binding()
            .unwrap();
        assert_eq!(second.root, first.root);
        assert_eq!(second.attempt, 2);
        history.consume(&second).unwrap();

        let failure = failed(project, "correction-retest", &"c".repeat(40));
        let run = failure.run_id;
        history.record(failure, Some(&second));
        assert!(
            history
                .find(run, "correction-retest")
                .unwrap()
                .binding()
                .is_err()
        );
    }

    #[test]
    fn correction_graph_requires_new_commit_and_retests_the_same_suite() {
        let mut history = History::default();
        let failure = failed(Uuid::new_v4(), "test", &"a".repeat(40));
        let run = failure.run_id;
        history.record(failure, None);
        let binding = history.find(run, "test").unwrap().binding().unwrap();
        let graph = correction_graph(&binding, "Fix the failing assertion").unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert!(graph.nodes[0].required_commit);
        assert_eq!(graph.nodes[1].inputs[0].node, "correction-write");
        assert_eq!(graph.nodes[1].inputs[0].proof, InputProof::Committed);
        assert!(matches!(
            graph.nodes[1].job,
            Job::Test {
                suite: Suite::CargoAgent,
                ..
            }
        ));
        let encoded = serde_json::to_string(&graph).unwrap();
        assert!(encoded.contains(&binding.commit));
        assert!(!encoded.contains("private stderr"));
        assert!(!encoded.contains("untrusted stdout"));
        assert!(correction_graph(&binding, "").is_err());
        assert!(correction_graph(&binding, "fix\nnew command").is_err());
    }
}

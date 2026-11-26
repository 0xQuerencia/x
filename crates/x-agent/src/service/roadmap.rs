//! Local roadmap proposals reuse the registered project, stored graph and review path.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::harness::Kind;
use crate::projects::Project;
use crate::verification::test_runner::Suite;
use crate::workflows::dag::{self, ArtifactInput, Graph, HarnessKind, InputProof, Job, Node};
use crate::workflows::model::OnFailure;
use crate::workflows::roadmap::{self, Document, Source, TaskSource};
use crate::workflows::scheduler::Outcome;
use crate::workflows::store::InventoryTask;
use crate::worktrees::Manager;

use super::composer::Role;
use super::{Service, Task, busy};

#[cfg(all(test, unix))]
mod tests;

const LIMITS: &str = "Completion markers are the roadmap author's inventory, not verified implementation. Stored proposals and exact task/goal identities prevent repeats. A source evidence link is an unreviewed mapping. Review exact revisions, acceptance criteria and declared independence before approving.";

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Disposition {
    MarkedComplete,
    SavedProposal {
        graph_id: String,
        changed_since_proposal: bool,
        last_outcome: Option<Outcome>,
    },
    Blocked {
        dependencies: Vec<String>,
    },
    Ready,
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub task: roadmap::Task,
    pub disposition: Disposition,
    pub evidence_mapping: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct Proposal {
    pub inspection_id: String,
    pub source_path: PathBuf,
    pub source_sha256: String,
    pub roadmap_id: String,
    pub project: Project,
    pub base_commit: String,
    pub entries: Vec<Entry>,
    pub next: Vec<String>,
    pub limitations: &'static str,
    pub executed: bool,
    pub permissions_issued: bool,
    #[serde(skip)]
    snapshot: roadmap::Snapshot,
    #[serde(skip)]
    document: Document,
}

#[derive(Clone, Debug)]
pub struct Selection {
    /// Empty chooses the first up to three currently ready tasks in source order.
    pub tasks: Vec<String>,
    pub provider: Kind,
    pub test_suite: Option<Suite>,
}

/// Bounded control transport data. The source file cannot populate these options.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRequest {
    pub project: String,
    pub path: PathBuf,
    pub inspection: Option<String>,
    pub tasks: Vec<String>,
    pub provider: HarnessKind,
    pub test_suite: Option<Suite>,
}

/// One explicitly selected GitHub mapping and its expected immutable merge.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRequest {
    pub project: String,
    pub path: PathBuf,
    pub inspection: String,
    pub task: String,
    pub repository: String,
    pub account: String,
    pub pull_request: u32,
    pub merge_commit: String,
}

fn entries(document: &Document, project: &Project, inventory: &[InventoryTask]) -> Vec<Entry> {
    let completed = document
        .tasks
        .iter()
        .filter(|task| task.completed_in_source)
        .map(|task| task.id.as_str())
        .collect::<HashSet<_>>();
    document
        .tasks
        .iter()
        .map(|task| {
            let fingerprint = roadmap::digest(task.goal_text().as_bytes());
            let stored = inventory
                .iter()
                .find(|saved| saved.matches(project.id, &document.id, &task.id, &fingerprint));
            let dependencies = task
                .depends
                .iter()
                .filter(|dependency| !completed.contains(dependency.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            let disposition = if task.completed_in_source {
                Disposition::MarkedComplete
            } else if let Some(saved) = stored {
                Disposition::SavedProposal {
                    graph_id: saved.graph_id.clone(),
                    changed_since_proposal: saved.goal_sha256 != fingerprint,
                    last_outcome: saved.last_outcome,
                }
            } else if !dependencies.is_empty() {
                Disposition::Blocked { dependencies }
            } else {
                Disposition::Ready
            };
            Entry {
                task: task.clone(),
                disposition,
                evidence_mapping: match task.evidence.len() {
                    0 => "unmapped",
                    1 => "needs_review",
                    _ => "ambiguous",
                },
            }
        })
        .collect()
}

fn inspection(
    snapshot: &roadmap::Snapshot,
    project: &Project,
    base: &str,
    inventory: &[InventoryTask],
) -> Result<String, String> {
    serde_json::to_vec(&(
        snapshot,
        project.id,
        &project.root,
        &project.git_common_dir,
        base,
        inventory,
    ))
    .map(|bytes| roadmap::digest(&bytes))
    .map_err(|_| "Cannot bind the roadmap inspection".into())
}

impl Service {
    /// Check a reviewed mapping through the installed GitHub reader.
    /// The observed artifact never changes source completion or graph authority.
    pub fn inspect_roadmap_evidence(
        &self,
        request: &EvidenceRequest,
    ) -> Result<serde_json::Value, String> {
        self.inspect_roadmap_evidence_with(request, |link, account| {
            let report = crate::github_read::inspect(&link.repository, link.pull_request, account)?;
            Ok((report.state, report.merge_commit))
        })
    }

    fn inspect_roadmap_evidence_with(
        &self,
        request: &EvidenceRequest,
        observe_pull: impl FnOnce(&roadmap::Evidence, &str) -> Result<(String, Option<String>), String>,
    ) -> Result<serde_json::Value, String> {
        let observe_source = || self.inspect_roadmap(&request.project, &request.path);
        let before = observe_source()?;
        if request.inspection != before.inspection_id {
            return Err(
                "The roadmap inspection is stale; inspect the selected source again".into(),
            );
        }
        let entry = before
            .entries
            .iter()
            .find(|entry| entry.task.id == request.task)
            .ok_or("No task matches the exact source ID")?;
        let link = entry
            .task
            .evidence
            .iter()
            .find(|link| {
                link.repository.eq_ignore_ascii_case(&request.repository)
                    && link.pull_request == request.pull_request
                    && link
                        .merge_commit
                        .eq_ignore_ascii_case(&request.merge_commit)
            })
            .ok_or("The reviewed repository, PR and merge commit do not match a source link")?;
        let home = self.roadmap_home()?;
        let manager = Manager::new(home.join("X Worktrees"), home.clone());
        if !manager.has_github_remote(&before.project, &link.repository)? {
            return Err("The selected project has no matching GitHub repository URL".into());
        }
        let (pull_state, observed_merge) = observe_pull(link, &request.account)?;
        let after = observe_source()?;
        if before.inspection_id != after.inspection_id
            || !manager.has_github_remote(&after.project, &link.repository)?
        {
            return Err("The project or roadmap changed during evidence inspection".into());
        }
        let state = match pull_state.as_str() {
            "CLOSED" => "closed_without_recorded_merge",
            "OPEN" => "open",
            "MERGED" if observed_merge.as_deref() != Some(link.merge_commit.as_str()) => {
                "merge_commit_changed"
            }
            "MERGED"
                if !manager.has_ancestor(
                    &after.project,
                    &after.base_commit,
                    &link.merge_commit,
                )? =>
            {
                "merge_not_in_selected_checkout"
            }
            "MERGED" => "merged_artifact_observed",
            _ => return Err("GitHub returned an unsupported pull request state".into()),
        };
        Ok(serde_json::json!({
            "roadmap_id": before.roadmap_id,
            "task": entry.task.id,
            "source_sha256": before.source_sha256,
            "selected_commit": before.base_commit,
            "evidence": link,
            "pull_request_url": format!("https://github.com/{}/pull/{}", link.repository, link.pull_request),
            "observed_merge_commit": observed_merge,
            "state": state,
            "changed_completion": false,
            "executed": false,
        }))
    }

    fn roadmap_home(&self) -> Result<PathBuf, String> {
        self.workflow_dirs
            .lock()
            .map_err(|_| "Workflow storage is unavailable")?
            .as_ref()
            .map(|(_, home)| home.clone())
            .ok_or_else(|| "Workflow storage is not configured".into())
    }

    fn roadmap_project(&self, query: &str) -> Result<Project, String> {
        let registry = self
            .session_store()
            .map(|(registry, _)| registry)
            .ok_or("Project storage is not configured")?;
        let project = registry.validated_resolve(query)?;
        if self.lock().workspace.as_ref() != Some(&project.root) {
            return Err(
                "Select this registered project before inspecting or planning its roadmap".into(),
            );
        }
        Ok(project)
    }

    fn roadmap_base(&self, project: &Project, home: &Path) -> Result<String, String> {
        Manager::new(home.join("X Worktrees"), home.into()).starting_commit(project)
    }

    /// Read exactly one selected file and actual stored graph inventory. No writes,
    /// grants, providers, tests, worktrees or model calls occur during inspection.
    pub fn inspect_roadmap(&self, project_query: &str, path: &Path) -> Result<Proposal, String> {
        let project = self.roadmap_project(project_query)?;
        let home = self.roadmap_home()?;
        let base_commit = self.roadmap_base(&project, &home)?;
        let (snapshot, document) = roadmap::read(path, &home)?;
        let inventory = self.workflow_store().roadmap_inventory()?;
        let inspection_id = inspection(&snapshot, &project, &base_commit, &inventory)?;
        let entries = entries(&document, &project, &inventory);
        let next = entries
            .iter()
            .filter(|entry| matches!(entry.disposition, Disposition::Ready))
            .take(roadmap::MAX_PROPOSAL)
            .map(|entry| entry.task.id.clone())
            .collect();
        Ok(Proposal {
            inspection_id,
            source_path: snapshot.path.clone(),
            source_sha256: snapshot.sha256.clone(),
            roadmap_id: document.id.clone(),
            project,
            base_commit,
            entries,
            next,
            limitations: LIMITS,
            executed: false,
            permissions_issued: false,
            snapshot,
            document,
        })
    }

    /// Revalidate the exact inspection, save an ordinary graph proposal, and open
    /// the existing explicit approval screen. Imported text cannot approve it.
    pub fn plan_roadmap(&self, proposal: &Proposal, selection: &Selection) -> Result<Task, String> {
        let phase = self.lock().phase;
        if phase.is_busy() {
            return Err(busy(phase));
        }
        let home = self.roadmap_home()?;
        let project = self.roadmap_project(&proposal.project.id.to_string())?;
        if project.root != proposal.project.root
            || project.git_common_dir != proposal.project.git_common_dir
        {
            return Err("The registered roadmap project changed; inspect again".into());
        }
        let base = self.roadmap_base(&project, &home)?;
        if base != proposal.base_commit {
            return Err("The project commit changed after inspection; inspect again".into());
        }
        let (source, document) = roadmap::read(&proposal.snapshot.path, &home)?;
        if source != proposal.snapshot || document != proposal.document {
            return Err("The selected roadmap changed after inspection; inspect again".into());
        }
        let descriptor = self
            .work_adapter(selection.provider)
            .ok_or("The selected writer is unavailable")?
            .descriptor();
        if descriptor.kind != selection.provider || !Role::WorkspaceWrite.supported_by(&descriptor)
        {
            return Err("Roadmap changes need an owned workspace writer; this provider lacks that capability".into());
        }
        let inventory = self.workflow_store().roadmap_inventory()?;
        if inspection(&source, &project, &base, &inventory)? != proposal.inspection_id {
            return Err(
                "The roadmap inspection or saved proposal inventory changed; inspect again".into(),
            );
        }
        let entries = entries(&document, &project, &inventory);
        let requested = if selection.tasks.is_empty() {
            proposal.next.clone()
        } else {
            selection.tasks.clone()
        };
        if requested.is_empty() || requested.len() > roadmap::MAX_PROPOSAL {
            return Err("Choose one to three ready independent roadmap tasks".into());
        }
        let mut seen = HashSet::new();
        let tasks = requested.iter().map(|id| {
            if !seen.insert(id) { return Err("A roadmap task was selected twice".to_owned()); }
            entries.iter().find(|entry| &entry.task.id == id && matches!(entry.disposition, Disposition::Ready))
                .map(|entry| &entry.task).ok_or_else(|| format!("Task {id:?} is unknown, completed, already proposed or has unfinished dependencies"))
        }).collect::<Result<Vec<_>, _>>()?;
        let mut references = Vec::new();
        let mut nodes = Vec::new();
        for (index, task) in tasks.iter().enumerate() {
            let id = format!("task-{}", index + 1);
            let goal = task.goal_text();
            references.push(TaskSource {
                task_id: task.id.clone(),
                node_id: id.clone(),
                goal_sha256: roadmap::digest(goal.as_bytes()),
            });
            nodes.push(Node {
                id,
                after: vec![],
                requires: vec![],
                requires_verified: vec![],
                required_change: None,
                required_commit: true,
                inputs: vec![],
                on_failure: OnFailure::Continue,
                job: Job::Harness {
                    harness: match selection.provider {
                        Kind::OpenCode => HarnessKind::OpenCode,
                        Kind::ClaudeCode => HarnessKind::ClaudeCode,
                    },
                    project_id: project.id.to_string(),
                    goal,
                },
            });
        }
        if let Some(suite) = selection.test_suite {
            let inputs = nodes
                .iter()
                .map(|node| ArtifactInput {
                    node: node.id.clone(),
                    proof: InputProof::Committed,
                })
                .collect();
            nodes.push(Node {
                id: "test-results".into(),
                after: vec![],
                requires: vec![],
                requires_verified: vec![],
                required_change: None,
                required_commit: false,
                inputs,
                on_failure: OnFailure::Continue,
                job: Job::Test {
                    project_id: project.id.to_string(),
                    suite,
                },
            });
        }
        let graph = Graph {
            version: dag::ARTIFACT_VERSION,
            id: format!("roadmap-{}", uuid::Uuid::new_v4().simple()),
            name: format!("Roadmap {} · {} changes", document.id, tasks.len()),
            triggers: vec![],
            roadmap: Some(Source {
                identity_sha256: source.identity_hash()?,
                roadmap_id: document.id,
                path: source.path,
                sha256: source.sha256,
                project_id: project.id,
                project_root: project.root.clone(),
                git_common_dir: project
                    .git_common_dir
                    .clone()
                    .ok_or("A roadmap writer requires a Git project")?,
                base_commit: base,
                tasks: references,
            }),
            nodes,
        };
        self.workflow_store().save_graph(&graph)?;
        self.plan_graph_workflow_pinned(&graph.id, Some(&project)).map_err(|error| format!("Saved proposal {} without execution, but review could not open: {error}. Inspect or delete this saved proposal before proposing the tasks again.", graph.id))
    }
}

/// Saved proposals still bind source bytes and project identity at review and
/// again immediately before dispatch. Metadata never restores grants.
pub(super) fn validate_source(
    graph: &Graph,
    project: &Project,
    base: &str,
    home: &Path,
) -> Result<(), String> {
    let Some(source) = &graph.roadmap else {
        return Ok(());
    };
    if source.project_id != project.id
        || source.project_root != project.root
        || Some(&source.git_common_dir) != project.git_common_dir.as_ref()
        || source.base_commit != base
    {
        return Err("The roadmap proposal's project identity or base commit changed; inspect and review a new proposal".into());
    }
    let (current, document) = roadmap::read(&source.path, home)?;
    if current.sha256 != source.sha256
        || current.identity_hash()? != source.identity_sha256
        || document.id != source.roadmap_id
    {
        return Err(
            "The roadmap source changed since this proposal; inspect and review it again".into(),
        );
    }
    for task in &source.tasks {
        if !document.tasks.iter().any(|entry| {
            !entry.completed_in_source
                && entry.id == task.task_id
                && roadmap::digest(entry.goal_text().as_bytes()) == task.goal_sha256
                && entry.depends.iter().all(|dependency| {
                    document
                        .tasks
                        .iter()
                        .any(|task| &task.id == dependency && task.completed_in_source)
                })
        }) {
            return Err("The roadmap source does not contain the exact proposed task".into());
        }
    }
    Ok(())
}

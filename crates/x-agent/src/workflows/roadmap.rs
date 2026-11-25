//! Bounded, explicit Markdown task data. Nothing in a roadmap grants authority.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::dag::{ARTIFACT_VERSION, Graph, Job};

mod source;
pub(crate) use source::{Snapshot, read};
#[cfg(test)]
mod tests;

pub const MAX_BYTES: usize = 64 * 1024;
pub const MAX_TASKS: usize = 128;
pub const MAX_PROPOSAL: usize = 3;

/// Provenance is descriptive data, never an approval, success assertion or grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub roadmap_id: String,
    pub path: PathBuf,
    pub sha256: String,
    pub identity_sha256: String,
    pub project_id: Uuid,
    pub project_root: PathBuf,
    pub git_common_dir: PathBuf,
    pub base_commit: String,
    pub tasks: Vec<TaskSource>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSource {
    pub task_id: String,
    pub node_id: String,
    pub goal_sha256: String,
}

impl Source {
    pub(super) fn validate(&self, graph: &Graph) -> Result<(), String> {
        let path_ok = |path: &std::path::Path| {
            path.is_absolute()
                && path
                    .to_str()
                    .is_some_and(|path| path.len() <= 1024 && !path.chars().any(char::is_control))
                && !path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
        };
        if graph.version != ARTIFACT_VERSION
            || !valid_id(&self.roadmap_id)
            || !valid_hash(&self.sha256)
            || !valid_hash(&self.identity_sha256)
            || self.project_id.is_nil()
            || !path_ok(&self.path)
            || !path_ok(&self.project_root)
            || !path_ok(&self.git_common_dir)
            || !crate::worktrees::valid_commit(&self.base_commit)
            || self.tasks.is_empty()
            || self.tasks.len() > MAX_PROPOSAL
            || !graph.triggers.is_empty()
        {
            return Err("Invalid non-authoritative roadmap provenance".into());
        }
        let project = self.project_id.to_string();
        let writers = graph
            .nodes
            .iter()
            .filter(|node| matches!(node.job, Job::Harness { .. }))
            .count();
        if writers != self.tasks.len()
            || graph.nodes.len() > self.tasks.len() + 1
            || graph
                .nodes
                .iter()
                .any(|node| node.job.project_id() != Some(project.as_str()))
        {
            return Err("Roadmap provenance must match the exact coding graph".into());
        }
        let mut tasks = HashSet::new();
        let mut nodes = HashSet::new();
        for source in &self.tasks {
            let valid_node = graph.nodes.iter().any(|node| {
                node.id == source.node_id
                    && node.required_commit
                    && matches!(&node.job, Job::Harness { goal, .. } if digest(goal.as_bytes()) == source.goal_sha256)
            });
            if !valid_id(&source.task_id)
                || !valid_hash(&source.goal_sha256)
                || !tasks.insert(&source.task_id)
                || !nodes.insert(&source.node_id)
                || !valid_node
            {
                return Err("Roadmap task identities do not match their stored goals".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub completed_in_source: bool,
    pub goal: String,
    pub acceptance: String,
    pub depends: Vec<String>,
}

impl Task {
    pub(crate) fn goal_text(&self) -> String {
        format!(
            "{}: {} Acceptance criteria: {}",
            self.title, self.goal, self.acceptance
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Document {
    pub id: String,
    pub tasks: Vec<Task>,
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Only a documented checkbox/field format is executable task *data*.
/// Prose, fenced code, unknown fields and directives cannot add operations.
fn parse(text: &str) -> Result<Document, String> {
    if text.len() > MAX_BYTES
        || text
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        return Err("Roadmap must be bounded UTF-8 text without control characters".into());
    }
    let mut lines = text
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty());
    let (_, heading) = lines.next().ok_or("The roadmap is empty")?;
    let id = heading
        .strip_prefix("# X Roadmap: ")
        .filter(|id| valid_id(id))
        .ok_or("Start with '# X Roadmap: stable-source-id'")?
        .to_owned();
    let mut document = Document { id, tasks: vec![] };
    let mut fields = Vec::<u8>::new();
    for (line_number, line) in lines {
        let error = || {
            format!(
                "Unsupported roadmap syntax on line {}; use checkbox tasks and Goal, Acceptance, Depends fields",
                line_number + 1
            )
        };
        if line.starts_with("## ") && line.len() <= 160 {
            continue;
        }
        if line.starts_with("- [") {
            let (completed, task) = if let Some(rest) = line.strip_prefix("- [ ] ") {
                (false, rest)
            } else if let Some(rest) = line
                .strip_prefix("- [x] ")
                .or_else(|| line.strip_prefix("- [X] "))
            {
                (true, rest)
            } else {
                return Err(error());
            };
            let (id, title) = task.split_once(": ").ok_or_else(error)?;
            if !valid_id(id)
                || title.trim().is_empty()
                || title.chars().count() > 80
                || document.tasks.len() >= MAX_TASKS
            {
                return Err("Roadmap tasks require stable IDs, titles of 1–80 characters, and at most 128 entries".into());
            }
            document.tasks.push(Task {
                id: id.into(),
                title: title.split_whitespace().collect::<Vec<_>>().join(" "),
                completed_in_source: completed,
                goal: String::new(),
                acceptance: String::new(),
                depends: vec![],
            });
            fields.push(0);
            continue;
        }
        let field = line.strip_prefix("  ").ok_or_else(error)?;
        let (key, value) = field.split_once(": ").ok_or_else(error)?;
        let task = document.tasks.last_mut().ok_or_else(error)?;
        let seen = fields.last_mut().ok_or_else(error)?;
        let bit = match key {
            "Goal" => 1,
            "Acceptance" => 2,
            "Depends" => 4,
            _ => return Err(error()),
        };
        if *seen & bit != 0 || value.trim().is_empty() {
            return Err(error());
        }
        *seen |= bit;
        let clean = value.split_whitespace().collect::<Vec<_>>().join(" ");
        match key {
            "Goal" => task.goal = clean,
            "Acceptance" => task.acceptance = clean,
            "Depends" => {
                if value != "none" {
                    task.depends = value
                        .split(',')
                        .map(|value| value.trim().to_owned())
                        .collect();
                    let mut ids = HashSet::new();
                    if task.depends.len() > 16
                        || task
                            .depends
                            .iter()
                            .any(|id| !valid_id(id) || !ids.insert(id))
                    {
                        return Err(error());
                    }
                }
            }
            _ => unreachable!(),
        }
    }
    if document.tasks.is_empty() {
        return Err("The roadmap contains no tasks".into());
    }
    let mut ids = HashMap::new();
    for (index, task) in document.tasks.iter().enumerate() {
        if ids.insert(task.id.as_str(), index).is_some() {
            return Err("Duplicate roadmap task ID".into());
        }
        if (!task.completed_in_source && fields[index] != 7)
            || task.goal_text().chars().count() > 4096
        {
            return Err("Each unchecked task needs Goal, Acceptance and Depends fields within 4,096 characters".into());
        }
    }
    let mut done = HashSet::new();
    for _ in 0..document.tasks.len() {
        for task in &document.tasks {
            if task.depends.iter().any(|id| !ids.contains_key(id.as_str())) {
                return Err("A roadmap dependency names an unknown task".into());
            }
            if task.depends.iter().all(|id| done.contains(id)) {
                done.insert(task.id.clone());
            }
        }
    }
    if done.len() != document.tasks.len() {
        return Err("Roadmap dependencies contain a cycle".into());
    }
    Ok(document)
}

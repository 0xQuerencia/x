//! A deterministic, non-executing workflow DAG contract.
//! The existing sequential workflow runner stays unchanged until the scheduler lands.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::model::{OnFailure, StepAction, Workflow};

pub const VERSION: u16 = 2;
pub const MAX_NODES: usize = 64;
pub const MAX_DEPENDENCIES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessKind {
    OpenCode,
    ClaudeCode,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Job {
    Desktop {
        action: StepAction,
    },
    Harness {
        harness: HarnessKind,
        project_id: String,
        goal: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: String,
    /// Ordering-only edges: predecessor must finish, but need not succeed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
    /// Verification edges: predecessor must finish successfully.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    pub job: Job,
    #[serde(default)]
    pub on_failure: OnFailure,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    pub version: u16,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<String>,
    pub nodes: Vec<Node>,
}

impl Graph {
    /// Migrates a legacy sequential workflow without executing or modifying it.
    /// Every step remains ordered; its explicit success requirements stay separate.
    pub fn from_legacy(legacy: Workflow, home: &Path) -> Result<Self, String> {
        legacy.validate(home).map_err(|problems| {
            problems
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        let mut previous: Option<String> = None;
        let nodes = legacy
            .steps
            .into_iter()
            .map(|step| {
                let after = previous.iter().cloned().collect();
                previous = Some(step.id.clone());
                Node {
                    id: step.id,
                    after,
                    requires: step.requires,
                    job: Job::Desktop {
                        action: step.action,
                    },
                    on_failure: step.on_failure,
                }
            })
            .collect();
        let graph = Self {
            version: VERSION,
            id: legacy.id,
            name: legacy.name,
            triggers: legacy.triggers,
            nodes,
        };
        graph.validate_with_home(home)?;
        Ok(graph)
    }

    /// Validate graph structure. This alone does not authorize any operation.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != VERSION {
            return Err("Unsupported workflow graph version".into());
        }
        if !slug(&self.id) || self.name.trim().is_empty() || self.name.chars().count() > 80 {
            return Err("Invalid graph ID or name".into());
        }
        if self.nodes.is_empty() || self.nodes.len() > MAX_NODES {
            return Err(format!("A graph must have 1-{MAX_NODES} nodes"));
        }
        let mut ids = HashSet::new();
        for node in &self.nodes {
            if !slug(&node.id) || !ids.insert(node.id.as_str()) {
                return Err("Graph nodes require unique safe IDs".into());
            }
            if node.after.len() > MAX_DEPENDENCIES || node.requires.len() > MAX_DEPENDENCIES {
                return Err("Too many dependencies".into());
            }
            if let Job::Harness {
                project_id, goal, ..
            } = &node.job
                && (uuid::Uuid::parse_str(project_id).is_err()
                    || goal.trim().is_empty()
                    || goal.chars().count() > 4_096
                    || goal.chars().any(char::is_control))
            {
                return Err("Harness tasks require a registered project ID and short goal".into());
            }
        }
        for node in &self.nodes {
            for edges in [&node.after, &node.requires] {
                let mut seen = HashSet::new();
                for required in edges {
                    if required == &node.id {
                        return Err(format!("Node {:?} cannot depend on itself", node.id));
                    }
                    if !ids.contains(required.as_str()) {
                        return Err(format!(
                            "Node {:?} has unknown dependency {required:?}",
                            node.id
                        ));
                    }
                    if !seen.insert(required) {
                        return Err(format!(
                            "Node {:?} repeats dependency {required:?}",
                            node.id
                        ));
                    }
                }
            }
        }
        self.order()?;
        Ok(())
    }

    /// Also verify the closed desktop action registry against the approved home scope.
    pub fn validate_with_home(&self, home: &Path) -> Result<(), String> {
        self.validate()?;
        if self.triggers.len() > super::model::MAX_TRIGGERS {
            return Err("Too many trigger phrases".into());
        }
        let mut triggers = HashSet::new();
        for trigger in &self.triggers {
            let normalized = super::model::normalize(trigger);
            if normalized.is_empty()
                || normalized.split_whitespace().count() > 6
                || normalized.chars().count() > 60
                || super::model::is_builtin_phrase(&normalized)
                || !triggers.insert(normalized)
            {
                return Err("Invalid or duplicated trigger phrase".into());
            }
        }
        for node in &self.nodes {
            if let Job::Desktop { action } = &node.job {
                super::model::check_action(action, home)?;
            }
        }
        Ok(())
    }

    /// A stable topological order; this is a plan, never an execution request.
    pub fn order(&self) -> Result<Vec<String>, String> {
        let lookup: HashMap<&str, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id.as_str(), index))
            .collect();
        let mut waiting = vec![0usize; self.nodes.len()];
        let mut next = vec![Vec::new(); self.nodes.len()];
        for (index, node) in self.nodes.iter().enumerate() {
            let mut already = HashSet::new();
            for dep in node.after.iter().chain(&node.requires) {
                if !already.insert(dep.as_str()) {
                    continue;
                }
                let Some(&source) = lookup.get(dep.as_str()) else {
                    return Err(format!("Unknown dependency {dep:?}"));
                };
                waiting[index] += 1;
                next[source].push(index);
            }
        }
        let mut ready = (0..self.nodes.len())
            .filter(|&index| waiting[index] == 0)
            .collect::<Vec<_>>();
        let mut result = Vec::with_capacity(self.nodes.len());
        while !ready.is_empty() {
            let index = ready.remove(0);
            result.push(self.nodes[index].id.clone());
            for &child in &next[index] {
                waiting[child] -= 1;
                if waiting[child] == 0 {
                    ready.push(child);
                }
            }
            ready.sort_unstable();
        }
        if result.len() != self.nodes.len() {
            return Err("Workflow contains a dependency cycle".into());
        }
        Ok(result)
    }
}

fn slug(value: &str) -> bool {
    (1..=40).contains(&value.len())
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
        && value.as_bytes()[0].is_ascii_alphanumeric()
}

/// Accept a strict v2 graph or convert an existing v1 workflow in memory.
/// Neither path executes anything, and unknown keys are refused.
pub fn decode(input: &str, home: &Path) -> Result<Graph, String> {
    let value: serde_json::Value =
        serde_json::from_str(input).map_err(|e| format!("Invalid workflow JSON: {e}"))?;
    match value.get("version") {
        None => {
            let legacy: Workflow = serde_json::from_value(value)
                .map_err(|e| format!("Invalid legacy workflow: {e}"))?;
            Graph::from_legacy(legacy, home)
        }
        Some(version) if version.as_u64() == Some(u64::from(VERSION)) => {
            let graph: Graph = serde_json::from_value(value)
                .map_err(|e| format!("Invalid workflow graph: {e}"))?;
            graph.validate_with_home(home)?;
            Ok(graph)
        }
        _ => Err("Unsupported workflow graph version".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::{Step, StepAction};
    use super::*;

    fn home() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    fn legacy() -> Workflow {
        Workflow {
            id: "example".into(),
            name: "Example".into(),
            triggers: vec!["sequence demo".into()],
            steps: vec![
                Step {
                    id: "first".into(),
                    action: StepAction::SwitchWorkspace { workspace: 2 },
                    requires: vec![],
                    timeout_secs: None,
                    on_failure: OnFailure::Continue,
                },
                Step {
                    id: "second".into(),
                    action: StepAction::SwitchWorkspace { workspace: 3 },
                    requires: vec![],
                    timeout_secs: None,
                    on_failure: OnFailure::Stop,
                },
            ],
        }
    }

    #[test]
    fn migrating_old_workflows_preserves_order_without_requiring_success() {
        let mut graph = Graph::from_legacy(legacy(), &home()).unwrap();
        assert_eq!(graph.order().unwrap(), ["first", "second"]);
        assert_eq!(graph.nodes[1].after, ["first"]);
        assert!(graph.nodes[1].requires.is_empty());
        assert_eq!(graph.nodes[0].on_failure, OnFailure::Continue);
        assert_eq!(graph.triggers, legacy().triggers);
        let original = serde_json::to_string(&legacy()).unwrap();
        assert_eq!(decode(&original, &home()).unwrap(), graph);
        graph.nodes[1].requires.push("first".into());
        graph.validate().unwrap();
        assert_eq!(graph.order().unwrap(), ["first", "second"]);
    }

    #[test]
    fn rejects_cycles_unknown_ids_and_duplicate_edges() {
        let mut graph = Graph::from_legacy(legacy(), &home()).unwrap();
        graph.nodes[0].after.push("second".into());
        assert!(graph.validate().unwrap_err().contains("cycle"));
        graph.nodes[0].after = vec!["missing".into()];
        assert!(graph.validate().unwrap_err().contains("unknown"));
        graph.nodes[0].after = vec![];
        graph.nodes[1].after = vec!["first".into(), "first".into()];
        assert!(graph.validate().unwrap_err().contains("repeats"));
    }

    #[test]
    fn supports_parallel_ready_nodes_in_a_stable_order() {
        let mut graph = Graph::from_legacy(legacy(), &home()).unwrap();
        graph.nodes[1].after.clear();
        graph.validate().unwrap();
        assert_eq!(graph.order().unwrap(), ["first", "second"]);
        graph.nodes.push(Node {
            id: "verify".into(),
            after: vec![],
            requires: vec!["first".into(), "second".into()],
            job: Job::Desktop {
                action: StepAction::SwitchWorkspace { workspace: 4 },
            },
            on_failure: OnFailure::Stop,
        });
        graph.validate().unwrap();
        assert_eq!(graph.order().unwrap(), ["first", "second", "verify"]);
    }

    #[test]
    fn v2_round_trip_and_unknown_keys_are_rejected() {
        let graph = Graph::from_legacy(legacy(), &home()).unwrap();
        let serialized = serde_json::to_string(&graph).unwrap();
        assert_eq!(decode(&serialized, &home()).unwrap(), graph);
        let mut value: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        value["nodes"][0]["unapproved"] = serde_json::json!({ "run": "rm -rf /" });
        assert!(decode(&value.to_string(), &home()).is_err());
        let mut invalid = graph.clone();
        invalid.nodes[0].job = Job::Desktop {
            action: StepAction::SwitchWorkspace { workspace: 255 },
        };
        assert!(decode(&serde_json::to_string(&invalid).unwrap(), &home()).is_err());
    }

    #[test]
    fn harness_tasks_require_valid_registered_project_identity() {
        let mut graph = Graph::from_legacy(legacy(), &home()).unwrap();
        graph.nodes[0].job = Job::Harness {
            harness: HarnessKind::OpenCode,
            project_id: "not-an-id".into(),
            goal: "check tests".into(),
        };
        assert!(graph.validate().is_err());
        if let Job::Harness { project_id, .. } = &mut graph.nodes[0].job {
            *project_id = uuid::Uuid::new_v4().to_string();
        }
        graph.validate().unwrap();
    }
}

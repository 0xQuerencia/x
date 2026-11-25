//! A deterministic, non-executing workflow DAG contract.
//! Legacy sequencing, explicit proof edges and immutable input contracts share this schema.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::model::{OnFailure, StepAction, Workflow};

pub const VERSION: u16 = 2;
pub const PROOF_VERSION: u16 = 3;
pub const ARTIFACT_VERSION: u16 = 4;
pub const MAX_INPUTS: usize = 8;
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
    Test {
        project_id: String,
        suite: crate::verification::test_runner::Suite,
    },
    Harness {
        harness: HarnessKind,
        project_id: String,
        goal: String,
    },
}

/// A local producer identity, never a model-supplied commit or filesystem path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactInput {
    pub node: String,
    pub proof: InputProof,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputProof {
    Committed,
    Tested,
}

impl Job {
    pub fn project_id(&self) -> Option<&str> {
        match self {
            Self::Harness { project_id, .. } | Self::Test { project_id, .. } => Some(project_id),
            Self::Desktop { .. } => None,
        }
    }
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
    /// Version 3: require additional proof, not merely successful delivery.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_verified: Vec<String>,
    /// Version 3: require an observed change to this exact relative Git path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_change: Option<String>,
    /// Version 3: require a changed, clean descendant commit as an artifact.
    #[serde(default, skip_serializing_if = "is_false")]
    pub required_commit: bool,
    /// Version 4: immutable outputs to assemble in this node's private checkout.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<ArtifactInput>,
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
                    requires_verified: vec![],
                    required_change: None,
                    required_commit: false,
                    inputs: vec![],
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
        if !matches!(self.version, VERSION | PROOF_VERSION | ARTIFACT_VERSION) {
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
            if (!node.inputs.is_empty() || matches!(node.job, Job::Test { .. }))
                && (self.version != ARTIFACT_VERSION || node.job.project_id().is_none())
            {
                return Err("Artifact inputs and test jobs require a v4 coding graph".into());
            }
            if !node.inputs.is_empty()
                && matches!(
                    node.job,
                    Job::Harness {
                        harness: HarnessKind::ClaudeCode,
                        ..
                    }
                )
            {
                return Err("Tool-free Claude cannot inspect an artifact checkout; choose a workspace-capable reviewer".into());
            }
            if node.inputs.len() > MAX_INPUTS {
                return Err("A job may consume at most eight artifacts".into());
            }
            if let Job::Test { project_id, .. } = &node.job
                && (uuid::Uuid::parse_str(project_id).map_or(true, |id| id.is_nil())
                    || node.inputs.is_empty()
                    || node.required_commit
                    || node.required_change.is_some())
            {
                return Err("A test job requires a registered project and committed input".into());
            }
            if node.required_commit
                && (self.version < PROOF_VERSION
                    || node.required_change.is_some()
                    || !matches!(
                        &node.job,
                        Job::Harness {
                            harness: HarnessKind::OpenCode,
                            ..
                        }
                    ))
            {
                return Err(
                    "Commit proof requires a v3 OpenCode job without another proof profile".into(),
                );
            }
            if let Some(path) = &node.required_change
                && (self.version < PROOF_VERSION
                    || !matches!(
                        &node.job,
                        Job::Harness {
                            harness: HarnessKind::OpenCode,
                            ..
                        }
                    )
                    || !crate::verification::valid_expected_path(path))
            {
                return Err("Required file change needs a safe v3 OpenCode proof path".into());
            }
            if self.version == VERSION && !node.requires_verified.is_empty() {
                return Err("Verified dependency edges require graph version 3".into());
            }
            if node.after.len() > MAX_DEPENDENCIES
                || node.requires.len() > MAX_DEPENDENCIES
                || node.requires_verified.len() > MAX_DEPENDENCIES
            {
                return Err("Too many dependencies".into());
            }
            if let Job::Harness {
                project_id, goal, ..
            } = &node.job
                && (uuid::Uuid::parse_str(project_id).map_or(true, |id| id.is_nil())
                    || goal.trim().is_empty()
                    || goal.chars().count() > 4_096
                    || goal.chars().any(char::is_control))
            {
                return Err("Harness tasks require a registered project ID and short goal".into());
            }
        }
        for node in &self.nodes {
            if let Job::Desktop {
                action: StepAction::CloseOpenedWindow { opened_by },
            } = &node.job
            {
                let source = self.nodes.iter().find(|source| &source.id == opened_by);
                if !source.is_some_and(|source| {
                    matches!(
                        &source.job,
                        Job::Desktop {
                            action: StepAction::OpenApp { .. } | StepAction::OpenTerminal { .. }
                        }
                    )
                }) || !self.depends_on(&node.id, opened_by)
                {
                    return Err(
                        "Closing a run-opened window requires an explicit predecessor opener"
                            .into(),
                    );
                }
            }
            let mut inputs = HashSet::new();
            for input in &node.inputs {
                let source = self
                    .nodes
                    .iter()
                    .find(|source| source.id == input.node)
                    .ok_or("Artifact source does not exist")?;
                if input.node == node.id
                    || !inputs.insert(&input.node)
                    || source.job.project_id() != node.job.project_id()
                    || !(source.required_commit || matches!(source.job, Job::Test { .. }))
                    || (input.proof == InputProof::Tested
                        && !matches!(source.job, Job::Test { .. }))
                {
                    return Err("Artifact input must name a distinct same-project producer with the required proof".into());
                }
            }
            for edges in [&node.after, &node.requires, &node.requires_verified] {
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

    /// Whether a declared predecessor is reachable through this node's edges.
    pub(crate) fn depends_on(&self, node: &str, predecessor: &str) -> bool {
        let mut waiting = vec![node];
        let mut seen = HashSet::new();
        while let Some(id) = waiting.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(current) = self.nodes.iter().find(|node| node.id == id) else {
                continue;
            };
            for dependency in current
                .after
                .iter()
                .chain(&current.requires)
                .chain(&current.requires_verified)
                .chain(current.inputs.iter().map(|input| &input.node))
            {
                if dependency == predecessor {
                    return true;
                }
                waiting.push(dependency);
            }
        }
        false
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
            for dep in node
                .after
                .iter()
                .chain(&node.requires)
                .chain(&node.requires_verified)
                .chain(node.inputs.iter().map(|input| &input.node))
            {
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

fn is_false(value: &bool) -> bool {
    !*value
}

fn slug(value: &str) -> bool {
    (1..=40).contains(&value.len())
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
        && value.as_bytes()[0].is_ascii_alphanumeric()
}

/// Accept a supported strict graph or convert an existing v1 workflow in memory.
/// Neither path executes anything, and unknown keys are refused.
pub fn decode(input: &str, home: &Path) -> Result<Graph, String> {
    let value: serde_json::Value =
        serde_json::from_str(input).map_err(|e| format!("Invalid workflow JSON: {e}"))?;
    match value.get("version") {
        None => {
            let legacy: Workflow =
                serde_json::from_str(input).map_err(|e| format!("Invalid legacy workflow: {e}"))?;
            Graph::from_legacy(legacy, home)
        }
        Some(version)
            if version
                .as_u64()
                .is_some_and(|v| matches!(v, v if v == u64::from(VERSION) || v == u64::from(PROOF_VERSION) || v == u64::from(ARTIFACT_VERSION))) =>
        {
            let graph: Graph =
                serde_json::from_str(input).map_err(|e| format!("Invalid workflow graph: {e}"))?;
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
            requires_verified: vec![],
            required_change: None,
            required_commit: false,

            inputs: vec![],
            job: Job::Desktop {
                action: StepAction::SwitchWorkspace { workspace: 4 },
            },
            on_failure: OnFailure::Stop,
        });
        graph.validate().unwrap();
        assert_eq!(graph.order().unwrap(), ["first", "second", "verify"]);
    }

    #[test]
    fn committed_artifact_requires_a_v3_writer_job() {
        let mut graph = Graph::from_legacy(legacy(), &home()).unwrap();
        graph.nodes[0].job = Job::Harness {
            harness: HarnessKind::OpenCode,
            project_id: uuid::Uuid::new_v4().to_string(),
            goal: "commit a fix".into(),
        };
        graph.nodes[0].required_commit = true;
        assert!(graph.validate().is_err());
        graph.version = PROOF_VERSION;
        graph.validate().unwrap();
        graph.nodes[0].required_change = Some("src/main.rs".into());
        assert!(graph.validate().is_err());
        graph.nodes[0].required_change = None;
        if let Job::Harness { harness, .. } = &mut graph.nodes[0].job {
            *harness = HarnessKind::ClaudeCode;
        }
        assert!(graph.validate().is_err());
    }

    #[test]
    fn proof_edges_require_v3_and_participate_in_cycle_validation() {
        let mut graph = Graph::from_legacy(legacy(), &home()).unwrap();
        graph.nodes[1].requires_verified = vec!["first".into()];
        assert!(graph.validate().is_err());
        graph.version = PROOF_VERSION;
        graph.validate().unwrap();
        assert_eq!(graph.order().unwrap(), ["first", "second"]);
        let serialized = serde_json::to_string(&graph).unwrap();
        assert_eq!(decode(&serialized, &home()).unwrap(), graph);
        graph.nodes[0].requires_verified = vec!["second".into()];
        assert!(graph.validate().unwrap_err().contains("cycle"));
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
    fn decode_rejects_duplicate_fields_without_value_normalization() {
        let graph = Graph::from_legacy(legacy(), &home()).unwrap();
        let json = serde_json::to_string(&graph).unwrap();
        let duplicated = json.replacen("\"version\":2", "\"version\":2,\"version\":2", 1);
        assert_ne!(duplicated, json);
        assert!(decode(&duplicated, &home()).is_err());

        let json = serde_json::to_string(&legacy()).unwrap();
        let duplicated = json.replacen(
            "\"id\":\"example\"",
            "\"id\":\"other\",\"id\":\"example\"",
            1,
        );
        assert_ne!(duplicated, json);
        assert!(decode(&duplicated, &home()).is_err());
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
            *project_id = uuid::Uuid::nil().to_string();
        }
        assert!(graph.validate().is_err());
        if let Job::Harness { project_id, .. } = &mut graph.nodes[0].job {
            *project_id = uuid::Uuid::new_v4().to_string();
        }
        graph.validate().unwrap();
    }

    #[test]
    fn artifact_edges_are_versioned_same_project_and_proof_typed() {
        let project = uuid::Uuid::new_v4().to_string();
        let raw = serde_json::json!({
            "version": 4, "id": "artifact-plan", "name": "Artifact plan", "nodes": [
                {"id":"write", "required_commit":true, "job":{"type":"harness", "harness":"open_code", "project_id":project, "goal":"Implement the reviewed change"}},
                {"id":"test", "inputs":[{"node":"write", "proof":"committed"}], "job":{"type":"test", "project_id":project, "suite":"cargo_agent"}},
                {"id":"review", "inputs":[{"node":"test", "proof":"tested"}], "job":{"type":"harness", "harness":"open_code", "project_id":project, "goal":"Review the tested code"}}
            ]
        });
        let graph = decode(&raw.to_string(), &home()).unwrap();
        assert_eq!(graph.order().unwrap(), ["write", "test", "review"]);
        for mutate in 0..7 {
            let mut value = raw.clone();
            match mutate {
                0 => value["version"] = 3.into(),
                1 => value["nodes"][1]["inputs"][0]["node"] = "missing".into(),
                2 => value["nodes"][1]["inputs"][0]["proof"] = "tested".into(),
                3 => {
                    value["nodes"][1]["job"]["project_id"] = uuid::Uuid::new_v4().to_string().into()
                }
                4 => value["nodes"][0]["required_commit"] = false.into(),
                5 => value["nodes"][1]["inputs"][0]["commit"] = "forged-sha".into(),
                _ => {
                    value["nodes"][0]["inputs"] =
                        serde_json::json!([{"node":"test", "proof":"tested"}])
                }
            }
            assert!(
                decode(&value.to_string(), &home()).is_err(),
                "mutation {mutate}"
            );
        }
    }
}

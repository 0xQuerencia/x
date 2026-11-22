//! `x agent workflow ...`: the same operations as the workflow manager, for scripts and for
//! people who keep their workflows in files. Everything goes through the same store and
//! validation as the editor.

use x_agent::service::Service;
use x_agent::workflows::Store;
use x_agent::workflows::dag::{Graph, Job};

use crate::control::ControlReply;

fn summary(store: &Store) -> serde_json::Value {
    let entries: Vec<_> = store
        .list()
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "id": e.workflow.id,
                "name": e.workflow.name,
                "triggers": e.workflow.triggers,
                "steps": e.workflow.steps.len(),
                "runnable": e.runnable() && e.conflicts.is_empty(),
                "problems": e.problems.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "conflicts": e.conflicts,
                "last": store.last(&e.workflow.id).map(|r| serde_json::json!({
                    "state": r.state,
                    "summary": r.summary,
                })),
            })
        })
        .collect();
    serde_json::Value::Array(entries)
}

/// A read-only policy summary. No receipt ID or private harness goal is serialized.
fn permission_preview(graph: &Graph) -> serde_json::Value {
    let nodes = graph
        .nodes
        .iter()
        .map(|node| match &node.job {
            Job::Desktop { action } => serde_json::json!({
                "node_id": node.id,
                "operation": "desktop",
                "approval": "service_plan",
                "action": action,
            }),
            Job::Harness {
                harness,
                project_id,
                ..
            } => serde_json::json!({
                "node_id": node.id,
                "operation": "harness",
                "approval": "task_scoped_one_use",
                "provider": harness,
                "project_id": project_id,
                "requires_registered_project": true,
            }),
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "workflow_id": graph.id,
        "nodes": nodes,
        "executed": false,
        "permissions_issued": false,
    })
}

pub(super) fn handle(svc: &Service, command: &str, value: &str) -> ControlReply {
    let store = svc.workflow_store();
    let refuse = |error: String| ControlReply::error("refused", error);
    let need =
        |what: &str| ControlReply::error("usage", format!("agent workflow {command} needs {what}"));
    match command {
        "list" => ControlReply::done(summary(&store).to_string()),
        "dir" => ControlReply::done(store.dir().display().to_string()),
        "show" if value.is_empty() => need("a workflow id"),
        "show" => match store.get(value) {
            Ok(wf) => ControlReply::done(
                serde_json::json!({"workflow": wf, "last": store.last(value)}).to_string(),
            ),
            Err(error) => refuse(error),
        },
        "permissions" if value.is_empty() => need("a workflow id"),
        "permissions" => match store.get_graph(value) {
            Ok(graph) => ControlReply::done(permission_preview(&graph).to_string()),
            Err(error) => refuse(error),
        },
        "claims" if !value.is_empty() => refuse("workflow claims takes no arguments".into()),
        "claims" => match svc.coding_claims_summary() {
            Ok(report) => ControlReply::done(report.to_string()),
            Err(error) => refuse(error),
        },
        "uncertain" if !value.is_empty() => refuse("workflow uncertain takes no arguments".into()),
        "uncertain" => match svc.uncertain_coding_jobs() {
            Ok(report) => ControlReply::done(report.to_string()),
            Err(error) => refuse(error),
        },
        "last-graph" if value.is_empty() => need("a workflow id"),
        "last-graph" => match store.last_graph(value) {
            Ok(Some(report)) => {
                ControlReply::done(serde_json::to_string(&report).unwrap_or_default())
            }
            Ok(None) => refuse(format!("{value:?} has no saved coding run result")),
            Err(error) => refuse(error),
        },
        "preview" if value.is_empty() => need("a workflow id"),
        "preview" => match store.get_graph(value) {
            Ok(graph) => match graph.order() {
                Ok(order) => ControlReply::done(
                    serde_json::json!({
                        "graph": graph,
                        "order": order,
                        "executed": false,
                        "authorization": "not_requested",
                        "note": "Read-only preview; no actions or harnesses were started",
                    })
                    .to_string(),
                ),
                Err(error) => refuse(error),
            },
            Err(error) => refuse(error),
        },
        "last" if value.is_empty() => need("a workflow id"),
        "last" => match store.last(value) {
            Some(report) => ControlReply::done(serde_json::to_string(&report).unwrap_or_default()),
            None => refuse(format!("{value:?} has not run yet")),
        },
        "run" if value.is_empty() => need("a workflow id"),
        "run" => match svc.plan_workflow_by_id(value) {
            Ok(_) => ControlReply::done(
                "Planned. Approve it in the Agent pane or with `agent approve <id>`",
            ),
            Err(error) => refuse(error),
        },
        "test-project" if value.is_empty() => need("a registered project id and test suite"),
        "test-project" => {
            let args: Vec<_> = value.split_whitespace().collect();
            if args.len() != 2 {
                need(
                    "exactly a project id and suite (cargo-workspace, cargo-agent, npm, pnpm, pytest, go)",
                )
            } else {
                match x_agent::verification::test_runner::Suite::parse(args[1])
                    .and_then(|suite| svc.plan_worktree_tests(args[0], suite).map(|_| ()))
                {
                    Ok(()) => super::snapshot(0),
                    Err(error) => refuse(error),
                }
            }
        }
        "run-graph" if value.is_empty() => need("a workflow id"),
        "run-graph" => match svc.plan_graph_workflow(value) {
            Ok(_) => super::snapshot(0),
            Err(error) => refuse(error),
        },
        "delete" if value.is_empty() => need("a workflow id"),
        "delete" => match store.delete(value) {
            Ok(()) => ControlReply::done(format!("Deleted {value}")),
            Err(error) => refuse(error),
        },
        "duplicate" if value.is_empty() => need("a workflow id"),
        "duplicate" => match store.duplicate(value) {
            Ok(copy) => ControlReply::done(format!("Created {} ({})", copy.id, copy.name)),
            Err(error) => refuse(error),
        },
        "import" if value.is_empty() => need("a file path"),
        "import" => match store.import(std::path::Path::new(value)) {
            Ok(wf) => ControlReply::done(format!("Imported {} ({})", wf.id, wf.name)),
            Err(error) => refuse(error),
        },
        "import-graph" if value.is_empty() => need("a file path"),
        "import-graph" => match store.import_graph(std::path::Path::new(value)) {
            Ok(graph) => ControlReply::done(format!("Imported {} ({})", graph.id, graph.name)),
            Err(error) => refuse(error),
        },
        "undo" => match svc.undo_workflow() {
            Ok(message) => ControlReply::done(message),
            Err(error) => refuse(error),
        },
        _ => ControlReply::error(
            "usage",
            "workflow commands: list, show, preview, permissions, uncertain, claims, run, run-graph, test-project, last, last-graph, delete, duplicate, import, import-graph, undo, dir",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x_agent::workflows::dag::{HarnessKind, Node, VERSION};
    use x_agent::workflows::model::OnFailure;

    #[test]
    fn permission_review_does_not_reveal_private_goal_or_issue_grants() {
        let graph = Graph {
            version: VERSION,
            id: "review".into(),
            name: "Review".into(),
            triggers: vec![],
            nodes: vec![Node {
                id: "backend".into(),
                after: vec![],
                requires: vec![],
                job: Job::Harness {
                    harness: HarnessKind::OpenCode,
                    project_id: "123e4567-e89b-12d3-a456-426614174000".into(),
                    goal: "private coding instructions".into(),
                },
                on_failure: OnFailure::Stop,
            }],
        };
        let output = permission_preview(&graph);
        let rendered = output.to_string();
        assert_eq!(output["executed"], false);
        assert_eq!(output["permissions_issued"], false);
        assert_eq!(output["nodes"][0]["approval"], "task_scoped_one_use");
        assert!(!rendered.contains("private coding instructions"));
    }
}

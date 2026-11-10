//! `x agent workflow ...`: the same operations as the workflow manager, for scripts and for
//! people who keep their workflows in files. Everything goes through the same store and
//! validation as the editor.

use x_agent::service::Service;
use x_agent::workflows::Store;

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
        "undo" => match svc.undo_workflow() {
            Ok(message) => ControlReply::done(message),
            Err(error) => refuse(error),
        },
        _ => ControlReply::error(
            "usage",
            "workflow commands: list, show, run, last, delete, duplicate, import, undo, dir",
        ),
    }
}

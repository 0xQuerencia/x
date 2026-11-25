use super::*;
use x_agent::service::roadmap::{CommandRequest, Selection};

fn request(value: &str) -> Result<CommandRequest, String> {
    if value.len() > 4096 {
        return Err("Roadmap request exceeds 4 KiB".into());
    }
    serde_json::from_str(value).map_err(|_| "Roadmap request has invalid or unknown fields".into())
}

pub(super) fn handle(svc: &Service, command: &str, value: &str) -> ControlReply {
    let result: Result<String, String> = (|| {
        let request = request(value)?;
        let proposal = svc.inspect_roadmap(&request.project, &request.path)?;
        if command == "roadmap-inspect" {
            if request.inspection.is_some()
                || !request.tasks.is_empty()
                || request.test_suite.is_some()
            {
                return Err("Inspection takes no task selection or execution settings".into());
            }
            return serde_json::to_string(&proposal)
                .map_err(|_| "Cannot render roadmap inspection".into());
        }
        if request.inspection.as_deref() != Some(proposal.inspection_id.as_str()) {
            return Err("The roadmap inspection is missing or stale; inspect the source, project and stored proposals again".into());
        }
        let selection = Selection {
            tasks: request.tasks,
            provider: match request.provider {
                x_agent::workflows::dag::HarnessKind::OpenCode => x_agent::harness::Kind::OpenCode,
                x_agent::workflows::dag::HarnessKind::ClaudeCode => {
                    x_agent::harness::Kind::ClaudeCode
                }
            },
            test_suite: request.test_suite,
        };
        let task = svc.plan_roadmap(&proposal, &selection)?;
        serde_json::to_string(&serde_json::json!({ "task": task, "executed": false, "permissions_issued": false, "note": "Proposal saved and awaiting ordinary owner approval; no jobs entered the execution queue" })).map_err(|_| "Cannot render roadmap plan".into())
    })();
    match result {
        Ok(output) => ControlReply::done(output),
        Err(error) => ControlReply::error("refused", error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roadmap_control_request_cannot_supply_approval_or_unbounded_text() {
        let valid = serde_json::json!({ "project": "app", "path": "/home/me/roadmap.md", "inspection": null, "tasks": [], "provider": "open_code", "test_suite": null });
        assert!(request(&valid.to_string()).is_ok());
        let mut forged = valid;
        forged["approved"] = serde_json::json!(true);
        assert!(request(&forged.to_string()).is_err());
        assert!(request(&"a".repeat(4097)).is_err());
    }
}

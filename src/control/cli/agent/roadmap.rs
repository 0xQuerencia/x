use super::*;
use x_agent::service::roadmap::{CommandRequest, EvidenceRequest};
use x_agent::verification::test_runner::Suite;
use x_agent::workflows::dag::HarnessKind;

#[derive(Subcommand, Debug)]
pub enum RoadmapCommand {
    /// Read the selected Markdown and saved proposal inventory without saving or running.
    Inspect {
        file: String,
        #[arg(long)]
        project: String,
        #[command(flatten)]
        output: Output,
    },
    /// Check one source evidence link at an exact inspected revision.
    Evidence {
        file: String,
        #[arg(long)]
        project: String,
        #[arg(long)]
        inspection: String,
        #[arg(long)]
        task: String,
        #[arg(long)]
        repository: String,
        #[arg(long)]
        account: String,
        #[arg(long)]
        pull_request: u32,
        #[arg(long)]
        merge_commit: String,
        #[command(flatten)]
        output: Output,
    },
    /// Save selected independent tasks and open the ordinary approval preview.
    Plan {
        file: String,
        #[arg(long)]
        project: String,
        /// Exact inspection_id returned by inspect; changes invalidate it.
        #[arg(long)]
        inspection: String,
        /// Repeat for one to three ready task IDs; omit to use the inspected next three.
        #[arg(long = "task")]
        tasks: Vec<String>,
        #[arg(long, value_parser = ["open-code", "claude-code"], default_value = "open-code")]
        provider: String,
        /// Optional fixed suite at the assembled immutable commits. Repository scripts run only after approval.
        #[arg(long, value_parser = ["cargo-workspace", "cargo-agent", "npm", "pnpm", "pytest", "go"])]
        suite: Option<String>,
        #[command(flatten)]
        output: Output,
    },
}

pub(super) fn route(command: RoadmapCommand) -> Result<(AgentAction, Output), String> {
    let (command, file, project, inspection, tasks, provider, test_suite, output) = match command {
        RoadmapCommand::Inspect {
            file,
            project,
            output,
        } => (
            "roadmap-inspect",
            file,
            project,
            None,
            vec![],
            HarnessKind::OpenCode,
            None,
            output,
        ),
        RoadmapCommand::Evidence {
            file,
            project,
            inspection,
            task,
            repository,
            account,
            pull_request,
            merge_commit,
            output,
        } => {
            let path = std::path::absolute(file)
                .map_err(|_| "Could not resolve the selected roadmap file")?;
            let request = EvidenceRequest {
                project,
                path,
                inspection,
                task,
                repository,
                account,
                pull_request,
                merge_commit,
            };
            let value = serde_json::to_string(&request)
                .map_err(|_| "Could not encode the roadmap evidence request")?;
            return Ok((
                AgentAction::Workflow {
                    command: "roadmap-evidence".into(),
                    value,
                },
                output,
            ));
        }
        RoadmapCommand::Plan {
            file,
            project,
            inspection,
            tasks,
            provider,
            suite,
            output,
        } => {
            let provider = match provider.as_str() {
                "open-code" => HarnessKind::OpenCode,
                "claude-code" => HarnessKind::ClaudeCode,
                _ => return Err("Unknown roadmap provider".into()),
            };
            let suite = suite.as_deref().map(Suite::parse).transpose()?;
            (
                "roadmap-plan",
                file,
                project,
                Some(inspection),
                tasks,
                provider,
                suite,
                output,
            )
        }
    };
    let path =
        std::path::absolute(file).map_err(|_| "Could not resolve the selected roadmap file")?;
    let request = CommandRequest {
        project,
        path,
        inspection,
        tasks,
        provider,
        test_suite,
    };
    let value =
        serde_json::to_string(&request).map_err(|_| "Could not encode the roadmap request")?;
    Ok((
        AgentAction::Workflow {
            command: command.into(),
            value,
        },
        output,
    ))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn roadmap_cli_requires_an_inspection_and_has_no_execution_flag() {
        #[derive(Parser)]
        struct Args {
            #[command(subcommand)]
            command: crate::control::cli::ControlCli,
        }
        let base = [
            "hex",
            "agent",
            "roadmap",
            "plan",
            "roadmap.md",
            "--project",
            "app",
        ];
        assert!(Args::try_parse_from(base).is_err());
        let mut args = base.to_vec();
        args.extend([
            "--inspection",
            "fingerprint",
            "--task",
            "ui",
            "--task",
            "backend",
            "--suite",
            "cargo-agent",
        ]);
        assert!(Args::try_parse_from(&args).is_ok());
        args.push("--approve");
        assert!(Args::try_parse_from(args).is_err());
    }

    #[test]
    fn typed_cli_route_preserves_file_spaces_and_selected_task_ids() {
        let command = RoadmapCommand::Plan {
            file: "roadmaps/next work.md".into(),
            project: "app".into(),
            inspection: "a".repeat(64),
            tasks: vec!["ui".into(), "backend".into()],
            provider: "open-code".into(),
            suite: Some("cargo-agent".into()),
            output: Output::default(),
        };
        let (AgentAction::Workflow { command, value }, _) = route(command).unwrap() else {
            panic!("expected existing workflow transport");
        };
        assert_eq!(command, "roadmap-plan");
        let request: CommandRequest = serde_json::from_str(&value).unwrap();
        assert!(request.path.is_absolute());
        assert!(request.path.ends_with("roadmaps/next work.md"));
        assert_eq!(request.tasks, ["ui", "backend"]);
        assert_eq!(request.test_suite, Some(Suite::CargoAgent));
    }
}

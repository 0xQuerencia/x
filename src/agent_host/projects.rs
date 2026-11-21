use x_agent::projects::Registry;
use x_agent::workflows::paths;

use crate::control::{ControlReply, ProjectCommand};

pub(super) fn handle(command: ProjectCommand) -> ControlReply {
    run(command).unwrap_or_else(|error| ControlReply::error("refused", error))
}

fn run(command: ProjectCommand) -> Result<ControlReply, String> {
    let directory = crate::app_paths::support_dir().map_err(|error| error.to_string())?;
    let registry = Registry::new(directory, paths::home()?);
    match command {
        ProjectCommand::List => Ok(ControlReply::AgentProjects {
            projects: registry.list()?,
            registered: false,
        }),
        ProjectCommand::Register { name, path } => {
            let (project, registered) = registry.register(&name, &path)?;
            Ok(ControlReply::AgentProjects {
                projects: vec![project],
                registered,
            })
        }
        ProjectCommand::Forget { id } => Ok(if registry.forget(&id)? {
            ControlReply::done("Project removed from X's registry")
        } else {
            ControlReply::unchanged("No project has that ID")
        }),
    }
}

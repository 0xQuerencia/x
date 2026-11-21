use x_agent::projects::Registry;
use x_agent::session_links::Store as SessionStore;
use x_agent::workflows::paths;
use x_agent::service::Service;

use crate::control::{ControlReply, ProjectCommand};

pub(super) fn handle(service: &Service, command: ProjectCommand) -> ControlReply {
    run(service, command).unwrap_or_else(|error| ControlReply::error("refused", error))
}

fn run(service: &Service, command: ProjectCommand) -> Result<ControlReply, String> {
    let directory = crate::app_paths::support_dir().map_err(|error| error.to_string())?;
    let registry = Registry::new(directory.clone(), paths::home()?);
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
        ProjectCommand::Use { query } => {
            let project = registry.validated_resolve(&query)?;
            let shown = service.set_workspace(
                project
                    .root
                    .to_str()
                    .ok_or("The project folder has an unsupported path")?,
            )?;
            let (_, changed) = registry.select(&project.id.to_string())?;
            if changed {
                Ok(ControlReply::done(format!("Selected {} ({shown})", project.name)))
            } else {
                Ok(ControlReply::unchanged(format!("Already selected {} ({shown})", project.name)))
            }
        }
        ProjectCommand::AliasAdd { id, alias } => {
            let (project, changed) = registry.add_alias(&id, &alias)?;
            if changed {
                Ok(ControlReply::done(format!("Added alias for {}", project.name)))
            } else {
                Ok(ControlReply::unchanged("That alias already exists"))
            }
        }
        ProjectCommand::AliasRemove { id, alias } => {
            let (project, changed) = registry.remove_alias(&id, &alias)?;
            if changed {
                Ok(ControlReply::done(format!("Removed alias from {}", project.name)))
            } else {
                Ok(ControlReply::unchanged("That alias was not registered"))
            }
        }
        ProjectCommand::Forget { id } => {
            let project = registry
                .list()?
                .into_iter()
                .find(|project| project.id.to_string() == id);
            if registry.forget(&id)? {
                if let Some(project) = project {
                    SessionStore::new(directory)
                        .forget_project(project.id)
                        .map_err(|error| {
                            format!("Project removed, but session metadata remains: {error}")
                        })?;
                }
                Ok(ControlReply::done("Project removed from X's registry"))
            } else {
                Ok(ControlReply::unchanged("No project has that ID"))
            }
        }
    }
}

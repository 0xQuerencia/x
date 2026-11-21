use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use scrawl_router::opencode::{self, ApiGet, OpenCodeSession, Scan};
use x_agent::harness::{Session, SessionId};
use x_agent::service::Service;

use crate::control::ControlReply;
use crate::desktop::opencode_service;

const MAX_SESSIONS: usize = 100;
const DEADLINE: Duration = Duration::from_secs(4);

pub(super) fn handle(service: &Service, resume: Option<String>) -> ControlReply {
    let result = sessions(service, resume);
    match result {
        Ok((sessions, selected)) => ControlReply::AgentSessions { sessions, selected },
        Err(error) => ControlReply::error("refused", error),
    }
}

fn sessions(service: &Service, resume: Option<String>) -> Result<(Vec<Session>, bool), String> {
    let workspace = service
        .workspace_path()
        .ok_or("Choose a project with `hex agent workspace` first")?;
    let id = resume.map(SessionId::try_from).transpose()?;
    with_lookup(|get| {
        if let Some(id) = id {
            let selected = read_selected(get, &workspace, &id)?;
            service.select_session(selected.clone())?;
            Ok((vec![selected], true))
        } else {
            Ok((read_sessions(get, &workspace)?, false))
        }
    })
}

pub(super) fn verify(session: &Session) -> Result<(), String> {
    with_lookup(|get| read_selected(get, &session.workspace, &session.id).map(drop))
}

fn with_lookup<T>(run: impl FnOnce(&mut ApiGet<'_>) -> Result<T, String>) -> Result<T, String> {
    let deadline = Instant::now() + DEADLINE;
    let connection = opencode_service::find_bounded(DEADLINE).map_err(|error| error.message())?;
    let cancelled = AtomicBool::new(false);
    let mut get = |path: &str| {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let response = connection
            .request("GET", path, None, None, remaining, &cancelled)
            .map_err(|_| anyhow::anyhow!("Could not read the registered OpenCode service"))?;
        match response.status {
            200 => Ok(response.body),
            401 | 403 => anyhow::bail!("OpenCode refused the registered service credentials"),
            404 => anyhow::bail!("OpenCode could not find that session"),
            code => anyhow::bail!("OpenCode session request failed with HTTP {code}"),
        }
    };
    run(&mut get)
}

fn scoped(raw: &OpenCodeSession, workspace: &Path) -> Option<Session> {
    if !raw.is_open_root() {
        return None;
    }
    Session::from_open_code(
        raw.id.clone(),
        raw.label(),
        Path::new(raw.directory()?),
        workspace,
        raw.time.updated,
    )
}

fn read_sessions(get: &mut ApiGet<'_>, workspace: &Path) -> Result<Vec<Session>, String> {
    let directory = workspace
        .to_str()
        .ok_or("The project path cannot be sent to OpenCode")?;
    let query = format!("&directory={}", opencode::query_escape(directory));
    let mut sessions = Vec::new();
    let mut ids = std::collections::HashSet::new();
    opencode::find_root_session(get, &query, opencode::ROOT_SEARCH_PAGES, |raw| {
        if let Some(session) = scoped(raw, workspace)
            && ids.insert(session.id.clone())
        {
            sessions.push(session);
        }
        if sessions.len() == MAX_SESSIONS {
            Scan::Stop
        } else {
            Scan::Skip
        }
    })
    .map_err(|error| error.to_string())?;
    Ok(sessions)
}

fn read_selected(
    get: &mut ApiGet<'_>,
    workspace: &Path,
    id: &SessionId,
) -> Result<Session, String> {
    let raw = opencode::get_session(get, id.as_str()).map_err(|error| error.to_string())?;
    if raw.id != id.as_str() {
        return Err("OpenCode returned a different session ID".into());
    }
    scoped(&raw, workspace)
        .ok_or_else(|| "That OpenCode session is unavailable in this project".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn discover_filters_to_open_unique_sessions_in_the_exact_project() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().canonicalize().unwrap();
        let nested = workspace.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let mut get = |path: &str| {
            assert!(path.starts_with("/api/session?parentID=null&limit=100&directory="));
            Ok(serde_json::to_vec(&json!({"data":[
                {"id":"ses_valid", "title":"real", "location":{"directory":workspace}},
                {"id":"ses_valid", "title":"duplicate", "directory":workspace},
                {"id":"ses_outside", "directory":nested},
                {"id":"ses_child", "directory":workspace,"parentID":"ses_valid"},
                {"id":"ses_archived", "directory":workspace,"time":{"archived":42}},
                {"id":"title only", "directory":workspace}
            ]}))
            .unwrap())
        };
        let sessions = read_sessions(&mut get, &workspace).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id.as_str(), "ses_valid");
        assert_eq!(sessions[0].title, "real");
    }

    #[test]
    fn resume_requires_identity_from_a_fresh_exact_lookup() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().canonicalize().unwrap();
        let id = SessionId::try_from("ses_requested".to_string()).unwrap();
        let mut get = |path: &str| {
            assert_eq!(path, "/api/session/ses_requested");
            Ok(
                serde_json::to_vec(&json!({"data": {"id":"ses_different","directory":workspace}}))
                    .unwrap(),
            )
        };
        let error = read_selected(&mut get, &workspace, &id).unwrap_err();
        assert!(error.contains("different session ID"));
        let mut get = |path: &str| {
            assert_eq!(path, "/api/session/ses_requested");
            Ok(
                serde_json::to_vec(&json!({"data": {"id":"ses_requested","directory":workspace}}))
                    .unwrap(),
            )
        };
        assert_eq!(read_selected(&mut get, &workspace, &id).unwrap().id, id);
    }
}

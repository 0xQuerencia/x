//! Local metadata for provider-confirmed coding sessions; never a source of execution authority.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::harness::{Kind, Session, SessionId};
use crate::projects::Project;

const FILE: &str = "x-session-links.json";
const MAX_BYTES: usize = 1024 * 1024;
const MAX_LINKS: usize = 1024;
const VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    Observed,
    Missing,
    Unverified,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub project_id: Uuid,
    pub session_id: SessionId,
    pub workspace: PathBuf,
    pub last_seen_at: u64,
    pub presence: Presence,
}

#[derive(Serialize, Deserialize)]
struct Data {
    version: u32,
    links: Vec<Link>,
}

#[derive(Clone)]
pub struct Store {
    directory: PathBuf,
}

impl Store {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    /// Saved presence is historical; only a current provider read may confirm availability.
    pub fn list(&self, project: &Project) -> Result<Vec<Link>, String> {
        Ok(self
            .read()?
            .links
            .into_iter()
            .filter(|link| link.project_id == project.id && link.workspace == project.root)
            .map(|mut link| {
                link.presence = Presence::Unverified;
                link
            })
            .collect())
    }

    /// Call only with an ID acknowledged by a structured harness event or verified provider API.
    pub fn observe(
        &self,
        project: &Project,
        session: &Session,
        observed_at: u64,
    ) -> Result<Link, String> {
        validate_project_session(project, session)?;
        self.update(|data| {
            if let Some(link) = data
                .links
                .iter_mut()
                .find(|link| link.project_id == project.id && link.session_id == session.id)
            {
                link.presence = Presence::Observed;
                link.last_seen_at = observed_at.max(link.last_seen_at);
                return Ok((link.clone(), true));
            }
            if data.links.len() >= MAX_LINKS {
                return Err("The session index is full".into());
            }
            let link = Link {
                project_id: project.id,
                session_id: session.id.clone(),
                workspace: project.root.clone(),
                last_seen_at: observed_at,
                presence: Presence::Observed,
            };
            data.links.push(link.clone());
            Ok((link, true))
        })
    }

    /// `None` means provider lookup failed, not that sessions disappeared.
    /// A successful full listing is authoritative only for its own harness.
    pub fn reconcile(
        &self,
        project: &Project,
        kind: Kind,
        sessions: Option<&[Session]>,
        observed_at: u64,
    ) -> Result<Vec<Link>, String> {
        validate_project(project)?;
        if let Some(sessions) = sessions {
            for session in sessions {
                validate_project_session(project, session)?;
                if session.id.kind() != kind {
                    return Err("Session listing mixed different harnesses".into());
                }
            }
        }
        self.update(|data| {
            for link in data
                .links
                .iter_mut()
                .filter(|link| link.project_id == project.id && link.session_id.kind() == kind)
            {
                link.presence = match sessions {
                    None => Presence::Unverified,
                    Some(sessions) if sessions.iter().any(|s| s.id == link.session_id) => {
                        link.last_seen_at = observed_at.max(link.last_seen_at);
                        Presence::Observed
                    }
                    Some(_) => Presence::Missing,
                };
            }
            if let Some(sessions) = sessions {
                for session in sessions {
                    if data
                        .links
                        .iter()
                        .any(|link| link.project_id == project.id && link.session_id == session.id)
                    {
                        continue;
                    }
                    if data.links.len() >= MAX_LINKS {
                        return Err("The session index is full".into());
                    }
                    data.links.push(Link {
                        project_id: project.id,
                        session_id: session.id.clone(),
                        workspace: project.root.clone(),
                        last_seen_at: observed_at,
                        presence: Presence::Observed,
                    });
                }
            }
            let result = data
                .links
                .iter()
                .filter(|link| link.project_id == project.id && link.session_id.kind() == kind)
                .cloned()
                .collect();
            Ok((result, true))
        })
    }

    pub fn forget_project(&self, project_id: Uuid) -> Result<(), String> {
        self.update(|data| {
            let before = data.links.len();
            data.links.retain(|link| link.project_id != project_id);
            Ok(((), data.links.len() != before))
        })
    }

    fn read(&self) -> Result<Data, String> {
        let path = self.directory.join(FILE);
        let file = match open_regular(&path, false) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Ok(Data {
                    version: VERSION,
                    links: vec![],
                });
            }
            Err(e) => return Err(format!("Cannot read the session index: {e}")),
        };
        let mut buf = vec![];
        file.take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut buf)
            .map_err(|e| format!("Cannot read the session index: {e}"))?;
        if buf.len() > MAX_BYTES {
            return Err("Session index is too large".into());
        }
        let data: Data =
            serde_json::from_slice(&buf).map_err(|_| "Cannot decode the session index")?;
        if data.version != VERSION {
            return Err("The session index uses an unknown schema".into());
        }
        let mut keys = HashSet::new();
        if data.links.len() > MAX_LINKS
            || data.links.iter().any(|link| {
                link.project_id.is_nil()
                    || !link.workspace.is_absolute()
                    || !keys.insert((link.project_id, link.session_id.clone()))
            })
        {
            return Err("Session index has invalid entries".into());
        }
        Ok(data)
    }

    fn update<T>(
        &self,
        modify: impl FnOnce(&mut Data) -> Result<(T, bool), String>,
    ) -> Result<T, String> {
        fs::create_dir_all(&self.directory)
            .map_err(|e| format!("Cannot create session index folder: {e}"))?;
        let lock = open_regular(&self.directory.join("x-session-links.lock"), true)
            .map_err(|e| format!("Cannot open session index lock: {e}"))?;
        lock.try_lock()
            .map_err(|e| format!("Cannot lock session index: {e}"))?;
        let result = (|| {
            let mut data = self.read()?;
            let (value, changed) = modify(&mut data)?;
            if changed {
                let mut temp = tempfile::NamedTempFile::new_in(&self.directory)
                    .map_err(|e| format!("Cannot create session index: {e}"))?;
                let bytes = serde_json::to_vec(&data)
                    .map_err(|e| format!("Cannot encode session index: {e}"))?;
                if bytes.len() > MAX_BYTES {
                    return Err("Session index is too large".into());
                }
                temp.write_all(&bytes)
                    .and_then(|_| temp.as_file().sync_all())
                    .map_err(|e| format!("Cannot write session index: {e}"))?;
                temp.persist(self.directory.join(FILE))
                    .map_err(|e| format!("Cannot save session index: {e}"))?;
                #[cfg(unix)]
                File::open(&self.directory)
                    .and_then(|dir| dir.sync_all())
                    .map_err(|e| format!("Cannot sync session index: {e}"))?;
            }
            Ok(value)
        })();
        let unlocked = lock.unlock();
        let result = result?;
        unlocked.map_err(|e| format!("Cannot unlock session index: {e}"))?;
        Ok(result)
    }
}

fn validate_project(project: &Project) -> Result<(), String> {
    if project.id.is_nil()
        || !project.root.is_absolute()
        || project
            .root
            .canonicalize()
            .map_err(|_| "The project directory is unavailable")?
            != project.root
    {
        return Err("The project folder is stale or not canonical".into());
    }
    Ok(())
}

fn validate_project_session(project: &Project, session: &Session) -> Result<(), String> {
    validate_project(project)?;
    if session.workspace != project.root {
        return Err("That coding session belongs to a different project".into());
    }
    Ok(())
}

fn open_regular(path: &Path, create: bool) -> std::io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(std::io::Error::other("Expected a regular file"));
        }
        Ok(_) => {}
        Err(error) if create && error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(create).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Project, Store) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        fs::create_dir(&root).unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "example".into(),
            aliases: vec![],
            root,
            git_common_dir: None,
        };
        let store = Store::new(dir.path().join("metadata"));
        (dir, project, store)
    }

    fn session(id: &str, project: &Project) -> Session {
        Session {
            id: SessionId::try_from(id.to_string()).unwrap(),
            title: "title never persisted".into(),
            workspace: project.root.clone(),
            updated_at: 10,
        }
    }

    #[test]
    fn acknowledged_session_is_persisted_but_unverified_after_restart() {
        let (dir, project, store) = fixture();
        let seen = store
            .observe(&project, &session("ses_one", &project), 10)
            .unwrap();
        assert_eq!(seen.presence, Presence::Observed);
        let restarted = Store::new(dir.path().join("metadata"));
        let old = restarted.list(&project).unwrap();
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].session_id.as_str(), "ses_one");
        assert_eq!(old[0].presence, Presence::Unverified);
        assert!(
            !fs::read_to_string(dir.path().join("metadata/x-session-links.json"))
                .unwrap()
                .contains("title never persisted")
        );
    }

    #[test]
    fn absent_provider_is_unknown_not_missing_or_success() {
        let (_dir, project, store) = fixture();
        store
            .observe(&project, &session("ses_one", &project), 10)
            .unwrap();
        let result = store.reconcile(&project, Kind::OpenCode, None, 20).unwrap();
        assert_eq!(result[0].presence, Presence::Unverified);
        let absent = store
            .reconcile(&project, Kind::OpenCode, Some(&[]), 30)
            .unwrap();
        assert_eq!(absent[0].presence, Presence::Missing);
        assert_eq!(absent[0].last_seen_at, 10);
    }

    #[test]
    fn each_provider_reconciles_only_its_own_sessions() {
        let (_dir, project, store) = fixture();
        let open_code = session("ses_one", &project);
        let claude = session("123e4567-e89b-12d3-a456-426614174000", &project);
        store.observe(&project, &open_code, 10).unwrap();
        store.observe(&project, &claude, 10).unwrap();
        let result = store
            .reconcile(&project, Kind::OpenCode, Some(&[open_code]), 20)
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].last_seen_at, 20);
        let other = store
            .reconcile(&project, Kind::ClaudeCode, None, 30)
            .unwrap();
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].presence, Presence::Unverified);
    }

    #[test]
    fn mismatched_or_stale_project_is_rejected() {
        let (dir, project, store) = fixture();
        let other = dir.path().join("other");
        fs::create_dir(&other).unwrap();
        let wrong = Session {
            workspace: other,
            ..session("ses_wrong", &project)
        };
        assert!(store.observe(&project, &wrong, 10).is_err());
        assert!(store.list(&project).unwrap().is_empty());
        fs::remove_dir(&project.root).unwrap();
        assert!(
            store
                .observe(&project, &session("ses_one", &project), 20)
                .is_err()
        );
    }

    #[test]
    fn removing_link_metadata_does_not_touch_project_files() {
        let (_dir, project, store) = fixture();
        fs::write(project.root.join("file.txt"), "keep").unwrap();
        store
            .observe(&project, &session("ses_one", &project), 10)
            .unwrap();
        store.forget_project(project.id).unwrap();
        assert!(store.list(&project).unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(project.root.join("file.txt")).unwrap(),
            "keep"
        );
    }
}

//! Explicitly registered local projects and their stable X identities.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::workflows::paths;

const FILE: &str = "x-projects.json";
const VERSION: u32 = 1;
const MAX_PROJECTS: usize = 256;
const MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub root: PathBuf,
    pub git_common_dir: Option<PathBuf>,
}

#[derive(Deserialize, Serialize)]
struct Data {
    version: u32,
    projects: Vec<Project>,
}

pub struct Registry {
    directory: PathBuf,
    home: PathBuf,
}

impl Registry {
    pub fn new(directory: PathBuf, home: PathBuf) -> Self {
        Self { directory, home }
    }

    pub fn list(&self) -> Result<Vec<Project>, String> {
        Ok(self.read()?.projects)
    }

    pub fn register(&self, name: &str, raw: &str) -> Result<(Project, bool), String> {
        let name = name.trim();
        if !valid_name(name) {
            return Err(
                "Enter a project name of 1–80 characters without control characters".into(),
            );
        }
        let root = project_root(raw, &self.home)?;
        let git_common_dir = git_common_dir(&root);
        self.update(|projects| {
            if let Some(project) = projects.iter().find(|project| project.root == root) {
                return Ok((project.clone(), false));
            }
            if projects.len() >= MAX_PROJECTS {
                return Err("The project registry is full".into());
            }
            let project = Project {
                id: Uuid::new_v4(),
                name: name.into(),
                root,
                git_common_dir,
            };
            projects.push(project.clone());
            Ok((project, true))
        })
    }

    pub fn forget(&self, raw_id: &str) -> Result<bool, String> {
        let id = Uuid::parse_str(raw_id).map_err(|_| "Enter an exact project ID from the list")?;
        self.update(|projects| {
            let old_count = projects.len();
            projects.retain(|project| project.id != id);
            let removed = projects.len() != old_count;
            Ok((removed, removed))
        })
        .map(|(removed, _)| removed)
    }

    fn read(&self) -> Result<Data, String> {
        let path = self.directory.join(FILE);
        let file = match open_regular(&path, false) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(Data {
                    version: VERSION,
                    projects: Vec::new(),
                });
            }
            Err(error) => return Err(format!("Could not read the project registry: {error}")),
        };
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("Could not read the project registry: {error}"))?;
        if bytes.len() > MAX_BYTES {
            return Err("The project registry is too large".into());
        }
        let data: Data =
            serde_json::from_slice(&bytes).map_err(|_| "Could not decode the project registry")?;
        if data.version != VERSION {
            return Err("The project registry uses an unknown schema".into());
        }
        let mut ids = HashSet::new();
        let mut roots = HashSet::new();
        if data.projects.len() > MAX_PROJECTS
            || data.projects.iter().any(|project| {
                project.id.is_nil()
                    || !ids.insert(project.id)
                    || !roots.insert(&project.root)
                    || !project.root.is_absolute()
                    || !valid_name(&project.name)
                    || project
                        .git_common_dir
                        .as_ref()
                        .is_some_and(|path| !path.is_absolute())
            })
        {
            return Err("The project registry has invalid project entries".into());
        }
        Ok(data)
    }

    fn update<T>(
        &self,
        change: impl FnOnce(&mut Vec<Project>) -> Result<(T, bool), String>,
    ) -> Result<(T, bool), String> {
        fs::create_dir_all(&self.directory)
            .map_err(|error| format!("Could not create the project registry directory: {error}"))?;
        let lock = open_regular(&self.directory.join("x-projects.lock"), true)
            .map_err(|error| format!("Could not open the project registry lock: {error}"))?;
        lock.try_lock()
            .map_err(|error| format!("Could not lock the project registry: {error}"))?;
        let result: Result<_, String> = (|| {
            let mut data = self.read()?;
            let (value, changed) = change(&mut data.projects)?;
            if changed {
                self.write(&data)
                    .map_err(|error| format!("Could not save the project registry: {error}"))?;
            }
            Ok((value, changed))
        })();
        let unlocked = lock.unlock();
        let value = result?;
        unlocked.map_err(|error| format!("Could not unlock the project registry: {error}"))?;
        Ok(value)
    }

    fn write(&self, data: &Data) -> std::io::Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        let bytes = serde_json::to_vec(data)?;
        if bytes.len() > MAX_BYTES {
            return Err(std::io::Error::other("The project registry is too large"));
        }
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist(self.directory.join(FILE))?;
        #[cfg(unix)]
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
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

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= 80 && !name.chars().any(char::is_control)
}

fn project_root(raw: &str, home: &Path) -> Result<PathBuf, String> {
    let wanted = paths::check(raw, home)?;
    let home_real = home
        .canonicalize()
        .map_err(|_| "Could not resolve your home folder")?;
    let root = crate::opencode::validate_workspace(&paths::existing(raw, home)?)?;
    for (candidate, anchor) in [(&wanted, home), (&root, home_real.as_path())] {
        let relative = candidate
            .strip_prefix(anchor)
            .map_err(|_| "Choose a project inside your home folder")?;
        if relative.as_os_str().is_empty()
            || relative.components().any(|part| {
                let name = part.as_os_str().to_string_lossy();
                name.starts_with('.')
                    || (cfg!(windows) && name.eq_ignore_ascii_case("AppData"))
                    || (cfg!(target_os = "macos") && name.eq_ignore_ascii_case("Library"))
            })
        {
            return Err("Choose a project outside your home or private configuration roots".into());
        }
    }
    Ok(root)
}

fn git_common_dir(root: &Path) -> Option<PathBuf> {
    // Only probe a Git root explicitly chosen for registration.
    if !root.join(".git").exists() {
        return None;
    }
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    let (true, output) = crate::desktop::capture(&mut command, Duration::from_secs(2)).ok()? else {
        return None;
    };
    let path = Path::new(
        std::str::from_utf8(&output)
            .ok()?
            .trim_end_matches(['\r', '\n']),
    );
    path.canonicalize().ok().filter(|path| path.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Registry) {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        fs::create_dir(&home).unwrap();
        let registry = Registry::new(directory.path().join("state"), home);
        (directory, registry)
    }

    #[test]
    fn registry_survives_restart_and_forgets_only_metadata() {
        let (_directory, registry) = fixture();
        let root = registry.home.join("source").join("example");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("test.txt"), "keep me").unwrap();
        assert!(registry.list().unwrap().is_empty());
        let (project, changed) = registry.register(" Example ", "~/source/example").unwrap();
        assert!(changed);
        assert_eq!(project.root, root.canonicalize().unwrap());
        assert_eq!(project.name, "Example");
        assert_eq!(project.git_common_dir, None);

        let registry = Registry::new(registry.directory, registry.home);
        assert_eq!(registry.list().unwrap(), std::slice::from_ref(&project));
        assert_eq!(
            registry
                .register("Different name", root.to_str().unwrap())
                .unwrap(),
            (project.clone(), false)
        );
        assert!(registry.forget(&project.id.to_string()).unwrap());
        assert!(!registry.forget(&project.id.to_string()).unwrap());
        assert!(registry.list().unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(root.join("test.txt")).unwrap(),
            "keep me"
        );
    }

    #[test]
    fn refuses_home_traversal_credential_paths_and_files() {
        let (_directory, registry) = fixture();
        for path in [".config/secret", "source/.ssh", "source/project"] {
            fs::create_dir_all(registry.home.join(path)).unwrap();
        }
        fs::write(registry.home.join("source/file"), b"").unwrap();
        for path in [
            "/",
            "~",
            "~/source/../source/project",
            "~/.config/secret",
            "~/source/.ssh",
            "~/source/file",
            "~/source/missing",
        ] {
            assert!(registry.register("example", path).is_err(), "{path}");
        }
        assert!(registry.list().unwrap().is_empty());
        assert!(!registry.directory.exists());
    }

    #[cfg(unix)]
    #[test]
    fn link_cannot_escape_into_another_tree_or_hide_credential_directory() {
        use std::os::unix::fs::symlink;

        let (directory, registry) = fixture();
        let outside = directory.path().join("outside");
        let hidden = registry.home.join("source/.aws");
        fs::create_dir(&outside).unwrap();
        fs::create_dir_all(&hidden).unwrap();
        symlink(outside, registry.home.join("escape")).unwrap();
        symlink(hidden, registry.home.join("credentials")).unwrap();
        for path in ["~/escape", "~/credentials"] {
            assert!(registry.register("example", path).is_err(), "{path}");
        }
    }

    #[test]
    fn unknown_schema_is_never_overwritten() {
        let (_directory, registry) = fixture();
        fs::create_dir(&registry.directory).unwrap();
        fs::create_dir(registry.home.join("project")).unwrap();
        let path = registry.directory.join(FILE);
        let original = br#"{"version":99,"projects":[]}"#;
        fs::write(&path, original).unwrap();
        assert!(registry.list().unwrap_err().contains("unknown schema"));
        assert!(registry.register("example", "~/project").is_err());
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn competing_writer_preserves_the_existing_project() {
        let (_directory, registry) = fixture();
        fs::create_dir(registry.home.join("first")).unwrap();
        fs::create_dir(registry.home.join("second")).unwrap();
        let (first, _) = registry.register("First", "~/first").unwrap();
        let lock = open_regular(&registry.directory.join("x-projects.lock"), false).unwrap();
        lock.try_lock().unwrap();
        assert!(
            registry
                .register("Second", "~/second")
                .unwrap_err()
                .contains("lock")
        );
        assert_eq!(registry.list().unwrap(), std::slice::from_ref(&first));
        lock.unlock().unwrap();
        let (second, _) = registry.register("Second", "~/second").unwrap();
        assert_eq!(registry.list().unwrap(), [first, second]);
    }

    #[test]
    fn git_identity_is_shared_by_linked_worktrees() {
        let (_directory, registry) = fixture();
        let origin = registry.home.join("origin");
        let linked = registry.home.join("linked");
        fs::create_dir(&origin).unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=X fixture",
                "-c",
                "user.email=x@invalid.example",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "fixture",
            ],
            vec![
                "worktree",
                "add",
                "-q",
                "--detach",
                linked.to_str().unwrap(),
            ],
        ] {
            let status = Command::new("git")
                .args(args)
                .current_dir(&origin)
                .status()
                .unwrap();
            assert!(status.success());
        }
        let (project, _) = registry.register("main", "~/origin").unwrap();
        let (worktree, _) = registry.register("linked", "~/linked").unwrap();
        assert_ne!(project.id, worktree.id);
        assert_eq!(
            project.git_common_dir,
            Some(origin.join(".git").canonicalize().unwrap())
        );
        assert_eq!(worktree.git_common_dir, project.git_common_dir);
    }
}

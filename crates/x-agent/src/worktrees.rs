//! Explicitly owned, local Git worktrees for isolated coding jobs.
//! No automatic merge, force-removal, network operation or user-tree mutation.

use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use uuid::Uuid;

use crate::projects::Project;

const TIMEOUT: Duration = Duration::from_secs(12);

pub struct Manager {
    base: PathBuf,
    home: PathBuf,
}

#[derive(Clone, Debug)]
pub struct OwnedWorktree {
    project_id: Uuid,
    path: PathBuf,
    run_id: Uuid,
    task_id: String,
    start_commit: String,
}

impl OwnedWorktree {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn start_commit(&self) -> &str {
        &self.start_commit
    }

    pub fn run_id(&self) -> Uuid {
        self.run_id
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }
}

impl Manager {
    /// The owner chooses an existing, visible directory inside their home.
    /// Creating system folders or silently moving worktrees is not supported.
    pub fn new(base: PathBuf, home: PathBuf) -> Self {
        Self { base, home }
    }

    pub(crate) fn starting_commit(&self, project: &Project) -> Result<String, String> {
        self.verify_project(project)?;
        current_commit(&project.root)
    }

    pub(crate) fn verify_start(
        &self,
        project: &Project,
        worktree: &OwnedWorktree,
    ) -> Result<(), String> {
        let path = self.validate_owned(project, worktree)?;
        if current_commit(&path)? != worktree.start_commit() {
            return Err("The task worktree changed its approved starting commit".into());
        }
        let status = git(
            &path,
            &["status", "--porcelain=v1", "--untracked-files=all", "--ignored"],
        )?;
        if !status.trim().is_empty() {
            return Err("The task worktree is not clean at its approved starting commit".into());
        }
        Ok(())
    }

    /// Requires explicit user approval for this exact run/task/project tuple.
    /// The caller's policy gate must make sure `approved` came from the owner.
    pub fn allocate(
        &self,
        project: &Project,
        run_id: Uuid,
        task_id: &str,
        start_commit: &str,
        approved: bool,
    ) -> Result<OwnedWorktree, String> {
        if !approved {
            return Err("Creating a worktree requires explicit approval".into());
        }
        if run_id.is_nil() || !valid_slug(task_id) || !valid_commit(start_commit) {
            return Err("Invalid worktree task or starting commit".into());
        }
        let base = self.validated_base()?;
        let common = self.verify_project(project)?;
        let parent = base.join(project.id.to_string());
        if parent.exists() {
            let metadata = fs::symlink_metadata(&parent)
                .map_err(|_| "Cannot inspect the X worktree directory")?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("X worktree directory is not a regular folder".into());
            }
        } else {
            fs::create_dir(&parent).map_err(|_| "Cannot create the X worktree directory")?;
        }
        let name = format!("{run_id}-{task_id}");
        let destination = parent.join(name);
        if fs::symlink_metadata(&destination).is_ok() {
            return Err("This task already has a worktree path".into());
        }
        let lock = open_lock(&base.join("x-worktrees.lock"))?;
        lock.try_lock()
            .map_err(|_| "Another worktree operation is running")?;
        let result: Result<OwnedWorktree, String> = (|| {
            // A fresh check under the lock prevents collisions during concurrent runs.
            if fs::symlink_metadata(&destination).is_ok() {
                return Err("This task already has a worktree path".into());
            }
            let rev = format!("{start_commit}^{{commit}}");
            let verified = git(&project.root, &["rev-parse", "--verify", "--quiet", &rev])?;
            if verified.trim() != start_commit {
                return Err("The starting commit does not exist in the project".into());
            }
            let destination_text = destination
                .to_str()
                .ok_or("The worktree path cannot be represented")?;
            git(
                &project.root,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    "--",
                    destination_text,
                    start_commit,
                ],
            )?;
            let resolved = destination
                .canonicalize()
                .map_err(|_| "The worktree was created but its folder cannot be verified")?;
            if resolved != destination {
                return Err("The worktree resolved outside the approved directory".into());
            }
            let observed_common = git_common_dir(&resolved)?;
            if observed_common != common {
                return Err("The new worktree does not belong to the selected repository".into());
            }
            Ok(OwnedWorktree {
                project_id: project.id,
                path: resolved,
                run_id,
                task_id: task_id.into(),
                start_commit: start_commit.into(),
            })
        })();
        let unlocked = lock.unlock();
        let owned = result?;
        unlocked.map_err(|_| "Could not unlock the worktree registry")?;
        Ok(owned)
    }

    /// Re-validate an owned worktree before using it for a coding session.
    /// A saved string path or model-supplied folder is not an ownership token.
    pub fn validate_owned(
        &self,
        project: &Project,
        worktree: &OwnedWorktree,
    ) -> Result<PathBuf, String> {
        let base = self.validated_base()?;
        let common = self.verify_project(project)?;
        if project.id != worktree.project_id
            || worktree.run_id.is_nil()
            || !valid_slug(&worktree.task_id)
            || !valid_commit(&worktree.start_commit)
        {
            return Err("The worktree ownership record is invalid".into());
        }
        let expected = base
            .join(project.id.to_string())
            .join(format!("{}-{}", worktree.run_id, worktree.task_id));
        if worktree.path != expected
            || !worktree.path.is_dir()
            || worktree
                .path
                .canonicalize()
                .map_err(|_| "The worktree path no longer exists")?
                != worktree.path
        {
            return Err("The worktree path has changed".into());
        }
        if git_common_dir(&worktree.path)? != common {
            return Err("The worktree no longer belongs to the selected Git repository".into());
        }
        Ok(worktree.path.clone())
    }

    /// Reconstruct ownership only from the locally generated X path and
    /// independently verified Git common directory. Arbitrary user paths
    /// never become trusted worktrees.
    pub fn reattach(
        &self,
        project: &Project,
        run_id: Uuid,
        task_id: &str,
        start_commit: &str,
    ) -> Result<OwnedWorktree, String> {
        if run_id.is_nil() || !valid_slug(task_id) || !valid_commit(start_commit) {
            return Err("Enter an exact X run, task and starting commit".into());
        }
        let base = self.validated_base()?;
        let path = base
            .join(project.id.to_string())
            .join(format!("{run_id}-{task_id}"));
        let owned = OwnedWorktree {
            project_id: project.id,
            path,
            run_id,
            task_id: task_id.into(),
            start_commit: start_commit.into(),
        };
        self.validate_owned(project, &owned)?;
        let verified = git(
            &project.root,
            &[
                "rev-parse",
                "--verify",
                &format!("{start_commit}^{{commit}}"),
            ],
        )?;
        if verified.trim() != start_commit {
            return Err("The recorded starting commit no longer exists".into());
        }
        git(
            owned.path(),
            &["merge-base", "--is-ancestor", start_commit, "HEAD"],
        )?;
        Ok(owned)
    }

    /// Never delete edited, untracked, ignored or otherwise unknown files.
    /// An owner-approved cleanup uses Git without `--force` and never deletes
    /// the primary checkout.
    pub fn remove_if_clean(
        &self,
        project: &Project,
        worktree: &OwnedWorktree,
        approved: bool,
    ) -> Result<(), String> {
        if !approved {
            return Err("Removing a worktree requires explicit approval".into());
        }
        let base = self.validated_base()?;
        let common = self.verify_project(project)?;
        if project.id != worktree.project_id
            || worktree.run_id.is_nil()
            || !valid_slug(&worktree.task_id)
            || !valid_commit(&worktree.start_commit)
        {
            return Err("The worktree ownership record is invalid".into());
        }
        let expected = base
            .join(project.id.to_string())
            .join(format!("{}-{}", worktree.run_id, worktree.task_id));
        if worktree.path != expected
            || !worktree.path.starts_with(&base)
            || !worktree.path.is_dir()
            || worktree
                .path
                .canonicalize()
                .map_err(|_| "Worktree folder no longer exists")?
                != worktree.path
        {
            return Err("The worktree path has changed".into());
        }
        if git_common_dir(&worktree.path)? != common {
            return Err("That path is no longer a worktree of the chosen project".into());
        }
        let lock = open_lock(&base.join("x-worktrees.lock"))?;
        lock.try_lock()
            .map_err(|_| "Another worktree operation is running")?;
        let result: Result<(), String> = (|| {
            let dirty = git(
                &worktree.path,
                &[
                    "status",
                    "--porcelain=v1",
                    "--untracked-files=all",
                    "--ignored",
                ],
            )?;
            if !dirty.trim().is_empty() {
                return Err(
                    "Worktree contains changes or untracked/ignored files; nothing was removed"
                        .into(),
                );
            }
            let path = worktree
                .path
                .to_str()
                .ok_or("The worktree path cannot be represented")?;
            git(&project.root, &["worktree", "remove", "--", path])?;
            if worktree.path.exists() {
                return Err("Git did not completely remove the worktree".into());
            }
            Ok(())
        })();
        let unlocked = lock.unlock();
        result?;
        unlocked.map_err(|_| "Could not unlock the worktree registry")?;
        Ok(())
    }

    fn validated_base(&self) -> Result<PathBuf, String> {
        let home = self
            .home
            .canonicalize()
            .map_err(|_| "Your home directory is unavailable")?;
        let relative = self
            .base
            .strip_prefix(&self.home)
            .map_err(|_| "Choose a worktree directory inside your home")?;
        if relative.as_os_str().is_empty()
            || relative.components().any(|part| match part {
                Component::Normal(value) => value.to_string_lossy().starts_with('.'),
                _ => true,
            })
        {
            return Err("Choose a visible worktree directory below your home".into());
        }
        let metadata = fs::symlink_metadata(&self.base)
            .map_err(|_| "Create the chosen worktree directory first")?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("The worktree directory cannot be a symlink".into());
        }
        let real = self
            .base
            .canonicalize()
            .map_err(|_| "Cannot resolve the worktree directory")?;
        if !real.starts_with(&home) || real == home {
            return Err("The worktree directory must remain inside your home".into());
        }
        Ok(real)
    }

    fn verify_project(&self, project: &Project) -> Result<PathBuf, String> {
        let home = self
            .home
            .canonicalize()
            .map_err(|_| "Your home directory is unavailable")?;
        let root = project
            .root
            .canonicalize()
            .map_err(|_| "The registered project is unavailable")?;
        if root != project.root || !root.starts_with(&home) || root == home {
            return Err("The registered project root changed".into());
        }
        let expected = project
            .git_common_dir
            .as_ref()
            .ok_or("Register a Git checkout before creating worktrees")?;
        let common = git_common_dir(&root)?;
        if &common != expected {
            return Err("The project Git identity changed".into());
        }
        Ok(common)
    }
}

fn current_commit(root: &Path) -> Result<String, String> {
    let commit = git(root, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let commit = commit.trim();
    if !valid_commit(commit) {
        return Err("Git did not return an exact starting commit".into());
    }
    Ok(commit.into())
}

fn git_common_dir(root: &Path) -> Result<PathBuf, String> {
    let output = git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = Path::new(output.trim())
        .canonicalize()
        .map_err(|_| "Git common directory is unavailable")?;
    if !common.is_dir() {
        return Err("Git common directory is not a folder".into());
    }
    Ok(common)
}

fn valid_slug(value: &str) -> bool {
    (1..=40).contains(&value.len())
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
        && value.as_bytes()[0].is_ascii_alphanumeric()
}

fn valid_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|c| c.is_ascii_hexdigit())
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .args(["-c", "core.hooksPath=", "-c", "core.fsmonitor=false"])
        .args(args);
    let (success, output) = crate::desktop::capture(&mut command, TIMEOUT)
        .map_err(|_| "Git worktree operation failed or timed out")?;
    if !success {
        return Err("Git refused the worktree operation".into());
    }
    String::from_utf8(output).map_err(|_| "Git returned an invalid UTF-8 result".into())
}

fn open_lock(path: &Path) -> Result<File, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err("The X worktree lock file is invalid".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(_) => return Err("Could not inspect the X worktree lock".into()),
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .map_err(|_| "Could not open the X worktree lock".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Project, Manager, String) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir(&home).unwrap();
        let source = home.join("source");
        fs::create_dir(&source).unwrap();
        let base = home.join("XWorktrees");
        fs::create_dir(&base).unwrap();

        let run = |args: &[&str]| {
            let success = Command::new("git")
                .args(args)
                .current_dir(&source)
                .status()
                .unwrap();
            assert!(success.success());
        };
        run(&["init", "-q"]);
        run(&[
            "-c",
            "user.name=X fixture",
            "-c",
            "user.email=x@invalid.example",
            "-c",
            "core.hooksPath=",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "fixture",
        ]);
        let source = source.canonicalize().unwrap();
        let common = source.join(".git").canonicalize().unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "Source".into(),
            aliases: vec![],
            root: source.clone(),
            git_common_dir: Some(common),
        };
        let sha = git(&source, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        (dir, project, Manager::new(base, home), sha)
    }

    #[test]
    fn creates_an_isolated_worktree_without_touching_dirty_original_checkout() {
        let (_dir, project, manager, sha) = fixture();
        fs::write(project.root.join("local-untracked.txt"), "never delete").unwrap();
        let before = git(&project.root, &["status", "--porcelain=v1"]).unwrap();
        let owned = manager
            .allocate(&project, Uuid::new_v4(), "backend", &sha, true)
            .unwrap();
        assert!(owned.path().is_dir());
        assert_eq!(owned.start_commit(), sha);
        assert_eq!(
            git_common_dir(owned.path()).unwrap(),
            project.git_common_dir.unwrap()
        );
        assert_eq!(
            git(&project.root, &["status", "--porcelain=v1"]).unwrap(),
            before
        );
    }

    #[test]
    fn worktree_start_requires_clean_files_as_well_as_the_approved_commit() {
        let (_dir, project, manager, sha) = fixture();
        let owned = manager
            .allocate(&project, Uuid::new_v4(), "backend", &sha, true)
            .unwrap();
        manager.verify_start(&project, &owned).unwrap();
        fs::write(owned.path().join("local.txt"), "preserve this").unwrap();
        assert!(manager.verify_start(&project, &owned).is_err());
        assert_eq!(
            fs::read_to_string(owned.path().join("local.txt")).unwrap(),
            "preserve this"
        );
    }

    #[test]
    fn never_removes_changed_or_untracked_files_without_force() {
        let (_dir, project, manager, sha) = fixture();
        let owned = manager
            .allocate(&project, Uuid::new_v4(), "frontend", &sha, true)
            .unwrap();
        fs::write(owned.path().join("untracked.txt"), "keep").unwrap();
        assert!(manager.remove_if_clean(&project, &owned, true).is_err());
        assert_eq!(
            fs::read_to_string(owned.path().join("untracked.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn reattach_accepts_only_exact_owned_paths_and_existing_ancestry() {
        let (_dir, project, manager, sha) = fixture();
        let run = Uuid::new_v4();
        let owned = manager
            .allocate(&project, run, "frontend", &sha, true)
            .unwrap();
        let recovered = manager.reattach(&project, run, "frontend", &sha).unwrap();
        assert_eq!(recovered.path(), owned.path());
        assert_eq!(recovered.start_commit(), sha);
        assert!(manager.reattach(&project, run, "other-task", &sha).is_err());
        assert!(
            manager
                .reattach(&project, run, "../frontend", &sha)
                .is_err()
        );
        assert!(
            manager
                .reattach(&project, Uuid::nil(), "frontend", &sha)
                .is_err()
        );
        assert!(
            manager
                .reattach(&project, run, "frontend", &"0".repeat(40))
                .is_err()
        );
        assert!(
            Command::new("git")
                .current_dir(&project.root)
                .args([
                    "-c",
                    "user.name=X test",
                    "-c",
                    "user.email=x@invalid.example",
                    "-c",
                    "core.hooksPath=",
                    "commit",
                    "--allow-empty",
                    "-qm",
                    "new primary commit",
                ])
                .status()
                .unwrap()
                .success()
        );
        let newer_sha = git(&project.root, &["rev-parse", "HEAD"]).unwrap();
        assert!(
            manager
                .reattach(&project, run, "frontend", newer_sha.trim())
                .is_err()
        );
    }

    #[test]
    fn symlinked_worktree_base_is_refused() {
        use std::os::unix::fs::symlink;

        let (dir, project, manager, sha) = fixture();
        let redirected = dir.path().join("redirected");
        fs::create_dir(&redirected).unwrap();
        let alias = manager.home.join("redirected-worktrees");
        symlink(&redirected, &alias).unwrap();
        let unsafe_manager = Manager::new(alias, manager.home.clone());
        assert!(
            unsafe_manager
                .allocate(&project, Uuid::new_v4(), "backend", &sha, true)
                .is_err()
        );
        assert!(fs::read_dir(&redirected).unwrap().next().is_none());
    }

    #[test]
    fn task_ids_and_cleanup_ownership_are_rechecked() {
        let (_dir, project, manager, sha) = fixture();
        let run_id = Uuid::new_v4();
        let owned = manager
            .allocate(&project, run_id, "backend", &sha, true)
            .unwrap();
        assert!(
            manager
                .allocate(&project, run_id, "backend", &sha, true)
                .is_err()
        );
        let mut different = project.clone();
        different.id = Uuid::new_v4();
        assert!(manager.remove_if_clean(&different, &owned, true).is_err());
        assert!(owned.path().exists());
    }

    #[test]
    fn explicit_approval_and_clean_tree_are_required_for_cleanup() {
        let (_dir, project, manager, sha) = fixture();
        assert!(
            manager
                .allocate(&project, Uuid::new_v4(), "api", &sha, false)
                .is_err()
        );
        let owned = manager
            .allocate(&project, Uuid::new_v4(), "api", &sha, true)
            .unwrap();
        assert!(manager.remove_if_clean(&project, &owned, false).is_err());
        manager.remove_if_clean(&project, &owned, true).unwrap();
        assert!(!owned.path().exists());
        assert!(project.root.exists());
    }
}

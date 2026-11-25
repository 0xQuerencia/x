//! Explicitly owned, local Git worktrees for isolated coding jobs.
//! No automatic merge, force-removal, network operation or user-tree mutation.

use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use uuid::Uuid;

use crate::cancellation::Cancellation;
use crate::projects::Project;

mod inspection;
pub use inspection::{Inspection, Reservation, WorktreeReview};

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

/// An observed immutable commit produced in an exact owned checkout.
/// This records Git evidence, not test success or permission to integrate.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommittedArtifact {
    project_id: Uuid,
    run_id: Uuid,
    node_id: String,
    base_commit: String,
    commit: String,
}

impl CommittedArtifact {
    pub fn commit(&self) -> &str {
        &self.commit
    }

    pub(crate) fn valid_for(&self, run_id: Uuid, node_id: &str) -> bool {
        !self.project_id.is_nil()
            && !run_id.is_nil()
            && self.run_id == run_id
            && self.node_id == node_id
            && valid_slug(node_id)
            && valid_commit(&self.base_commit)
            && valid_commit(&self.commit)
            && self.base_commit != self.commit
    }
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
        self.verify_unchanged(project, worktree)?;
        let status = git(
            worktree.path(),
            &[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignored",
            ],
        )?;
        if !status.trim().is_empty() {
            return Err("The task worktree is not clean at its approved starting commit".into());
        }
        Ok(())
    }

    /// Tests may create ignored build outputs, but may not change the tested
    /// commit, index, tracked files or nonignored input files.
    pub(crate) fn verify_unchanged(
        &self,
        project: &Project,
        worktree: &OwnedWorktree,
    ) -> Result<(), String> {
        let path = self.validate_owned(project, worktree)?;
        if current_commit(&path)? != worktree.start_commit() {
            return Err("The task worktree changed its approved starting commit".into());
        }
        ensure_clean(&path)?;
        Ok(())
    }

    /// Validate a planned repository and immutable base without creating a checkout.
    pub(crate) fn validate_base(&self, project: &Project, commit: &str) -> Result<(), String> {
        self.verify_project(project)?;
        if !valid_commit(commit)
            || git(
                &project.root,
                &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
            )?
            .trim()
                != commit
        {
            return Err("The approved base is not an exact project commit".into());
        }
        Ok(())
    }

    /// Resolve only an independently observed output from this exact run/project.
    /// Persisted JSON is historical evidence, never authority to select a checkout.
    pub(crate) fn validate_artifact(
        &self,
        project: &Project,
        run: Uuid,
        artifact: &CommittedArtifact,
    ) -> Result<(), String> {
        if artifact.project_id != project.id || !artifact.valid_for(run, &artifact.node_id) {
            return Err("Artifact belongs to a different project or run".into());
        }
        let source = self.reattach(project, run, &artifact.node_id, &artifact.base_commit)?;
        if self.committed_artifact(project, &source)?.as_ref() != Some(artifact) {
            return Err("Artifact source is dirty, stale or no longer matches its commit".into());
        }
        Ok(())
    }

    /// The reviewed graph authorizes these exact producer edges. Allocate only
    /// when ready, then merge their immutable commits in declared order. Conflicts
    /// remain in this owned checkout for inspection; the primary tree is untouched.
    pub(crate) fn allocate_with_inputs(
        &self,
        project: &Project,
        run: Uuid,
        node: &str,
        base: &str,
        inputs: &[CommittedArtifact],
        stop: &Cancellation,
    ) -> Result<OwnedWorktree, String> {
        stop.check()?;
        self.validate_base(project, base)?;
        for input in inputs {
            self.validate_artifact(project, run, input)?;
            git(
                &project.root,
                &["merge-base", "--is-ancestor", base, input.commit()],
            )?;
        }
        let mut owned = self.allocate(
            project,
            run,
            node,
            inputs.first().map_or(base, CommittedArtifact::commit),
            true,
        )?;
        for input in inputs.iter().skip(1) {
            stop.check()?;
            self.validate_owned(project, &owned)?;
            ensure_clean(owned.path())?;
            git(owned.path(), &[
                "-c", "user.name=X artifact assembly", "-c", "user.email=x@localhost",
                "-c", "commit.gpgSign=false", "-c", "merge.autoStash=false",
                "merge", "--no-edit", "--no-gpg-sign", "--no-verify", input.commit(),
            ]).map_err(|_| "Artifact inputs conflict or could not be merged; the isolated checkout is retained")?;
        }
        stop.check()?;
        for input in inputs {
            self.validate_artifact(project, run, input)?;
        }
        ensure_clean(owned.path())?;
        owned.start_commit = current_commit(owned.path())?;
        self.verify_start(project, &owned)?;
        Ok(owned)
    }

    /// Record the tested/assembled checkpoint against the original reviewed
    /// project base, rather than pretending the tester authored new changes.
    pub(crate) fn checkpoint_artifact(
        &self,
        project: &Project,
        worktree: &OwnedWorktree,
        base: &str,
    ) -> Result<Option<CommittedArtifact>, String> {
        self.verify_unchanged(project, worktree)?;
        let mut observed = worktree.clone();
        observed.start_commit = base.to_owned();
        self.committed_artifact(project, &observed)
    }

    /// Independently inspect the final checkout relative to the reviewed base.
    /// Includes tracked/committed changes and non-ignored untracked paths.
    pub(crate) fn changed_paths(
        &self,
        project: &Project,
        worktree: &OwnedWorktree,
    ) -> Result<Vec<String>, String> {
        let path = self.validate_owned(project, worktree)?;
        git(
            &path,
            &[
                "merge-base",
                "--is-ancestor",
                worktree.start_commit(),
                "HEAD",
            ],
        )?;
        let tracked = git(
            &path,
            &[
                "diff",
                "--name-only",
                "-z",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                worktree.start_commit(),
                "--",
            ],
        )?;
        let untracked = git(&path, &["ls-files", "--others", "--exclude-standard", "-z"])?;
        let mut changes = tracked
            .split('\0')
            .chain(untracked.split('\0'))
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        changes.sort_unstable();
        changes.dedup();
        self.validate_owned(project, worktree)?;
        Ok(changes)
    }

    /// Inspect a committed output independently of provider prose. Uncommitted
    /// changes are never an artifact; ignored build files are not transferred.
    pub(crate) fn committed_artifact(
        &self,
        project: &Project,
        worktree: &OwnedWorktree,
    ) -> Result<Option<CommittedArtifact>, String> {
        let path = self.validate_owned(project, worktree)?;
        let commit = current_commit(&path)?;
        if commit == worktree.start_commit() {
            return Ok(None);
        }
        git(
            &path,
            &[
                "merge-base",
                "--is-ancestor",
                worktree.start_commit(),
                &commit,
            ],
        )?;
        let base_tree = git(
            &path,
            &[
                "rev-parse",
                "--verify",
                &format!("{}^{{tree}}", worktree.start_commit()),
            ],
        )?;
        let final_tree = git(
            &path,
            &["rev-parse", "--verify", &format!("{commit}^{{tree}}")],
        )?;
        if !valid_commit(base_tree.trim()) || !valid_commit(final_tree.trim()) {
            return Err("Git did not return valid artifact tree identities".into());
        }
        if base_tree.trim() == final_tree.trim() {
            return Ok(None);
        }
        let status = git(
            &path,
            &[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
        )?;
        if !status.is_empty() {
            return Ok(None);
        }
        self.validate_owned(project, worktree)?;
        if current_commit(&path)? != commit {
            return Err("The artifact HEAD changed during verification".into());
        }
        Ok(Some(CommittedArtifact {
            project_id: project.id,
            run_id: worktree.run_id(),
            node_id: worktree.task_id().to_owned(),
            base_commit: worktree.start_commit().to_owned(),
            commit,
        }))
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

pub(crate) fn valid_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|c| c.is_ascii_hexdigit())
}

fn ensure_clean(path: &Path) -> Result<(), String> {
    if !git(
        path,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?
    .is_empty()
    {
        return Err("The owned checkout has uncommitted or untracked changes".into());
    }
    Ok(())
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_NAMESPACE")
        .args([
            "--no-replace-objects",
            "-c",
            "core.hooksPath=",
            "-c",
            "core.fsmonitor=false",
        ])
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
mod tests;

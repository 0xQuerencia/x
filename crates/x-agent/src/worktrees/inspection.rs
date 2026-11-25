//! Historical preparation records and independent, non-mutating checkout inspection.

use serde::{Deserialize, Serialize};

use super::*;

/// A recorded intention to create one generated checkout. This contains no
/// provider data and cannot authorize execution, integration or deletion.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reservation {
    project_id: Uuid,
    project_root: PathBuf,
    git_common_dir: PathBuf,
    approved_base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolved_commit: Option<String>,
}

impl Reservation {
    pub(crate) fn planned(project: &Project, base: &str) -> Result<Self, String> {
        let reservation = Self {
            project_id: project.id,
            project_root: project.root.clone(),
            git_common_dir: project
                .git_common_dir
                .clone()
                .ok_or("The approved project has no Git identity")?,
            approved_base: base.into(),
            resolved_commit: None,
        };
        if !reservation.valid() {
            return Err("Invalid worktree preparation identity".into());
        }
        Ok(reservation)
    }

    pub fn project_id(&self) -> Uuid {
        self.project_id
    }

    pub fn resolved_commit(&self) -> Option<&str> {
        self.resolved_commit.as_deref()
    }

    pub(crate) fn valid(&self) -> bool {
        !self.project_id.is_nil()
            && self.project_root.is_absolute()
            && self.git_common_dir.is_absolute()
            && valid_commit(&self.approved_base)
            && self.resolved_commit.as_deref().is_none_or(valid_commit)
    }

    pub(crate) fn same_plan(&self, other: &Self) -> bool {
        self.project_id == other.project_id
            && self.project_root == other.project_root
            && self.git_common_dir == other.git_common_dir
            && self.approved_base == other.approved_base
    }

    pub(crate) fn record_checkout(&mut self, tree: &OwnedWorktree) -> Result<bool, String> {
        if self.project_id != tree.project_id
            || !valid_commit(tree.start_commit())
            || self
                .resolved_commit()
                .is_some_and(|recorded| recorded != tree.start_commit())
        {
            return Err("The prepared checkout changed its recorded identity".into());
        }
        if self.resolved_commit.is_some() {
            return Ok(false);
        }
        self.resolved_commit = Some(tree.start_commit().into());
        Ok(true)
    }

    pub(crate) fn reset_preparation(&mut self) {
        self.resolved_commit = None;
    }
}

/// Current filesystem evidence, separate from a claim's historical outcome.
#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Inspection {
    Absent {
        expected_path: PathBuf,
    },
    Owned {
        path: PathBuf,
        current_commit: String,
        clean_for_cleanup: bool,
        /// None means a crash could have interrupted preparation recording.
        prepared_commit_matches: Option<bool>,
    },
}

impl Manager {
    /// Inspect only the generated path for this exact registered project/run/node.
    /// A missing or foreign checkout is never repaired or adopted automatically.
    pub fn inspect_reserved(
        &self,
        project: &Project,
        run: Uuid,
        node: &str,
        reservation: &Reservation,
    ) -> Result<Inspection, String> {
        if !reservation.valid()
            || project.id != reservation.project_id
            || project.root != reservation.project_root
            || project.git_common_dir.as_ref() != Some(&reservation.git_common_dir)
            || run.is_nil()
            || !valid_slug(node)
        {
            return Err("The recorded project or preparation identity changed".into());
        }
        self.validate_base(project, &reservation.approved_base)?;
        let parent = self.validated_base()?.join(project.id.to_string());
        let expected_path = parent.join(format!("{run}-{node}"));
        match fs::symlink_metadata(&parent) {
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(Inspection::Absent { expected_path });
            }
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            _ => return Err("The recorded worktree parent is not a regular folder".into()),
        }
        match fs::symlink_metadata(&expected_path) {
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(Inspection::Absent { expected_path });
            }
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            _ => return Err("The recorded worktree is not a regular folder".into()),
        }
        let owned = self.reattach(project, run, node, &reservation.approved_base)?;
        let entries = git(&project.root, &["worktree", "list", "--porcelain", "-z"])?;
        if !entries.split('\0').any(|entry| {
            entry
                .strip_prefix("worktree ")
                .is_some_and(|path| Path::new(path) == owned.path())
        }) {
            return Err("Git has no registered worktree at the recorded path".into());
        }
        let head = current_commit(owned.path())?;
        let status = git(
            owned.path(),
            &[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignored",
                "--ignore-submodules=none",
            ],
        )?;
        self.validate_owned(project, &owned)?;
        if current_commit(owned.path())? != head {
            return Err("The checkout changed while it was being inspected".into());
        }
        Ok(Inspection::Owned {
            path: owned.path().into(),
            prepared_commit_matches: reservation.resolved_commit().map(|commit| commit == head),
            current_commit: head,
            clean_for_cleanup: status.trim().is_empty(),
        })
    }
}

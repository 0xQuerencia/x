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

    pub(crate) fn approved_base(&self) -> &str {
        &self.approved_base
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

#[derive(Debug, Serialize)]
pub struct ChangedFile {
    pub status: char,
    pub path: String,
}

/// Local read-only review of the exact recorded checkout. The patch covers
/// committed changes only; uncommitted inputs are explicitly distinguished.
#[derive(Debug, Serialize)]
pub struct WorktreeReview {
    pub path: PathBuf,
    pub base_commit: String,
    pub current_commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_artifact: Option<String>,
    pub clean_inputs: bool,
    pub tracked_changes: bool,
    pub untracked_files: bool,
    /// None means the bounded committed file metadata was incomplete.
    pub binary_changes: Option<bool>,
    pub files: Vec<ChangedFile>,
    pub files_truncated: bool,
    pub committed_diff: String,
    pub diff_truncated: bool,
    pub test_evidence: Option<crate::verification::test_runner::TestEvidence>,
    /// None means no test result was recorded. False means it is historical
    /// evidence only, not evidence for the checkout currently being reviewed.
    pub test_matches_checkout: Option<bool>,
}

impl Manager {
    pub fn review_reserved(
        &self,
        project: &Project,
        run: Uuid,
        node: &str,
        reservation: &Reservation,
        test: Option<&crate::verification::test_runner::TestEvidence>,
    ) -> Result<WorktreeReview, String> {
        let Inspection::Owned {
            path,
            current_commit: head,
            ..
        } = self.inspect_reserved(project, run, node, reservation)?
        else {
            return Err("The recorded checkout is not available for review".into());
        };
        let before = review_status(&path)?;
        const MAX_DIFF_BYTES: usize = 24_000;
        let (diff, diff_truncated) = review_diff(
            &path,
            &reservation.approved_base,
            &head,
            "--patch",
            MAX_DIFF_BYTES,
        )?;
        let (files, files_truncated) = review_diff(
            &path,
            &reservation.approved_base,
            &head,
            "--numstat",
            MAX_DIFF_BYTES,
        )?;
        let binary_changes = if files
            .split(|byte| *byte == b'\n')
            .any(|line| line.starts_with(b"-\t-\t"))
        {
            Some(true)
        } else {
            (!files_truncated).then_some(false)
        };
        let (names, names_truncated) = review_diff(
            &path,
            &reservation.approved_base,
            &head,
            "--name-status",
            MAX_DIFF_BYTES,
        )?;
        let (files, files_truncated) = file_changes(&names, names_truncated);
        let after = review_status(&path)?;
        let Inspection::Owned {
            path: observed,
            current_commit,
            ..
        } = self.inspect_reserved(project, run, node, reservation)?
        else {
            return Err("The checkout changed during review; refresh it".into());
        };
        if observed != path || current_commit != head || before != after {
            return Err("The checkout changed during review; refresh it".into());
        }
        let clean_inputs = after.is_empty();
        let entries = || {
            after
                .split(|byte| *byte == 0)
                .filter(|entry| !entry.is_empty())
        };
        let tracked_changes = entries().any(|entry| !entry.starts_with(b"?? "));
        let untracked_files = entries().any(|entry| entry.starts_with(b"?? "));
        Ok(WorktreeReview {
            path,
            base_commit: reservation.approved_base.clone(),
            test_matches_checkout: test.map(|test| {
                clean_inputs && test.valid() && test.passed.is_some() && test.commit == head
            }),
            current_commit: head,
            recorded_artifact: None,
            clean_inputs,
            tracked_changes,
            untracked_files,
            binary_changes,
            files,
            files_truncated,
            committed_diff: patch_text(&diff, diff_truncated),
            diff_truncated,
            test_evidence: test.cloned(),
        })
    }
}

fn review_status(path: &Path) -> Result<Vec<u8>, String> {
    let args = [
        "status",
        "--porcelain=v1",
        "-z",
        "--no-renames",
        "--untracked-files=all",
        "--ignore-submodules=none",
    ];
    let (ok, status) = crate::desktop::capture(&mut git_command(path, &args), TIMEOUT)?;
    if !ok {
        return Err("Git refused to inspect checkout inputs".into());
    }
    Ok(status)
}

fn review_diff(
    path: &Path,
    base: &str,
    head: &str,
    selection: &str,
    cap: usize,
) -> Result<(Vec<u8>, bool), String> {
    let mut args = vec![
        "diff",
        selection,
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--no-renames",
        "--submodule=short",
    ];
    if selection == "--name-status" {
        args.push("-z");
    }
    args.extend([base, head, "--"]);
    crate::desktop::capture_prefix(&mut git_command(path, &args), TIMEOUT, cap)
}

fn file_changes(bytes: &[u8], truncated: bool) -> (Vec<ChangedFile>, bool) {
    let mut fields = bytes.split_inclusive(|byte| *byte == 0);
    let mut changes = Vec::new();
    while let Some(status) = fields.next() {
        let Some(path) = fields.next().and_then(|path| path.strip_suffix(&[0])) else {
            return (changes, true);
        };
        let [status, 0] = status else {
            return (changes, true);
        };
        if changes.len() == 128 {
            return (changes, true);
        }
        let mut label = String::new();
        for character in String::from_utf8_lossy(path).chars() {
            if character.is_control() {
                label.extend(character.escape_default());
            } else {
                label.push(character);
            }
        }
        changes.push(ChangedFile {
            status: char::from(*status),
            path: label,
        });
    }
    (changes, truncated)
}

fn patch_text(bytes: &[u8], truncated: bool) -> String {
    let bytes = match std::str::from_utf8(bytes) {
        Err(error) if truncated && error.error_len().is_none() => &bytes[..error.valid_up_to()],
        _ => bytes,
    };
    String::from_utf8_lossy(bytes).into_owned()
}

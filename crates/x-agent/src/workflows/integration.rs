//! Read-only merge-back previews for approved worktree results.
//! Never checks out files, updates refs, merges, cherry-picks or pushes.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde::Serialize;

use crate::projects::Project;
use crate::worktrees::{Manager, OwnedWorktree};

const TIMEOUT: Duration = Duration::from_secs(12);
const MAX_FILES: usize = 64;
const MAX_PATH_CHARS: usize = 220;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Preview {
    pub primary_head: String,
    pub task_head: String,
    pub clean: bool,
    /// The primary checkout has uncommitted files and is not ready for merge.
    pub primary_dirty: bool,
    pub merge_tree: Option<String>,
    pub changed_files: Vec<String>,
    pub change_list_truncated: bool,
    pub conflict_count: usize,
    pub needs_explicit_merge_approval: bool,
}

/// Computes the potential merge result without changing either checkout. A
/// conflict returns `clean = false` and does not apply or stage anything.
pub fn preview(
    manager: &Manager,
    project: &Project,
    owned: &OwnedWorktree,
) -> Result<Preview, String> {
    let task_root = manager.validate_owned(project, owned)?;
    let primary_head = commit(&project.root)?;
    let task_head = commit(&task_root)?;
    let (primary_ok, primary_status) = git(
        &project.root,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if !primary_ok {
        return Err("Git could not inspect the primary checkout status".into());
    }
    let primary_dirty = !primary_status.trim().is_empty();
    let dirty = git(
        &task_root,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if !dirty.1.trim().is_empty() {
        return Err("The task worktree has uncommitted files. Review and commit them first".into());
    }
    // The proposed change list belongs to the task branch, not to changes
    // made independently by the owner since this worktree was created.
    let (base_ok, merge_base) = git(&project.root, &["merge-base", &primary_head, &task_head])?;
    let merge_base = merge_base.trim();
    if !base_ok || !is_commit(merge_base) {
        return Err("The task and primary checkout have no verified shared base".into());
    }
    let (diff_ok, output) = git(
        &project.root,
        &[
            "diff",
            "--no-ext-diff",
            "--name-only",
            merge_base,
            &task_head,
            "--",
        ],
    )?;
    if !diff_ok {
        return Err("Git could not list the proposed changes".into());
    }
    let mut changed_files = Vec::new();
    let mut change_list_truncated = false;
    for path in output.lines().filter(|path| !path.is_empty()) {
        if path.chars().any(char::is_control) {
            return Err("Git diff returned unsafe filename metadata".into());
        }
        if changed_files.len() == MAX_FILES || path.chars().count() > MAX_PATH_CHARS {
            change_list_truncated = true;
        } else {
            changed_files.push(path.to_string());
        }
    }
    let (success, merge) = git(
        &project.root,
        &["merge-tree", "--write-tree", &primary_head, &task_head],
    )?;
    let result_tree = merge
        .lines()
        .next()
        .map(str::trim)
        .filter(|text| is_commit(text))
        .map(ToString::to_string);
    let conflict_count = merge
        .lines()
        .filter(|line| line.starts_with("CONFLICT ("))
        .count();
    if !success && conflict_count == 0 {
        return Err("Git could not compute a trusted merge preview".into());
    }
    if success && result_tree.is_none() {
        return Err("Git did not return a merge-result tree".into());
    }
    Ok(Preview {
        primary_head,
        task_head,
        clean: success,
        primary_dirty,
        merge_tree: success.then_some(result_tree).flatten(),
        changed_files,
        change_list_truncated,
        conflict_count,
        needs_explicit_merge_approval: true,
    })
}

/// Refuse diverged histories. An approved integration may advance only the
/// owner's clean checked-out branch by a Git fast-forward. No force-push,
/// conflict resolution, reset, arbitrary file copies or implicit merge commit.
pub fn eligible_fast_forward(project: &Project, proposed: &Preview) -> Result<bool, String> {
    if !proposed.clean
        || proposed.primary_dirty
        || proposed.primary_head == proposed.task_head
        || proposed.changed_files.is_empty()
        || proposed.change_list_truncated
    {
        return Ok(false);
    }
    let (ancestor, _) = git(
        &project.root,
        &[
            "merge-base",
            "--is-ancestor",
            &proposed.primary_head,
            &proposed.task_head,
        ],
    )?;
    Ok(ancestor)
}

/// The external approval gate must have captured `expected` from a
/// deterministic preview immediately before owner approval.
pub fn apply_fast_forward(
    manager: &Manager,
    project: &Project,
    worktree: &OwnedWorktree,
    expected: &Preview,
    owner_approved: bool,
) -> Result<Preview, String> {
    if !owner_approved {
        return Err("Only the owner can approve merging a task worktree".into());
    }
    let latest = preview(manager, project, worktree)?;
    if latest != *expected {
        return Err("Project or task changes since approval; review a new merge preview".into());
    }
    if !eligible_fast_forward(project, &latest)? {
        return Err("Only a clean, conflict-free fast-forward can be applied by X".into());
    }
    // Git itself checks clean index, worktree and branch reference while
    // updating. Never fall back to --force, reset or automatic conflict fixes.
    let (success, _) = git(
        &project.root,
        &["merge", "--ff-only", "--no-edit", "--", &latest.task_head],
    )?;
    if !success {
        return Err("Git refused the fast-forward; inspect the primary checkout".into());
    }
    let actual = commit(&project.root)?;
    if actual != latest.task_head {
        return Err("Git merge returned without the approved commit; verify manually".into());
    }
    Ok(latest)
}

fn commit(directory: &Path) -> Result<String, String> {
    let (success, output) = git(directory, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let value = output.trim();
    if !success || !is_commit(value) {
        return Err("The selected Git checkout has no verified HEAD commit".into());
    }
    Ok(value.to_string())
}

fn is_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|c| c.is_ascii_hexdigit())
}

fn git(root: &Path, args: &[&str]) -> Result<(bool, String), String> {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .args(["-c", "core.hooksPath=", "-c", "core.fsmonitor=false"])
        .args(args);
    let (success, output) =
        crate::desktop::capture(&mut command, TIMEOUT).map_err(|_| "Git preview failed")?;
    let output = String::from_utf8(output).map_err(|_| "Git preview returned invalid UTF-8")?;
    Ok((success, output))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::worktrees::Manager;
    use std::fs;
    use uuid::Uuid;

    fn command(root: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .current_dir(root)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }

    fn fixture() -> (tempfile::TempDir, Project, Manager, OwnedWorktree) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir(&home).unwrap();
        let source = home.join("primary");
        fs::create_dir(&source).unwrap();
        command(&source, &["init", "-q"]);
        fs::write(source.join("shared.txt"), "baseline\n").unwrap();
        command(&source, &["add", "shared.txt"]);
        command(
            &source,
            &[
                "-c",
                "user.name=X",
                "-c",
                "user.email=x@invalid.example",
                "-c",
                "core.hooksPath=",
                "commit",
                "-q",
                "-m",
                "baseline",
            ],
        );
        let source = source.canonicalize().unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "project".into(),
            aliases: vec![],
            root: source.clone(),
            git_common_dir: Some(source.join(".git").canonicalize().unwrap()),
        };
        let base = home.join("Worktrees");
        fs::create_dir(&base).unwrap();
        let manager = Manager::new(base, home);
        let rev = commit(&source).unwrap();
        let worktree = manager
            .allocate(&project, Uuid::new_v4(), "backend", &rev, true)
            .unwrap();
        (dir, project, manager, worktree)
    }

    fn commit_worktree(path: &Path, message: &str) {
        command(path, &["add", "-A"]);
        command(
            path,
            &[
                "-c",
                "user.name=X",
                "-c",
                "user.email=x@invalid.example",
                "-c",
                "core.hooksPath=",
                "commit",
                "-q",
                "-m",
                message,
            ],
        );
    }

    #[test]
    fn explicit_fast_forward_advances_only_a_clean_primary_checkout() {
        let (_dir, project, manager, task) = fixture();
        fs::write(task.path().join("feature.txt"), "added by X\n").unwrap();
        commit_worktree(task.path(), "feature");
        let review = preview(&manager, &project, &task).unwrap();
        assert!(eligible_fast_forward(&project, &review).unwrap());
        let original = commit(&project.root).unwrap();
        assert!(apply_fast_forward(&manager, &project, &task, &review, false).is_err());
        assert_eq!(commit(&project.root).unwrap(), original);
        apply_fast_forward(&manager, &project, &task, &review, true).unwrap();
        assert_eq!(commit(&project.root).unwrap(), review.task_head);
        assert_eq!(
            fs::read_to_string(project.root.join("feature.txt")).unwrap(),
            "added by X\n"
        );
    }

    #[test]
    fn stale_or_dirty_merge_plans_never_modify_the_primary_checkout() {
        let (_dir, project, manager, task) = fixture();
        fs::write(task.path().join("feature.txt"), "worktree commit\n").unwrap();
        commit_worktree(task.path(), "feature");
        let review = preview(&manager, &project, &task).unwrap();
        let original = commit(&project.root).unwrap();
        fs::write(project.root.join("owner-notes.txt"), "unsaved notes\n").unwrap();
        assert!(apply_fast_forward(&manager, &project, &task, &review, true).is_err());
        assert_eq!(commit(&project.root).unwrap(), original);
        assert!(project.root.join("owner-notes.txt").exists());
        fs::remove_file(project.root.join("owner-notes.txt")).unwrap();
        fs::write(task.path().join("feature.txt"), "changed task\n").unwrap();
        commit_worktree(task.path(), "changed");
        assert!(apply_fast_forward(&manager, &project, &task, &review, true).is_err());
        assert_eq!(commit(&project.root).unwrap(), original);
    }

    #[test]
    fn diverged_primary_is_never_automatically_merged() {
        let (_dir, project, manager, task) = fixture();
        fs::write(task.path().join("task.txt"), "task\n").unwrap();
        commit_worktree(task.path(), "task");
        fs::write(project.root.join("owner.txt"), "owner\n").unwrap();
        commit_worktree(&project.root, "owner");
        let before = commit(&project.root).unwrap();
        let review = preview(&manager, &project, &task).unwrap();
        assert!(review.clean);
        assert!(!eligible_fast_forward(&project, &review).unwrap());
        assert!(apply_fast_forward(&manager, &project, &task, &review, true).is_err());
        assert_eq!(commit(&project.root).unwrap(), before);
        assert_eq!(
            fs::read_to_string(project.root.join("owner.txt")).unwrap(),
            "owner\n"
        );
    }

    #[test]
    fn clean_preview_does_not_touch_dirty_primary_checkout() {
        let (_dir, project, manager, task) = fixture();
        fs::write(task.path().join("backend.rs"), "new code\n").unwrap();
        commit_worktree(task.path(), "backend edit");
        fs::write(project.root.join("local_notes.txt"), "do not delete\n").unwrap();
        let before = commit(&project.root).unwrap();
        let result = preview(&manager, &project, &task).unwrap();
        assert!(result.clean);
        assert!(result.primary_dirty);
        assert!(result.merge_tree.is_some());
        assert!(result.changed_files.contains(&"backend.rs".into()));
        assert!(result.needs_explicit_merge_approval);
        assert_eq!(commit(&project.root).unwrap(), before);
        assert_eq!(
            fs::read_to_string(project.root.join("local_notes.txt")).unwrap(),
            "do not delete\n"
        );
    }

    #[test]
    fn conflicting_commits_do_not_modify_primary_checkout() {
        let (_dir, project, manager, task) = fixture();
        fs::write(task.path().join("shared.txt"), "task content\n").unwrap();
        commit_worktree(task.path(), "task edit");
        fs::write(project.root.join("shared.txt"), "owner content\n").unwrap();
        commit_worktree(&project.root, "owner edit");
        let head = commit(&project.root).unwrap();
        let result = preview(&manager, &project, &task).unwrap();
        assert!(!result.clean);
        assert!(result.conflict_count >= 1);
        assert!(result.merge_tree.is_none());
        assert_eq!(commit(&project.root).unwrap(), head);
        assert_eq!(
            fs::read_to_string(project.root.join("shared.txt")).unwrap(),
            "owner content\n"
        );
    }

    #[test]
    fn changed_files_list_only_agent_branch_changes() {
        let (_dir, project, manager, task) = fixture();
        fs::write(task.path().join("task_only.rs"), "agent work\n").unwrap();
        commit_worktree(task.path(), "task edit");
        fs::write(project.root.join("owner_only.rs"), "owner work\n").unwrap();
        commit_worktree(&project.root, "owner edit");
        let result = preview(&manager, &project, &task).unwrap();
        assert!(result.clean);
        assert!(result.changed_files.contains(&"task_only.rs".into()));
        assert!(!result.changed_files.contains(&"owner_only.rs".into()));
    }

    #[test]
    fn uncommitted_task_changes_are_not_silently_omitted() {
        let (_dir, project, manager, task) = fixture();
        fs::write(task.path().join("unsaved.rs"), "uncommitted\n").unwrap();
        let error = preview(&manager, &project, &task).unwrap_err();
        assert!(error.contains("uncommitted"));
        assert!(task.path().join("unsaved.rs").exists());
    }
}

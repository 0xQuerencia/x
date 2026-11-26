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
fn artifact_requires_a_changed_clean_descendant_commit() {
    let (_dir, project, manager, sha) = fixture();
    let owned = manager
        .allocate(&project, Uuid::new_v4(), "backend", &sha, true)
        .unwrap();
    assert!(
        manager
            .committed_artifact(&project, &owned)
            .unwrap()
            .is_none()
    );
    git(
        owned.path(),
        &[
            "-c",
            "user.name=X",
            "-c",
            "user.email=x@invalid.test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "empty",
        ],
    )
    .unwrap();
    assert!(
        manager
            .committed_artifact(&project, &owned)
            .unwrap()
            .is_none()
    );
    fs::write(owned.path().join("artifact.txt"), "implementation").unwrap();
    assert!(
        manager
            .committed_artifact(&project, &owned)
            .unwrap()
            .is_none()
    );
    git(owned.path(), &["add", "--", "artifact.txt"]).unwrap();
    git(
        owned.path(),
        &[
            "-c",
            "user.name=X",
            "-c",
            "user.email=x@invalid.test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "artifact",
        ],
    )
    .unwrap();
    let artifact = manager
        .committed_artifact(&project, &owned)
        .unwrap()
        .unwrap();
    assert_eq!(artifact.commit(), current_commit(owned.path()).unwrap());
    assert!(artifact.valid_for(owned.run_id(), owned.task_id()));
    fs::write(owned.path().join("partial.txt"), "not committed").unwrap();
    assert!(
        manager
            .committed_artifact(&project, &owned)
            .unwrap()
            .is_none()
    );
}

#[test]
fn changed_paths_include_real_commits_and_untracked_files() {
    let (_dir, project, manager, sha) = fixture();
    let owned = manager
        .allocate(&project, Uuid::new_v4(), "backend", &sha, true)
        .unwrap();
    assert!(manager.changed_paths(&project, &owned).unwrap().is_empty());
    fs::write(owned.path().join("committed.txt"), "first").unwrap();
    git(owned.path(), &["add", "--", "committed.txt"]).unwrap();
    git(
        owned.path(),
        &[
            "-c",
            "user.name=X",
            "-c",
            "user.email=x@invalid.test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "fixture",
        ],
    )
    .unwrap();
    fs::write(owned.path().join("untracked.txt"), "second").unwrap();
    assert_eq!(
        manager.changed_paths(&project, &owned).unwrap(),
        vec!["committed.txt".to_string(), "untracked.txt".to_string()]
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

fn committed_file(
    manager: &Manager,
    project: &Project,
    run: Uuid,
    id: &str,
    base: &str,
    name: &str,
    text: &str,
) -> CommittedArtifact {
    let owned = manager.allocate(project, run, id, base, true).unwrap();
    fs::write(owned.path().join(name), text).unwrap();
    git(owned.path(), &["add", "--", name]).unwrap();
    git(
        owned.path(),
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=x@localhost",
            "-c",
            "commit.gpgSign=false",
            "commit",
            "-m",
            id,
        ],
    )
    .unwrap();
    manager
        .committed_artifact(project, &owned)
        .unwrap()
        .unwrap()
}

#[test]
fn artifact_inputs_assemble_actual_independent_commits_without_touching_primary() {
    let (_dir, project, manager, base) = fixture();
    let run = Uuid::new_v4();
    let front = committed_file(
        &manager,
        &project,
        run,
        "front",
        &base,
        "front.txt",
        "frontend",
    );
    let back = committed_file(
        &manager, &project, run, "back", &base, "back.txt", "backend",
    );
    fs::write(project.root.join("local.txt"), "owner edits").unwrap();
    let before = git(&project.root, &["status", "--porcelain=v1"]).unwrap();
    let combined = manager
        .allocate_with_inputs(
            &project,
            run,
            "test",
            &base,
            &[front.clone(), back.clone()],
            &Cancellation::default(),
        )
        .unwrap();
    assert_eq!(
        fs::read_to_string(combined.path().join("front.txt")).unwrap(),
        "frontend"
    );
    assert_eq!(
        fs::read_to_string(combined.path().join("back.txt")).unwrap(),
        "backend"
    );
    assert_eq!(current_commit(&project.root).unwrap(), base);
    assert_eq!(
        git(&project.root, &["status", "--porcelain=v1"]).unwrap(),
        before
    );
    for input in [&front, &back] {
        git(
            combined.path(),
            &["merge-base", "--is-ancestor", input.commit(), "HEAD"],
        )
        .unwrap();
        manager.validate_artifact(&project, run, input).unwrap();
    }
    let proof = manager
        .checkpoint_artifact(&project, &combined, &base)
        .unwrap()
        .unwrap();
    assert_eq!(proof.commit(), combined.start_commit());
    let downstream = manager
        .allocate_with_inputs(
            &project,
            run,
            "review",
            &base,
            &[proof],
            &Cancellation::default(),
        )
        .unwrap();
    assert_eq!(downstream.start_commit(), combined.start_commit());
}

#[test]
fn dirty_foreign_conflicting_or_cancelled_inputs_never_modify_primary() {
    let (_dir, project, manager, base) = fixture();
    let run = Uuid::new_v4();
    let one = committed_file(&manager, &project, run, "one", &base, "same.txt", "one\n");
    let two = committed_file(&manager, &project, run, "two", &base, "same.txt", "two\n");
    let stopped = Cancellation::default();
    stopped.request();
    assert!(
        manager
            .allocate_with_inputs(
                &project,
                run,
                "cancelled",
                &base,
                std::slice::from_ref(&one),
                &stopped
            )
            .is_err()
    );
    assert!(
        manager
            .allocate_with_inputs(
                &project,
                Uuid::new_v4(),
                "foreign",
                &base,
                std::slice::from_ref(&one),
                &Cancellation::default()
            )
            .is_err()
    );
    let error = manager
        .allocate_with_inputs(
            &project,
            run,
            "conflict",
            &base,
            &[one.clone(), two.clone()],
            &Cancellation::default(),
        )
        .unwrap_err();
    assert!(error.contains("conflict"));
    let reservation = Reservation::planned(&project, &base).unwrap();
    assert!(matches!(
        manager
            .inspect_reserved(&project, run, "conflict", &reservation)
            .unwrap(),
        Inspection::Owned {
            clean_for_cleanup: false,
            prepared_commit_matches: None,
            ..
        }
    ));
    assert!(
        manager
            .base
            .join(project.id.to_string())
            .join(format!("{run}-conflict"))
            .is_dir()
    );
    let source = manager.reattach(&project, run, "one", &base).unwrap();
    fs::write(source.path().join("private.txt"), "do not transfer").unwrap();
    assert!(
        manager
            .allocate_with_inputs(
                &project,
                run,
                "dirty",
                &base,
                &[one],
                &Cancellation::default()
            )
            .is_err()
    );
    for node in ["cancelled", "dirty"] {
        assert!(
            !manager
                .base
                .join(project.id.to_string())
                .join(format!("{run}-{node}"))
                .exists()
        );
    }
    assert_eq!(current_commit(&project.root).unwrap(), base);
    assert!(!project.root.join("same.txt").exists());
    assert_eq!(
        fs::read_to_string(source.path().join("private.txt")).unwrap(),
        "do not transfer"
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
fn recovery_inventory_never_treats_ignored_files_as_clean_for_cleanup() {
    let (_dir, project, manager, sha) = fixture();
    let run = Uuid::new_v4();
    let reservation = Reservation::planned(&project, &sha).unwrap();
    assert!(matches!(
        manager
            .inspect_reserved(&project, run, "job", &reservation)
            .unwrap(),
        Inspection::Absent { .. }
    ));
    let owned = manager.allocate(&project, run, "job", &sha, true).unwrap();
    assert!(matches!(
        manager
            .inspect_reserved(&project, run, "job", &reservation)
            .unwrap(),
        Inspection::Owned {
            clean_for_cleanup: true,
            ..
        }
    ));
    fs::write(project.root.join(".git/info/exclude"), "cache/\n").unwrap();
    fs::create_dir(owned.path().join("cache")).unwrap();
    fs::write(owned.path().join("cache/result"), "valuable ignored output").unwrap();
    assert!(matches!(
        manager
            .inspect_reserved(&project, run, "job", &reservation)
            .unwrap(),
        Inspection::Owned {
            clean_for_cleanup: false,
            ..
        }
    ));
    assert!(manager.remove_if_clean(&project, &owned, true).is_err());
    assert_eq!(
        fs::read_to_string(owned.path().join("cache/result")).unwrap(),
        "valuable ignored output"
    );
}

#[test]
fn recovery_inventory_refuses_symlinked_foreign_and_reregistered_checkouts() {
    use std::os::unix::fs::symlink;

    let (_dir, project, manager, sha) = fixture();
    let run = Uuid::new_v4();
    let reservation = Reservation::planned(&project, &sha).unwrap();
    let owned = manager.allocate(&project, run, "job", &sha, true).unwrap();
    let mut changed = project.clone();
    changed.root = manager.home.join("different-project");
    assert!(
        manager
            .inspect_reserved(&changed, run, "job", &reservation)
            .is_err()
    );

    let kept = owned.path().with_file_name("kept");
    fs::rename(owned.path(), &kept).unwrap();
    symlink(&kept, owned.path()).unwrap();
    assert!(
        manager
            .inspect_reserved(&project, run, "job", &reservation)
            .is_err()
    );
    assert!(kept.is_dir());

    let foreign = manager
        .base
        .join(project.id.to_string())
        .join(format!("{run}-foreign"));
    fs::create_dir(&foreign).unwrap();
    fs::write(
        foreign.join(".git"),
        format!(
            "gitdir: {}\n",
            project.git_common_dir.as_ref().unwrap().display()
        ),
    )
    .unwrap();
    fs::write(foreign.join("owner.txt"), "unrelated directory").unwrap();
    assert!(
        manager
            .inspect_reserved(&project, run, "foreign", &reservation)
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(foreign.join("owner.txt")).unwrap(),
        "unrelated directory"
    );
}

#[test]
fn worktree_review_reads_exact_committed_changes_and_distinguishes_stale_tests() {
    use crate::verification::test_runner::{Suite, TestEvidence};

    let (_dir, project, manager, sha) = fixture();
    let run = Uuid::new_v4();
    let mut reservation = Reservation::planned(&project, &sha).unwrap();
    let owned = manager
        .allocate(&project, run, "writer", &sha, true)
        .unwrap();
    reservation.record_checkout(&owned).unwrap();
    fs::write(owned.path().join("review.txt"), "reviewed change\n").unwrap();
    git(owned.path(), &["add", "--", "review.txt"]).unwrap();
    git(
        owned.path(),
        &[
            "-c",
            "user.name=X",
            "-c",
            "user.email=x@invalid.example",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "review fixture",
        ],
    )
    .unwrap();
    let head = current_commit(owned.path()).unwrap();
    let evidence = TestEvidence {
        suite: Suite::CargoAgent,
        commit: head.clone(),
        passed: Some(true),
        observed_at: 1,
    };
    let review = manager
        .review_reserved(&project, run, "writer", &reservation, Some(&evidence))
        .unwrap();
    assert_eq!(review.base_commit, sha);
    assert_eq!(review.current_commit, head);
    assert!(review.committed_diff.contains("+reviewed change"));
    assert_eq!(review.test_matches_checkout, Some(true));
    assert!(!review.diff_truncated);
    assert_eq!(review.binary_changes, Some(false));
    assert!(!review.files_truncated);
    assert_eq!(review.files.len(), 1);
    assert_eq!(review.files[0].status, 'A');
    assert_eq!(review.files[0].path, "review.txt");
    assert!(!review.tracked_changes);
    assert!(!review.untracked_files);
    assert!(!project.root.join("review.txt").exists());

    fs::write(owned.path().join("review.txt"), "uncommitted change\n").unwrap();
    fs::write(
        owned.path().join("untracked.txt"),
        "private untracked content",
    )
    .unwrap();
    let dirty = manager
        .review_reserved(&project, run, "writer", &reservation, Some(&evidence))
        .unwrap();
    assert!(!dirty.clean_inputs);
    assert!(dirty.tracked_changes);
    assert!(dirty.untracked_files);
    assert_eq!(dirty.test_matches_checkout, Some(false));
    assert!(!dirty.committed_diff.contains("uncommitted change"));
    assert!(
        manager
            .review_reserved(&project, Uuid::new_v4(), "writer", &reservation, None)
            .is_err()
    );
}

#[test]
fn worktree_review_caps_real_patch_output_and_identifies_binary_commits() {
    let (_dir, project, manager, sha) = fixture();
    let run = Uuid::new_v4();
    let mut reservation = Reservation::planned(&project, &sha).unwrap();
    let owned = manager
        .allocate(&project, run, "writer", &sha, true)
        .unwrap();
    reservation.record_checkout(&owned).unwrap();

    fs::write(owned.path().join("a.txt"), "🦀 line\n".repeat(650_000)).unwrap();
    fs::write(owned.path().join("b.bin"), [0, 1, 2]).unwrap();
    git(owned.path(), &["add", "--", "a.txt", "b.bin"]).unwrap();
    git(
        owned.path(),
        &[
            "-c",
            "user.name=X",
            "-c",
            "user.email=x@invalid.example",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "large review fixture",
        ],
    )
    .unwrap();
    let review = manager
        .review_reserved(&project, run, "writer", &reservation, None)
        .unwrap();
    assert!(review.diff_truncated);
    assert!(review.clean_inputs);
    assert_eq!(review.binary_changes, Some(true));
    assert_eq!(review.test_matches_checkout, None);
    assert!(review.committed_diff.contains("+🦀 line"));
    assert!(review.committed_diff.len() <= 24_000);
    assert!(!review.committed_diff.contains('�'));
}

#[test]
fn worktree_review_marks_results_stale_after_head_moves_and_refuses_wrong_node() {
    use crate::verification::test_runner::{Suite, TestEvidence};

    let (_dir, project, manager, sha) = fixture();
    let run = Uuid::new_v4();
    let mut reservation = Reservation::planned(&project, &sha).unwrap();
    let owned = manager.allocate(&project, run, "test", &sha, true).unwrap();
    reservation.record_checkout(&owned).unwrap();
    let evidence = TestEvidence {
        suite: Suite::CargoAgent,
        commit: sha,
        passed: Some(false),
        observed_at: 1,
    };
    let review = manager
        .review_reserved(&project, run, "test", &reservation, Some(&evidence))
        .unwrap();
    assert_eq!(review.test_matches_checkout, Some(true));
    assert_eq!(review.test_evidence.as_ref().unwrap().passed, Some(false));

    git(
        owned.path(),
        &[
            "-c",
            "user.name=X",
            "-c",
            "user.email=x@invalid.example",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "different proof target",
        ],
    )
    .unwrap();
    let review = manager
        .review_reserved(&project, run, "test", &reservation, Some(&evidence))
        .unwrap();
    assert_eq!(review.test_matches_checkout, Some(false));
    assert_ne!(review.current_commit, evidence.commit);
    assert!(
        manager
            .review_reserved(&project, run, "other", &reservation, None)
            .is_err()
    );
    let mut changed = project.clone();
    changed.id = Uuid::new_v4();
    assert!(
        manager
            .review_reserved(&changed, run, "test", &reservation, None)
            .is_err()
    );
}

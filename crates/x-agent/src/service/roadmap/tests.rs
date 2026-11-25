use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::cancellation::Cancellation;
use crate::harness::{self, Adapter, Capability, Descriptor};
use crate::hexcli::{Stt, SttError};
use crate::phase::Phase;
use crate::projects::Registry;
use crate::service::Launcher;
use crate::tools::Action;

const MARKDOWN: &str = "# X Roadmap: app-v1\n- [x] existing: Existing settings\n- [ ] ui: Keyboard controls\n  Goal: Add shortcuts\n  Acceptance: Keyboard tests pass\n  Depends: existing\n- [ ] backend: Status endpoint\n  Goal: Report real status\n  Acceptance: State tests pass\n  Depends: none\n- [ ] docs: User guide\n  Goal: Explain actual controls\n  Acceptance: Examples match the implementation\n  Depends: none\n- [ ] followup: Combined feature\n  Goal: Connect the controls and endpoint\n  Acceptance: Integration test passes\n  Depends: ui, backend\n";

struct NoEffects;
impl Stt for NoEffects {
    fn start(&self) -> Result<(), SttError> {
        panic!("roadmap inspection must not record");
    }
    fn stop_print(&self) -> Result<String, SttError> {
        panic!("roadmap inspection must not transcribe");
    }
    fn cancel(&self) -> Result<(), SttError> {
        Ok(())
    }
}
impl Launcher for NoEffects {
    fn launch(&self, _: &Action) -> Result<String, String> {
        panic!("roadmap inspection must not launch");
    }
}
struct Provider(Arc<AtomicUsize>);
impl Adapter for Provider {
    fn descriptor(&self) -> Descriptor {
        Descriptor {
            kind: Kind::OpenCode,
            event_schema: 1,
            version: Some("fixture".into()),
            capabilities: &[Capability::OwnedRun, Capability::WorkspaceWrite],
        }
    }
    fn start(
        &self,
        _: harness::Request,
        _: harness::EventSink,
        _: &Cancellation,
    ) -> Result<Box<dyn harness::OwnedRun>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("fixture refuses provider execution".into())
    }
}
struct Fixture {
    _home: tempfile::TempDir,
    home: PathBuf,
    source: PathBuf,
    service: Service,
    project: Project,
    registry: Registry,
    starts: Arc<AtomicUsize>,
}
impl Fixture {
    fn inspect(&self) -> Proposal {
        self.service
            .inspect_roadmap(&self.project.id.to_string(), &self.source)
            .unwrap()
    }
    fn selection(&self) -> Selection {
        Selection {
            tasks: vec![],
            provider: Kind::OpenCode,
            test_suite: Some(Suite::CargoAgent),
        }
    }
    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(&self.project.root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("app");
    let support = home.path().join("support");
    std::fs::create_dir(&root).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@example.invalid"],
        vec!["config", "user.name", "Fixture"],
        vec!["commit", "--allow-empty", "-qm", "initial"],
    ] {
        let output = Command::new("git")
            .current_dir(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
    }
    let registry = Registry::new(support.clone(), home.path().into());
    let (project, _) = registry.register("App", root.to_str().unwrap()).unwrap();
    let starts = Arc::new(AtomicUsize::new(0));
    let service = Service::with_harness(
        Arc::new(NoEffects),
        Arc::new(NoEffects),
        Some(Arc::new(Provider(starts.clone()))),
    );
    service.configure_session_storage(support.clone(), home.path().into());
    service.set_workflow_dirs(support.join("workflows"), home.path().into());
    service
        .select_registered_project(&project.id.to_string())
        .unwrap();
    let source = home.path().join("roadmap.md");
    std::fs::write(&source, MARKDOWN).unwrap();
    Fixture {
        home: home.path().into(),
        _home: home,
        source,
        service,
        project,
        registry,
        starts,
    }
}

#[test]
fn inspection_and_three_task_proposal_are_inert_and_use_exact_existing_graph_contracts() {
    let f = fixture();
    f.service.set_trust_actions(true);
    let proposal = f.inspect();
    assert_eq!(proposal.next, ["ui", "backend", "docs"]);
    assert!(matches!(
        proposal.entries[0].disposition,
        Disposition::MarkedComplete
    ));
    assert!(matches!(
        proposal.entries[4].disposition,
        Disposition::Blocked { .. }
    ));
    assert!(!proposal.executed && !proposal.permissions_issued);
    assert!(f.service.workflow_store().list_graphs().is_empty());
    assert!(f.service.snapshot(0).task.is_none());
    let task = f.service.plan_roadmap(&proposal, &f.selection()).unwrap();
    assert_eq!(task.phase, Phase::AwaitingApproval);
    assert_eq!(task.steps.len(), 4);
    assert!(
        task.steps[3]
            .title
            .contains("cargo test --locked -p x-agent --all-targets")
    );
    let saved = f.service.workflow_store().list_graphs();
    assert_eq!(saved.len(), 1);
    let graph = f.service.workflow_store().get_graph(&saved[0].id).unwrap();
    let source = graph.roadmap.as_ref().unwrap();
    assert_eq!(source.sha256, proposal.source_sha256);
    assert_eq!(source.base_commit, proposal.base_commit);
    assert_eq!(
        source
            .tasks
            .iter()
            .map(|task| task.task_id.as_str())
            .collect::<Vec<_>>(),
        ["ui", "backend", "docs"]
    );
    assert!(
        graph.nodes[..3]
            .iter()
            .all(|node| node.required_commit && node.after.is_empty() && node.inputs.is_empty())
    );
    assert_eq!(graph.nodes[3].inputs.len(), 3);
    assert_eq!(f.starts.load(Ordering::SeqCst), 0);
    assert!(!f.home.join("X Worktrees").exists());
    assert!(f.service.lock().grants.is_empty());
}

#[test]
fn saved_proposals_dedupe_without_claiming_acceptance_and_delete_allows_reconsideration() {
    let f = fixture();
    let proposal = f.inspect();
    f.service.plan_roadmap(&proposal, &f.selection()).unwrap();
    let graph_id = f.service.workflow_store().list_graphs()[0].id.clone();
    f.service.cancel();
    let next = f.inspect();
    assert!(next.next.is_empty());
    assert!(
        matches!(&next.entries[1].disposition, Disposition::SavedProposal { graph_id: id, last_outcome: None, .. } if id == &graph_id)
    );
    assert!(f.service.plan_roadmap(&proposal, &f.selection()).is_err());
    f.service.workflow_store().delete(&graph_id).unwrap();
    assert_eq!(f.inspect().next, ["ui", "backend", "docs"]);
    assert!(matches!(
        f.inspect().entries[0].disposition,
        Disposition::MarkedComplete
    ));
}

#[test]
fn changed_source_selected_project_and_git_base_invalidate_inspection() {
    let f = fixture();
    let proposal = f.inspect();
    std::fs::write(
        &f.source,
        MARKDOWN.replace("Add shortcuts", "Replace all controls"),
    )
    .unwrap();
    assert!(
        f.service
            .plan_roadmap(&proposal, &f.selection())
            .unwrap_err()
            .contains("changed")
    );
    std::fs::write(&f.source, MARKDOWN).unwrap();
    let proposal = f.inspect();
    f.git(&["commit", "--allow-empty", "-qm", "advanced"]);
    assert!(
        f.service
            .plan_roadmap(&proposal, &f.selection())
            .unwrap_err()
            .contains("commit changed")
    );
    let proposal = f.inspect();
    let other = f.home.join("other");
    std::fs::create_dir(&other).unwrap();
    f.service.set_workspace(other.to_str().unwrap()).unwrap();
    assert!(f.service.plan_roadmap(&proposal, &f.selection()).is_err());
    f.service
        .set_workspace(f.project.root.to_str().unwrap())
        .unwrap();
    f.registry.forget(&f.project.id.to_string()).unwrap();
    assert!(f.service.plan_roadmap(&proposal, &f.selection()).is_err());
    assert_eq!(f.starts.load(Ordering::SeqCst), 0);
    assert!(f.service.workflow_store().list_graphs().is_empty());
}

#[test]
fn malformed_selections_and_forged_metadata_cannot_add_operations() {
    let f = fixture();
    let proposal = f.inspect();
    for tasks in [
        vec!["existing"],
        vec!["followup"],
        vec!["ui", "ui"],
        vec!["ui", "backend", "docs", "followup"],
        vec!["unknown"],
    ] {
        let selection = Selection {
            tasks: tasks.into_iter().map(String::from).collect(),
            ..f.selection()
        };
        assert!(f.service.plan_roadmap(&proposal, &selection).is_err());
    }
    f.service.plan_roadmap(&proposal, &f.selection()).unwrap();
    let entry = &f.service.workflow_store().list_graphs()[0];
    let graph = f.service.workflow_store().get_graph(&entry.id).unwrap();
    let mut wrong = graph.clone();
    wrong.roadmap.as_mut().unwrap().tasks[0].goal_sha256 = "a".repeat(64);
    assert!(wrong.validate().is_err());
    wrong = graph.clone();
    wrong.version = dag::VERSION;
    assert!(wrong.validate().is_err());
    let mut json = serde_json::to_value(&graph).unwrap();
    json["roadmap"]["approved"] = serde_json::json!(true);
    assert!(dag::decode(&json.to_string(), &f.home).is_err());
    let imported = f.home.join("forged-provenance.json");
    std::fs::write(&imported, serde_json::to_vec(&graph).unwrap()).unwrap();
    assert!(
        f.service
            .workflow_store()
            .import_graph(&imported)
            .unwrap_err()
            .contains("explicitly selected Markdown")
    );
    assert_eq!(f.starts.load(Ordering::SeqCst), 0);
}

#[test]
fn changed_source_after_approval_preview_prevents_dispatch() {
    let f = fixture();
    let task = f
        .service
        .plan_roadmap(&f.inspect(), &f.selection())
        .unwrap();
    std::fs::write(
        &f.source,
        MARKDOWN.replace("Add shortcuts", "Replace all controls"),
    )
    .unwrap();
    f.service.approve(task.id).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while f.service.snapshot(0).phase == Phase::Running && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(f.service.snapshot(0).phase, Phase::Failed);
    assert_eq!(f.starts.load(Ordering::SeqCst), 0);
    assert!(!f.home.join("X Worktrees").exists());
}

#[test]
fn unreadable_inventory_does_not_make_existing_tasks_look_new() {
    let f = fixture();
    let store = f.service.workflow_store();
    std::fs::create_dir_all(store.dir()).unwrap();
    std::fs::write(store.dir().join("broken.json"), "{not a workflow}").unwrap();
    assert!(
        f.service
            .inspect_roadmap(&f.project.id.to_string(), &f.source)
            .is_err()
    );
    assert_eq!(f.starts.load(Ordering::SeqCst), 0);
}

#[test]
fn replacing_the_source_with_identical_bytes_still_invalidates_its_inspection() {
    use std::io::Write;
    let f = fixture();
    let first = f.inspect();
    let mut replacement = tempfile::NamedTempFile::new_in(&f.home).unwrap();
    replacement.write_all(MARKDOWN.as_bytes()).unwrap();
    replacement.persist(&f.source).unwrap();
    let second = f.inspect();
    assert_eq!(first.source_sha256, second.source_sha256);
    assert_ne!(first.inspection_id, second.inspection_id);
    assert!(
        f.service
            .plan_roadmap(&first, &f.selection())
            .unwrap_err()
            .contains("changed")
    );
    assert!(f.service.workflow_store().list_graphs().is_empty());
}

#[test]
fn replacing_identical_source_after_native_review_prevents_dispatch() {
    use std::io::Write;
    let f = fixture();
    let task = f
        .service
        .plan_roadmap(&f.inspect(), &f.selection())
        .unwrap();
    let mut replacement = tempfile::NamedTempFile::new_in(&f.home).unwrap();
    replacement.write_all(MARKDOWN.as_bytes()).unwrap();
    replacement.persist(&f.source).unwrap();
    f.service.approve(task.id).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while f.service.snapshot(0).phase == Phase::Running && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(f.service.snapshot(0).phase, Phase::Failed);
    assert_eq!(f.starts.load(Ordering::SeqCst), 0);
    assert!(!f.home.join("X Worktrees").exists());
}

#[test]
fn stored_task_identity_prevents_duplicate_import_and_does_not_become_source_completion() {
    let f = fixture();
    f.service
        .plan_roadmap(&f.inspect(), &f.selection())
        .unwrap();
    let store = f.service.workflow_store();
    let mut copy = store.get_graph(&store.list_graphs()[0].id).unwrap();
    copy.id = "copied-proposal".into();
    assert!(store.save_graph(&copy).is_err());
    assert_eq!(store.list_graphs().len(), 1);
    f.service.cancel();
    std::fs::write(
        &f.source,
        MARKDOWN.replace("Add shortcuts", "Add different shortcuts"),
    )
    .unwrap();
    let changed = f.inspect();
    assert!(matches!(
        changed.entries[1].disposition,
        Disposition::SavedProposal {
            changed_since_proposal: true,
            last_outcome: None,
            ..
        }
    ));
    std::fs::write(&f.source, MARKDOWN.replace("- [ ] ui:", "- [x] ui:")).unwrap();
    assert!(matches!(
        f.inspect().entries[1].disposition,
        Disposition::MarkedComplete
    ));
}

#[test]
fn inventory_lock_and_tampered_file_identity_fail_without_partial_proposals() {
    let f = fixture();
    let proposal = f.inspect();
    let store = f.service.workflow_store();
    let held = store.lock_roadmap_inventory().unwrap();
    assert!(
        f.service
            .plan_roadmap(&proposal, &f.selection())
            .unwrap_err()
            .contains("being saved")
    );
    assert!(store.list_graphs().is_empty());
    drop(held);
    f.service.plan_roadmap(&proposal, &f.selection()).unwrap();
    let graph_id = store.list_graphs()[0].id.clone();
    f.service.cancel();
    std::fs::rename(
        store.dir().join(format!("{graph_id}.json")),
        store.dir().join("wrong-name.json"),
    )
    .unwrap();
    assert!(
        f.service
            .inspect_roadmap(&f.project.id.to_string(), &f.source)
            .unwrap_err()
            .contains("mismatched file identity")
    );
    assert_eq!(f.starts.load(Ordering::SeqCst), 0);
}

#[test]
fn artifact_outcomes_do_not_check_off_source_tasks_or_unlock_semantic_dependencies() {
    let f = fixture();
    f.service
        .plan_roadmap(&f.inspect(), &f.selection())
        .unwrap();
    let store = f.service.workflow_store();
    let graph_id = store.list_graphs()[0].id.clone();
    let graph = store.get_graph(&graph_id).unwrap();
    store
        .record_graph_last(
            &graph_id,
            &crate::workflows::scheduler::Report {
                nodes: graph
                    .nodes
                    .iter()
                    .map(|node| crate::workflows::scheduler::NodeReport {
                        id: node.id.clone(),
                        outcome: if matches!(node.job, Job::Test { .. }) {
                            Outcome::Tested
                        } else {
                            Outcome::Verified
                        },
                        reason: String::new(),
                    })
                    .collect(),
                stopped: false,
                max_concurrent: 2,
            },
        )
        .unwrap();
    f.service.cancel();
    let proposal = f.inspect();
    assert!(matches!(
        proposal.entries[1].disposition,
        Disposition::SavedProposal {
            last_outcome: Some(Outcome::Verified),
            ..
        }
    ));
    assert!(!proposal.entries[1].task.completed_in_source);
    assert!(
        matches!(&proposal.entries[4].disposition, Disposition::Blocked { dependencies } if dependencies == &["ui", "backend"])
    );
    assert_eq!(std::fs::read_to_string(&f.source).unwrap(), MARKDOWN);
    assert_eq!(f.starts.load(Ordering::SeqCst), 0);
}

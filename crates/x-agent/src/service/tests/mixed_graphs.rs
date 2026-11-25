use super::*;
use crate::tools::EffectState;
use crate::workflows::dag::{ARTIFACT_VERSION, Graph, Job, Node};
use crate::workflows::model::{OnFailure, StepAction};
use crate::workflows::tests::Fake;

fn desktop(id: &str, action: StepAction, requires: &[&str]) -> Node {
    Node {
        id: id.into(),
        after: vec![],
        requires: requires.iter().map(|id| (*id).into()).collect(),
        requires_verified: vec![],
        required_change: None,
        required_commit: false,
        inputs: vec![],
        job: Job::Desktop { action },
        on_failure: OnFailure::Continue,
    }
}

fn save(directory: &Path, graph: &Graph) {
    std::fs::write(
        directory.join("state/workflows/coding-graph.json"),
        serde_json::to_vec(graph).unwrap(),
    )
    .unwrap();
}

fn chain(graph: &mut Graph) {
    let mut writer = graph.nodes[0].clone();
    writer.id = "writer".into();
    writer.required_commit = true;
    writer.requires_verified = vec!["editor".into()];
    let Job::Harness { goal, .. } = &mut writer.job else {
        unreachable!()
    };
    *goal = "write implementation".into();
    graph.version = ARTIFACT_VERSION;
    graph.nodes = vec![
        desktop(
            "editor",
            StepAction::OpenApp {
                app: "vscode".into(),
                workspace: None,
            },
            &[],
        ),
        writer,
        desktop(
            "focus",
            StepAction::FocusWindow {
                app: "vscode".into(),
                pick: None,
            },
            &["writer"],
        ),
        desktop(
            "close",
            StepAction::CloseOpenedWindow {
                opened_by: "editor".into(),
            },
            &["focus"],
        ),
    ];
}

#[test]
fn mixed_app_coding_and_desktop_nodes_share_one_review_and_real_artifact_proof() {
    let (service, directory, repo, mut graph) = graph_workflow_fixture();
    let host = Arc::new(Fake::new());
    service.set_machine(host.clone());
    chain(&mut graph);
    fake_opencode(
        directory.path(),
        r#"set -e
printf 'implemented\n' > implementation.txt
git add implementation.txt
git commit -m implementation >/dev/null
echo '{"type":"step_start","sessionID":"ses_mixed","part":{"type":"step-start"}}'
"#,
    );
    save(directory.path(), &graph);
    let task = service.plan_graph_workflow(&graph.id).unwrap();
    assert_eq!(task.phase, Phase::AwaitingApproval);
    assert!(task.steps[2].title.contains("predecessor editor"));
    assert_eq!(host.launches.load(Ordering::SeqCst), 0);
    assert_eq!(service.scoped_grants().pending_count().unwrap(), 0);
    assert!(service.approve_background(task.id).is_err());
    service.approve(task.id).unwrap();
    let finished = wait_for(&service, |snapshot| !snapshot.phase.is_busy());
    assert_eq!(finished.phase, Phase::Completed, "{:?}", finished.task);
    let steps = finished.task.unwrap().steps;
    assert_eq!(
        steps
            .iter()
            .map(|step| step.status.as_str())
            .collect::<Vec<_>>(),
        vec!["done", "done", "done", "done"]
    );
    for index in [0, 2, 3] {
        assert_eq!(
            steps[index].execution.as_ref().unwrap().state,
            EffectState::Confirmed
        );
    }
    assert!(host.log().iter().any(|entry| entry == "focus code"));
    assert!(
        host.log()
            .iter()
            .any(|entry| entry.starts_with("close exact"))
    );
    assert!(host.windows_now().is_empty());
    assert!(!repo.join("implementation.txt").exists());
    let claims = service.coding_claims_summary().unwrap();
    let jobs = claims["jobs"].as_array().unwrap();
    assert!(
        jobs.iter().find(|job| job["node_id"] == "writer").unwrap()["artifact"]["commit"]
            .is_string()
    );
    let editor = jobs.iter().find(|job| job["node_id"] == "editor").unwrap();
    assert_eq!(editor["desktop_outcome"], "verified");
    assert_eq!(editor["requires_provider_check_before_retry"], false);
    assert_eq!(editor["requires_new_desktop_review"], true);
}

#[test]
fn mixed_work_never_holds_the_desktop_lease_and_individual_stop_preserves_its_sibling_chain() {
    let (service, directory, _, mut graph) = graph_workflow_fixture();
    let host = Arc::new(Fake::new());
    service.set_machine(host.clone());
    let mut independent = graph.nodes[0].clone();
    independent.id = "independent".into();
    let Job::Harness { goal, .. } = &mut independent.job else {
        unreachable!()
    };
    *goal = "independent slow job".into();
    chain(&mut graph);
    graph.nodes.push(independent);
    let marker = directory.path().join("independent-started");
    fake_opencode(
        directory.path(),
        &format!(
            r#"set -e
case "$*" in
  *independent*) echo '{{"type":"step_start","sessionID":"ses_independent","part":{{"type":"step-start"}}}}'; touch '{}'; while :; do sleep 1; done;;
  *) printf 'implemented\n' > implementation.txt; git add implementation.txt; git commit -m implementation >/dev/null; echo '{{"type":"step_start","sessionID":"ses_writer","part":{{"type":"step-start"}}}}';;
esac
"#,
            marker.display()
        ),
    );
    save(directory.path(), &graph);
    let task = service.plan_graph_workflow(&graph.id).unwrap();
    let held = service.try_desktop_control().unwrap();
    service.approve(task.id).unwrap();
    wait_for(&service, |_| marker.is_file());
    assert_eq!(
        host.launches.load(Ordering::SeqCst),
        0,
        "the desktop node must wait for the shared lease"
    );
    drop(held);
    // A running provider does not reserve desktop input; the independent app
    // node may acquire it and unblock its own coding/desktop continuation.
    service.cancel_coding_job(task.id, 4).unwrap();
    let finished = wait_for(&service, |snapshot| !snapshot.phase.is_busy());
    let steps = finished.task.unwrap().steps;
    assert_eq!(steps[4].status, "cancelled");
    assert!(steps[..4].iter().all(|step| step.status == "done"));
    assert!(host.windows_now().is_empty());
    assert!(service.try_desktop_control().is_ok());
    let claims = service.coding_claims_summary().unwrap();
    let stopped = claims["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["node_id"] == "independent")
        .unwrap();
    assert_eq!(stopped["evidence_recorded"], true);
}

#[test]
fn desktop_launch_releases_only_delivery_edges_and_never_verification_edges() {
    let (service, directory, _, mut graph) = graph_workflow_fixture();
    let host = Arc::new(Fake::new());
    service.set_machine(host);
    let mut ordinary = graph.nodes[0].clone();
    ordinary.id = "ordinary".into();
    ordinary.requires = vec!["page".into()];
    let mut proof = ordinary.clone();
    proof.id = "proof".into();
    proof.requires.clear();
    proof.requires_verified = vec!["page".into()];
    graph.version = ARTIFACT_VERSION;
    graph.nodes = vec![
        desktop(
            "page",
            StepAction::OpenUrl {
                url: "https://example.com".into(),
            },
            &[],
        ),
        proof,
        ordinary,
    ];
    save(directory.path(), &graph);
    let task = service.plan_graph_workflow(&graph.id).unwrap();
    service.approve(task.id).unwrap();
    let finished = wait_for(&service, |snapshot| !snapshot.phase.is_busy());
    let steps = finished.task.unwrap().steps;
    assert_eq!(steps[0].status, "launched");
    assert_eq!(
        steps[0].execution.as_ref().unwrap().state,
        EffectState::Launched
    );
    assert_eq!(steps[1].status, "skipped");
    assert_eq!(steps[2].status, "delivered");
    let claims = service.coding_claims_summary().unwrap();
    assert_eq!(claims["count"], 2);
    assert_eq!(
        claims["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["node_id"] == "page")
            .unwrap()["desktop_outcome"],
        "launched"
    );
}

#[test]
fn changed_reviewed_window_fails_without_input_and_only_blocks_its_descendants() {
    let (service, directory, _, mut graph) = graph_workflow_fixture();
    let host = Arc::new(Fake::new());
    let existing = crate::wm::WinInfo {
        id: "0xabc".into(),
        pid: 123,
        class: "code".into(),
        title: "Editor".into(),
        workspace: 1,
        place: crate::wm::Place::N(1),
        minimized: false,
        maximized: false,
        recent: 0,
    };
    host.w.lock().unwrap().wins.push(existing);
    service.set_machine(host.clone());
    let mut child = graph.nodes[0].clone();
    child.requires_verified = vec!["focus".into()];
    graph.version = ARTIFACT_VERSION;
    graph.nodes = vec![
        desktop(
            "focus",
            StepAction::FocusWindow {
                app: "vscode".into(),
                pick: None,
            },
            &[],
        ),
        child,
        desktop(
            "independent",
            StepAction::SwitchWorkspace { workspace: 2 },
            &[],
        ),
    ];
    save(directory.path(), &graph);
    let task = service.plan_graph_workflow(&graph.id).unwrap();
    assert!(task.steps[0].title.contains("process 123"));
    host.w.lock().unwrap().wins[0].pid = 999;
    service.approve(task.id).unwrap();
    let finished = wait_for(&service, |snapshot| !snapshot.phase.is_busy());
    let steps = finished.task.unwrap().steps;
    assert_eq!(steps[0].status, "failed");
    assert_eq!(steps[1].status, "skipped");
    assert_eq!(steps[2].status, "done");
    assert!(!host.log().iter().any(|entry| entry.starts_with("focus")));
    assert_eq!(host.windows_now()[0].pid, 999);
    assert_eq!(service.coding_claims_summary().unwrap()["count"], 2);
}

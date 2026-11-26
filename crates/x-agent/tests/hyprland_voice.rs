#![cfg(target_os = "linux")]

use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use x_agent::hexcli::{Stt, SttError};
use x_agent::phase::Phase;
use x_agent::service::{
    ActionCompletion, InputSource, Mode, Origin, Planner, RealLauncher, Service, Snapshot,
};
use x_agent::tools::{EffectEvidence, EffectState, Reply};

struct FinishedTranscript {
    requests: Sender<()>,
    replies: Mutex<Receiver<String>>,
}

impl Stt for FinishedTranscript {
    fn start(&self) -> Result<(), SttError> {
        Ok(())
    }

    fn stop_print(&self) -> Result<String, SttError> {
        self.requests.send(()).unwrap();
        self.replies
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| SttError::NoSpeech)
    }

    fn cancel(&self) -> Result<(), SttError> {
        Ok(())
    }
}

#[derive(Default)]
struct UnexpectedPlanner(AtomicUsize);

impl Planner for UnexpectedPlanner {
    fn name(&self) -> String {
        "Unexpected test planner".into()
    }

    fn plan(&self, _: &str, _: bool) -> Result<Reply, String> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err("The workspace command should be resolved locally".into())
    }
}

fn workspace() -> u64 {
    let reply = Command::new("hyprctl")
        .args(["-j", "activeworkspace"])
        .output()
        .unwrap();
    assert!(reply.status.success());
    serde_json::from_slice::<serde_json::Value>(&reply.stdout).unwrap()["id"]
        .as_u64()
        .unwrap()
}

fn wait_for(service: &Service, phase: Phase) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = service.snapshot(0);
        if snapshot.phase == phase {
            return snapshot;
        }
        assert!(
            !matches!(snapshot.phase, Phase::Failed | Phase::Cancelled)
                && Instant::now() < deadline,
            "Waiting for {phase:?}, got {snapshot:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "mutates a disposable Hyprland session; run scripts/smoke-x/hyprland-voice.sh"]
fn finished_voice_reaches_verified_workspace_and_a_stopped_capture_cannot_move_it() {
    assert_eq!(
        std::env::var("X_TEST_ISOLATED_HYPRLAND").as_deref(),
        Ok("1")
    );
    assert_eq!(workspace(), 1);
    let (requests, requested) = channel();
    let (reply, replies) = channel();
    let stt = Arc::new(FinishedTranscript {
        requests,
        replies: Mutex::new(replies),
    });
    let service = Service::new(stt, Arc::new(RealLauncher), None);
    let planner = Arc::new(UnexpectedPlanner::default());
    service.set_planner(Some(planner.clone()));

    service.voice_start(Mode::Actions).unwrap();
    service.voice_stop().unwrap();
    requested.recv_timeout(Duration::from_secs(5)).unwrap();
    service.cancel_foreground();
    assert!(
        service.voice_start(Mode::Actions).is_err(),
        "a late stop must drain before another capture"
    );
    reply.send("go to workspace four".into()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while service.voice_start(Mode::Actions).is_err() {
        assert!(Instant::now() < deadline, "cancelled capture did not drain");
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(workspace(), 1);
    assert!(service.snapshot(0).task.is_none());

    service.voice_stop().unwrap();
    requested.recv_timeout(Duration::from_secs(5)).unwrap();
    reply.send("go to workspace three".into()).unwrap();
    let snapshot = wait_for(&service, Phase::AwaitingApproval);
    assert!(
        snapshot
            .events
            .iter()
            .all(|event| !event.text.contains("go to workspace"))
    );
    let preview = snapshot.task.unwrap();
    assert_eq!(preview.steps.len(), 1);
    let request = preview.steps[0].request.as_ref().unwrap();
    assert_eq!(request.input_source, InputSource::PushToTalk);
    assert_eq!(request.origin, Origin::Rules);
    assert_eq!(workspace(), 1);
    service.approve(preview.id).unwrap();
    let done = wait_for(&service, Phase::Completed);
    assert_eq!(done.action_outcome, Some(ActionCompletion::Confirmed));
    let task = done.task.unwrap();
    assert_eq!(task.id, preview.id);
    let step = task.steps[0].execution.as_ref().unwrap();
    assert_eq!(step.state, EffectState::Confirmed);
    assert_eq!(step.evidence, Some(EffectEvidence::NativeWindowReadback));
    assert_eq!(workspace(), 3);
    assert!(service.approve(preview.id).is_err());
    assert_eq!(planner.0.load(Ordering::Relaxed), 0);
}

#[test]
#[ignore = "mutates a disposable Hyprland session; run scripts/smoke-x/hyprland-voice.sh"]
fn saved_workflow_inserts_literal_text_once_and_rejects_a_reused_editor() {
    use x_agent::service::AutonomyMode;
    use x_agent::workflows::model::{OnFailure, Step, StepAction, Workflow};

    assert_eq!(
        std::env::var("X_TEST_ISOLATED_HYPRLAND").as_deref(),
        Ok("1")
    );
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let output = home.join("editor-text");
    let (requests, _requested) = channel();
    let (_reply, replies) = channel();
    let service = Service::new(
        Arc::new(FinishedTranscript {
            requests,
            replies: Mutex::new(replies),
        }),
        Arc::new(RealLauncher),
        None,
    );
    service.set_autonomy_mode(AutonomyMode::Autonomous);
    service.set_workflow_dirs(home.join("workflows"), home.clone());
    let literal = "  Café and open Firefox  ";
    let workflow = Workflow {
        id: "note".into(),
        name: "Write a local note".into(),
        triggers: vec![],
        steps: vec![
            Step {
                id: "editor".into(),
                action: StepAction::OpenApp {
                    app: "text-editor".into(),
                    workspace: None,
                },
                requires: vec![],
                timeout_secs: Some(5),
                on_failure: OnFailure::Stop,
            },
            Step {
                id: "note".into(),
                action: StepAction::TypeText {
                    opened_by: "editor".into(),
                    text: literal.into(),
                },
                requires: vec!["editor".into()],
                timeout_secs: None,
                on_failure: OnFailure::Stop,
            },
        ],
    };
    service.workflow_store().save(&workflow).unwrap();
    let preview = service.plan_workflow_by_id("note").unwrap();
    assert_eq!(service.snapshot(0).phase, Phase::AwaitingApproval);
    assert!(preview.steps[1].detail.contains(literal));
    assert!(!output.exists());
    service.approve(preview.id).unwrap();
    let done = wait_for(&service, Phase::Completed);
    assert_eq!(std::fs::read_to_string(&output).unwrap(), literal);
    assert!(
        done.events
            .iter()
            .all(|event| !event.text.contains(literal))
    );
    assert!(service.approve(preview.id).is_err());
    let report = service.workflow_store().last("note").unwrap();
    assert!(
        report
            .steps
            .iter()
            .all(|step| step.status == x_agent::workflows::StepStatus::Done)
    );
    assert!(!serde_json::to_string(&report).unwrap().contains(literal));

    let repeat = service.plan_workflow_by_id("note").unwrap();
    service.approve(repeat.id).unwrap();
    wait_for(&service, Phase::Failed);
    assert_eq!(std::fs::read_to_string(&output).unwrap(), literal);

    std::fs::write(home.join("close-editor"), []).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !x_agent::wm::view().unwrap().windows.is_empty() {
        assert!(
            Instant::now() < deadline,
            "the fixture editor did not close"
        );
        thread::sleep(Duration::from_millis(25));
    }
    std::fs::write(home.join("protected-editor"), []).unwrap();
    let protected = service.plan_workflow_by_id("note").unwrap();
    service.approve(protected.id).unwrap();
    let failed = wait_for(&service, Phase::Failed).task.unwrap();
    assert!(failed.steps[1].detail.contains("plain text field"));
    assert_eq!(std::fs::read_to_string(output).unwrap(), literal);
    std::fs::write(home.join("close-editor"), []).unwrap();
}

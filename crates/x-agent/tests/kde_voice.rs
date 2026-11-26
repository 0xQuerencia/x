#![cfg(target_os = "linux")]

use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use x_agent::hexcli::HexCli;
use x_agent::phase::Phase;
use x_agent::service::{ActionCompletion, Mode, RealLauncher, Service};
use x_agent::wm::{self, Cap, EditorTyping, SupportState};

#[test]
#[ignore = "use scripts/smoke-x/kde-voice.sh for a disposable KWin desktop"]
fn kde_requires_session_opt_in() {
    assert_eq!(std::env::var("X_TEST_ISOLATED_KDE").as_deref(), Ok("1"));
    let report = wm::report();
    assert!(report.backend.is_none());
    assert_eq!(report.editor_typing, EditorTyping::Unsupported);
    assert!(
        report
            .capabilities
            .iter()
            .all(|capability| capability.state == SupportState::NeedsPermission)
    );
}

fn native_workspace() -> usize {
    let output = Command::new("kdotool").arg("get_desktop").output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
#[ignore = "use scripts/smoke-x/kde-voice.sh for a disposable KWin desktop"]
fn kde_verifies_workspace_switch_after_typed_approval() {
    assert_eq!(std::env::var("X_TEST_ISOLATED_KDE").as_deref(), Ok("1"));
    let report = wm::report();
    assert_eq!(report.backend.as_deref(), Some("KDE Plasma 6"));
    assert_eq!(report.editor_typing, EditorTyping::Unsupported);
    assert!(
        report
            .capabilities
            .iter()
            .any(|capability| capability.capability == Cap::List
                && capability.state == SupportState::Supported)
    );
    assert_eq!(native_workspace(), 1);

    let service = Service::new(Arc::new(HexCli::default()), Arc::new(RealLauncher), None);
    let task = service
        .plan_spoken("go to workspace two", Mode::Actions)
        .unwrap();
    assert_eq!(task.phase, Phase::AwaitingApproval);
    assert_eq!(native_workspace(), 1);
    service.approve(task.id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let snapshot = service.snapshot(0);
        if snapshot.phase == Phase::Completed {
            assert_eq!(snapshot.action_outcome, Some(ActionCompletion::Confirmed));
            break;
        }
        assert!(
            snapshot.phase.is_busy() && Instant::now() < deadline,
            "Unexpected KDE action outcome: {snapshot:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(native_workspace(), 2);
}

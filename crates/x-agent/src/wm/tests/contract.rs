use super::super::report::{SelectionFailure, report_for};
use super::*;

const OPERATIONS: [(Cap, &str); 7] = [
    (Cap::List, "list windows"),
    (Cap::Focus, "focus terminal"),
    (Cap::SwitchWorkspace, "switch to workspace 3"),
    (Cap::MoveToWorkspace, "move this to workspace 3"),
    (Cap::Maximize, "maximize this"),
    (Cap::Minimize, "minimize this"),
    (Cap::Close, "close this"),
];

fn assert_report(
    report: &Report,
    advertised: &[Cap],
    expected: impl Fn(Cap) -> (SupportState, ProbeEvidence),
) {
    assert_eq!(report.supported, advertised);
    assert_eq!(report.capabilities.len(), OPERATIONS.len());
    for ((cap, _), entry) in OPERATIONS.iter().zip(&report.capabilities) {
        assert_eq!(entry.capability, *cap);
        assert_eq!((entry.state, entry.evidence), expected(*cap), "{cap:?}");
    }
}

fn assert_selected_report(report: &Report, advertised: &[Cap], list_evidence: ProbeEvidence) {
    use ProbeEvidence as Evidence;
    use SupportState as State;

    assert_report(report, advertised, |cap| {
        if !advertised.contains(&cap) {
            return (State::Unsupported, Evidence::BackendDidNotAdvertise);
        }
        if cap == Cap::List && list_evidence == Evidence::WindowListRead {
            return (State::Supported, Evidence::WindowListRead);
        }
        if list_evidence == Evidence::WindowListFailed {
            return (State::Partial, Evidence::WindowListFailed);
        }
        (State::Partial, Evidence::BackendAdvertised)
    });
}

#[test]
fn window_report_distinguishes_live_read_unprobed_failed_and_permission_required() {
    use ProbeEvidence as Evidence;
    use SupportState as State;

    let (_, fake) = setup();
    let caps = fake.caps();
    assert_selected_report(&report_for(Ok(&fake)), &caps, Evidence::BackendAdvertised);
    fake.0.lock().unwrap().probe_list = true;
    assert_selected_report(&report_for(Ok(&fake)), &caps, Evidence::WindowListRead);
    fake.0.lock().unwrap().dead = true;
    assert_selected_report(&report_for(Ok(&fake)), &caps, Evidence::WindowListFailed);
    assert!(fake.0.lock().unwrap().log.is_empty());

    for (opt_in_required, expected) in [
        (false, (State::Unsupported, Evidence::BackendUnavailable)),
        (
            true,
            (State::NeedsPermission, Evidence::BackendOptInRequired),
        ),
    ] {
        let report = report_for(Err(&SelectionFailure {
            note: "test session".into(),
            opt_in_required,
        }));
        assert_eq!(report.backend, None);
        assert_report(&report, &[], |_| expected);
    }
}

fn assert_profile(backend: &dyn Backend, unsupported: &[Cap]) {
    let expected: Vec<_> = OPERATIONS
        .iter()
        .filter(|(cap, _)| !unsupported.contains(cap))
        .map(|(cap, _)| *cap)
        .collect();
    let actual = backend.caps();
    assert_eq!(actual.len(), expected.len(), "{}", backend.name());
    assert!(
        expected.iter().all(|cap| actual.contains(cap)),
        "{} advertised {actual:?}, expected {expected:?}",
        backend.name()
    );

    // Exercise the real controller in a disposable world with this adapter's capability set.
    for (cap, phrase) in OPERATIONS {
        let (mut controller, fake) = setup();
        fake.0.lock().unwrap().caps = actual.clone();
        assert_selected_report(
            &report_for(Ok(&fake)),
            &actual,
            ProbeEvidence::BackendAdvertised,
        );
        let result = ask(&mut controller, phrase);
        let state = fake.0.lock().unwrap();
        if unsupported.contains(&cap) {
            assert!(matches!(result, Err(WmError::Unsupported(_))), "{result:?}");
            assert!(
                state.log.is_empty(),
                "a missing capability reached dispatch"
            );
        } else {
            assert!(result.is_ok(), "{} {cap:?}: {result:?}", backend.name());
            if cap == Cap::List {
                assert!(result.unwrap().message.contains("Docs"));
                assert!(state.log.is_empty());
            } else {
                assert_eq!(state.log.len(), 1);
                assert_eq!(state.log[0].cap(), cap);
            }
        }
    }
}

#[test]
fn unsupported_focus_cannot_restore_a_minimized_window() {
    let (mut controller, fake) = setup();
    {
        let mut world = fake.0.lock().unwrap();
        world.caps.retain(|cap| *cap != Cap::Focus);
        world.wins[1].minimized = true;
    }
    let result = ask(&mut controller, "focus terminal");
    assert!(matches!(result, Err(WmError::Unsupported(_))), "{result:?}");
    assert!(fake.0.lock().unwrap().log.is_empty());
}

#[cfg(not(windows))]
#[test]
fn unix_backends_share_the_controller_contract() {
    assert_profile(&hypr::Hypr, &[]);
    assert_profile(&kde::Kde, &[]);
    for (bin, label) in [("swaymsg", "Sway"), ("i3-msg", "i3")] {
        assert_profile(&i3::I3 { bin, label }, &[Cap::Minimize]);
    }
    let unsupported = if crate::tools::which("xdotool").is_some() {
        &[][..]
    } else {
        &[Cap::Minimize][..]
    };
    assert_profile(&x11::X11, unsupported);
}

#[cfg(windows)]
#[test]
fn windows_shares_the_controller_contract() {
    assert_profile(&win11::Win11, &[Cap::SwitchWorkspace, Cap::MoveToWorkspace]);
}

pub(in crate::wm) fn assert_live_report(backend: &dyn Backend) {
    let advertised = backend.caps();
    let report = report_for(Ok(backend));
    let expected = if cfg!(windows) || backend.name() == "Hyprland" {
        ProbeEvidence::WindowListRead
    } else {
        ProbeEvidence::BackendAdvertised
    };
    assert_selected_report(&report, &advertised, expected);
}

/// Read-only contract for an actual selected backend. A live window supplies
/// the ID, while the altered PID must be refused before any native mutation.
#[test]
#[ignore = "requires a selected desktop backend and a visible window"]
fn live_desktop_read_and_identity_contract() {
    let backend = detect().expect("usable desktop backend");
    assert_live_report(backend.as_ref());
    let mut controller = Controller::new(backend);
    let view = controller.view().unwrap();
    let window = view.windows.first().expect("a visible window");
    let stale_pid = window.pid ^ 1;
    assert!(matches!(
        controller.close_exact(&window.id, stale_pid, &window.class),
        Err(WmError::NotFound(_))
    ));
    assert!(controller.history.is_empty());
}

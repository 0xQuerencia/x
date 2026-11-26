use super::*;
use crate::workflows::dag::ARTIFACT_VERSION;
use crate::workflows::model::OnFailure;
use crate::workflows::tests::Fake;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn graph() -> Graph {
    Graph {
        version: ARTIFACT_VERSION,
        id: "desktop-scope".into(),
        name: "Desktop scope".into(),
        triggers: vec![],
        roadmap: None,
        nodes: vec![Node {
            id: "page".into(),
            after: vec![],
            requires: vec![],
            requires_verified: vec![],
            required_change: None,
            required_commit: false,
            inputs: vec![],
            on_failure: OnFailure::Continue,
            job: Job::Desktop {
                action: StepAction::OpenUrl {
                    url: "https://example.com".into(),
                },
            },
        }],
    }
}

#[test]
fn revoking_or_expiring_a_desktop_grant_while_acquiring_input_prevents_mutation() {
    for expire in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let graph = graph();
        let host = Arc::new(Fake::new());
        let cfg = Config {
            home: directory.path().into(),
            poll: Duration::from_millis(1),
        };
        let mut plans = prepare(&graph, host.as_ref(), &cfg).unwrap();
        let run = Uuid::new_v4();
        let grants = Arc::new(Grants::default());
        let plan = plans.pop().unwrap();
        let receipt = grants
            .issue(
                Permission::QueueDesktop,
                plan.scope(run, None).unwrap(),
                Duration::from_secs(120),
                1,
                true,
            )
            .unwrap();
        let revoke = grants.clone();
        let queued = receipt.clone();
        let acquisitions = Arc::new(AtomicUsize::new(0));
        let observed = acquisitions.clone();
        let acquire: AcquireInput = Arc::new(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            if expire {
                revoke.expire_for_test(&queued);
            } else {
                revoke.revoke_all()?;
            }
            Ok(Box::new(()))
        });
        let executor = DesktopExecutor::new(
            run,
            vec![(plan, receipt)],
            host.clone(),
            cfg,
            acquire,
            grants,
            Arc::new(Journal::new(directory.path().join("journal"))),
        )
        .unwrap();
        assert_eq!(
            executor.execute(graph.nodes[0].clone(), Cancellation::default()),
            Outcome::Failed
        );
        assert_eq!(acquisitions.load(Ordering::SeqCst), 1);
        assert_eq!(host.launches.load(Ordering::SeqCst), 0);
        if expire {
            assert!(
                executor
                    .outcome_reason(&graph.nodes[0])
                    .unwrap()
                    .contains("Approval has expired")
            );
        }
    }
}

#[test]
fn a_finished_desktop_claim_never_replays_when_a_runner_is_reconstructed() {
    let directory = tempfile::tempdir().unwrap();
    let graph = graph();
    let host = Arc::new(Fake::new());
    let run = Uuid::new_v4();
    for expected in [Outcome::Launched, Outcome::Unverified] {
        let cfg = Config {
            home: directory.path().into(),
            poll: Duration::from_millis(1),
        };
        let mut plans = prepare(&graph, host.as_ref(), &cfg).unwrap();
        let plan = plans.pop().unwrap();
        let grants = Arc::new(Grants::default());
        let receipt = grants
            .issue(
                Permission::QueueDesktop,
                plan.scope(run, None).unwrap(),
                Duration::from_secs(120),
                1,
                true,
            )
            .unwrap();
        let executor = DesktopExecutor::new(
            run,
            vec![(plan, receipt)],
            host.clone(),
            cfg,
            Arc::new(|_| Ok(Box::new(()))),
            grants,
            Arc::new(Journal::new(directory.path().join("journal"))),
        )
        .unwrap();
        assert_eq!(
            executor.execute(graph.nodes[0].clone(), Cancellation::default()),
            expected
        );
    }
    assert_eq!(host.launches.load(Ordering::SeqCst), 1);
}

struct StopDuringNativeRead(WinInfo);
impl Host for StopDuringNativeRead {
    fn windows(&self) -> Result<Vec<WinInfo>, String> {
        Ok(vec![self.0.clone()])
    }
    fn focused(&self) -> Result<Option<WinInfo>, String> {
        Ok(None)
    }
    fn workspace(&self) -> Result<i64, String> {
        Ok(1)
    }
    fn launch(&self, _: &Action) -> Result<String, String> {
        panic!("not an opener")
    }
    fn installed(&self, _: &Action) -> Result<(), String> {
        Ok(())
    }
    fn can(&self, _: Cap) -> bool {
        true
    }
    fn focus(&self, _: &WinInfo) -> Result<(), String> {
        panic!("node Stop must reach the scoped native adapter")
    }
    fn focus_scoped(&self, _: &WinInfo, stop: &Cancellation) -> Result<(), String> {
        // Represents Stop arriving during the final native identity lookup.
        stop.request();
        stop.check()
    }
    fn switch_workspace(&self, _: i64) -> Result<(), String> {
        unreachable!()
    }
    fn move_window(&self, _: &WinInfo, _: i64) -> Result<(), String> {
        unreachable!()
    }
    fn close_window(&self, _: &WinInfo) -> Result<(), String> {
        unreachable!()
    }
    fn opencode_ready(&self) -> Result<(), String> {
        unreachable!()
    }
    fn opencode(&self, _: &str, _: &Path) -> Result<String, String> {
        unreachable!()
    }
    fn permit(&self, _: &StepAction) -> bool {
        true
    }
    fn cancelled(&self) -> bool {
        false
    }
}

#[test]
fn desktop_node_passes_its_own_stop_through_the_final_native_adapter() {
    let directory = tempfile::tempdir().unwrap();
    let mut graph = graph();
    graph.nodes[0].job = Job::Desktop {
        action: StepAction::FocusWindow {
            app: "firefox".into(),
            pick: None,
        },
    };
    let host = Arc::new(StopDuringNativeRead(WinInfo {
        id: "window".into(),
        pid: 42,
        class: "firefox".into(),
        title: "Docs".into(),
        workspace: 1,
        place: crate::wm::Place::N(1),
        minimized: false,
        maximized: false,
        recent: 0,
    }));
    let cfg = Config {
        home: directory.path().into(),
        poll: Duration::from_millis(1),
    };
    let plan = prepare(&graph, host.as_ref(), &cfg).unwrap().pop().unwrap();
    let run = Uuid::new_v4();
    let grants = Arc::new(Grants::default());
    let receipt = grants
        .issue(
            Permission::QueueDesktop,
            plan.scope(run, None).unwrap(),
            Duration::from_secs(120),
            1,
            true,
        )
        .unwrap();
    let executor = DesktopExecutor::new(
        run,
        vec![(plan, receipt)],
        host,
        cfg,
        Arc::new(|_| Ok(Box::new(()))),
        grants,
        Arc::new(Journal::new(directory.path().join("journal"))),
    )
    .unwrap();
    let root = Cancellation::default();
    let child = root.child();
    let independent = root.child();
    assert_eq!(
        executor.execute(graph.nodes[0].clone(), child.clone()),
        Outcome::Cancelled
    );
    assert!(child.requested());
    assert!(!independent.requested());
    assert!(!root.requested());
}

fn node(id: &str, action: StepAction, requires_verified: &[&str]) -> Node {
    let mut node = graph().nodes.remove(0);
    node.id = id.into();
    node.job = Job::Desktop { action };
    node.requires_verified = requires_verified.iter().map(|id| (*id).into()).collect();
    node
}

fn approved(graph: &Graph, host: Arc<Fake>, home: &Path) -> Arc<DesktopExecutor> {
    let cfg = Config {
        home: home.into(),
        poll: Duration::from_millis(1),
    };
    let run = Uuid::new_v4();
    let grants = Arc::new(Grants::default());
    let plans = prepare(graph, host.as_ref(), &cfg)
        .unwrap()
        .into_iter()
        .map(|plan| {
            let receipt = grants
                .issue(
                    Permission::QueueDesktop,
                    plan.scope(run, None).unwrap(),
                    Duration::from_secs(120),
                    1,
                    true,
                )
                .unwrap();
            (plan, receipt)
        })
        .collect();
    Arc::new(
        DesktopExecutor::new(
            run,
            plans,
            host,
            cfg,
            Arc::new(|_| Ok(Box::new(()))),
            grants,
            Arc::new(Journal::new(home.join("journal"))),
        )
        .unwrap(),
    )
}

#[test]
fn ambiguous_new_windows_are_never_moved_published_or_closed() {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Fake::new());
    host.w.lock().unwrap().windows_per_launch = 2;
    let mut graph = graph();
    graph.nodes = vec![
        node(
            "editor",
            StepAction::OpenApp {
                app: "vscode".into(),
                workspace: Some(3),
            },
            &[],
        ),
        node(
            "focus",
            StepAction::FocusWindow {
                app: "vscode".into(),
                pick: None,
            },
            &["editor"],
        ),
        node(
            "close",
            StepAction::CloseOpenedWindow {
                opened_by: "editor".into(),
            },
            &["focus"],
        ),
    ];
    let executor = approved(&graph, host.clone(), directory.path());
    let report = super::super::scheduler::run(
        &graph,
        directory.path(),
        executor.clone(),
        &Cancellation::default(),
        2,
    )
    .unwrap();
    assert_eq!(
        report
            .nodes
            .iter()
            .map(|node| node.outcome)
            .collect::<Vec<_>>(),
        vec![Outcome::Failed, Outcome::Skipped, Outcome::Skipped]
    );
    assert!(report.nodes[0].reason.contains("ambiguous"));
    assert_eq!(host.log().len(), 1, "only the reviewed app launch may run");
    let windows = host.windows_now();
    assert_eq!(windows.len(), 2);
    assert!(windows.iter().all(|win| win.workspace == 1));
    let context = executor.context.lock().unwrap();
    assert!(context.windows.is_empty());
    assert!(context.opened.is_empty());
    assert!(context.reversals.is_empty());
}

#[test]
fn directory_open_can_supply_its_exact_window_to_a_dependent_focus() {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Fake::new());
    let mut graph = graph();
    graph.nodes = vec![
        node(
            "folder",
            StepAction::OpenDirectory {
                directory: directory.path().display().to_string(),
            },
            &[],
        ),
        node(
            "focus",
            StepAction::FocusWindow {
                app: "files".into(),
                pick: None,
            },
            &["folder"],
        ),
    ];
    let executor = approved(&graph, host.clone(), directory.path());
    let report = super::super::scheduler::run(
        &graph,
        directory.path(),
        executor,
        &Cancellation::default(),
        2,
    )
    .unwrap();
    assert!(
        report
            .nodes
            .iter()
            .all(|node| node.outcome == Outcome::Verified)
    );
    assert_eq!(host.windows_now().len(), 1);
    assert_eq!(
        host.focused().unwrap().unwrap().id,
        host.windows_now()[0].id
    );
}

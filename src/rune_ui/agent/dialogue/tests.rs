use std::sync::Arc;

use rune::text::TextSystem;
use rune::{Cx, Env, Ime, Key, KeyEvent, Mods, Runtime};

use super::super::{Mode, Phase, Task, task_card};
use super::*;
use ::x_agent::context::{Confidence, Source, Sourced, WindowCtx, WindowRef};
use ::x_agent::service::{Step, WindowChoice, WindowQuestion};

const SIZE: (f32, f32) = (600.0, 700.0);

fn fixture(selected: bool) -> Ui {
    let service = ::x_agent::service::Service::new(
        Arc::new(::x_agent::hexcli::HexCli::default()),
        Arc::new(::x_agent::service::RealLauncher),
        None,
    );
    let mut snapshot = service.snapshot(0);
    let phase = if selected {
        Phase::AwaitingApproval
    } else {
        Phase::WaitingForInput
    };
    let question = WindowQuestion {
        task_id: 42,
        step: 0,
        question_id: if selected { "review-token" } else { "choice-token" }.into(),
        operation: "Close exactly one browser window showing the account settings documentation for the selected project".into(),
        expires_in_ms: 24_000,
        choices: [
            "Account settings documentation — implementation notes, keyboard navigation requirements and the current review discussion",
            "Account settings documentation — previous release notes, outstanding questions and the archived implementation discussion",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, title)| WindowChoice {
            number: index + 1,
            context: Sourced {
                value: WindowCtx {
                    window: WindowRef {
                        id: format!("fixture-window-{}", index + 1),
                        pid: 400 + index as u32,
                        class: "fixture-browser".into(),
                    },
                    title: title.into(),
                },
                source: Source::WindowManager,
                confidence: Confidence::High,
            },
        })
        .collect(),
        selected: selected.then_some(2),
        previous: None,
    };
    snapshot.phase = phase;
    snapshot.task = Some(Task {
        id: question.task_id,
        mode: Mode::Actions,
        text: "Close the browser window for the old account settings documentation after I confirm the right window from the displayed list.".into(),
        steps: vec![Step {
            title: "Close one reviewed browser window".into(),
            status: "pending".into(),
            risk: "Unsaved changes may be lost".into(),
            request: None,
            detail: String::new(),
            execution: None,
        }],
        phase,
        error: None,
        summary: None,
        changed_files: None,
        workspace: None,
        session: None,
        accepted_session: None,
        work_access: Default::default(),
        work_harness: None,
        blocked: Vec::new(),
        edit: None,
    });
    snapshot.window_question = Some(question);
    let mut ui = Ui::with(
        crate::app_settings::AppSettings::default(),
        crate::rune_ui::Service::Fixture(Vec::new()),
    );
    ui.agent.dialogue.poll(&snapshot);
    ui.agent.snapshot = Some(snapshot);
    ui
}

fn drawn(ui: &Ui, cx: &mut Cx<Ui>) -> El {
    let p = Paint::new(cx.system_dark(), cx.env().scale);
    let snapshot = ui.agent.snapshot.as_ref().unwrap();
    col()
        .key("root")
        .w_full()
        .h_full()
        .padding(24.0)
        .fill(p.k.c.bg_base)
        .child(task_card(
            p,
            &ui.agent,
            snapshot,
            snapshot.task.as_ref().unwrap(),
        ))
}

fn runtime(dark: bool, selected: bool) -> Runtime<Ui> {
    let mut rt = crate::rune_ui::offscreen::runtime(
        fixture(selected),
        drawn,
        SIZE,
        Env::new(dark, 1.0),
        TextSystem::new(rune_theme_opencode::FONTS, rune_theme_opencode::FAMILIES),
    );
    rt.settle_all();
    rt.frame(0.0);
    rt
}

#[test]
fn a_narrow_window_question_keeps_choices_and_review_readable() {
    for dark in [false, true] {
        for selected in [false, true] {
            let rt = runtime(dark, selected);
            let elements = rt.elements();
            for key in [
                "task-text",
                "window-question-prompt",
                "window-choice-label-1",
                "window-choice-label-2",
            ] {
                let element = elements
                    .iter()
                    .find(|element| element.path.ends_with(&format!("/{key}")))
                    .unwrap_or_else(|| panic!("missing {key}"));
                let [x, y, width, height] = element.rect;
                assert!(height >= 24.0, "{key} must wrap: {:?}", element.rect);
                assert!(
                    x >= 24.0 && x + width <= SIZE.0 - 24.0 + 0.5,
                    "{key} overflows horizontally: {:?}",
                    element.rect
                );
                assert!(
                    y + height <= SIZE.1 - 24.0 + 0.5,
                    "{key} is clipped: {:?}",
                    element.rect
                );
            }
            let mut controls = vec!["window-select-1", "window-select-2", "window-answer-review"];
            controls.extend(if selected {
                vec!["agent-approve", "agent-discard"]
            } else {
                vec!["agent-stop"]
            });
            for key in controls {
                let element = elements
                    .iter()
                    .find(|element| element.path.ends_with(&format!("/{key}")))
                    .unwrap_or_else(|| panic!("missing control {key}"));
                let [x, y, width, height] = element.rect;
                assert!(
                    x >= 24.0
                        && x + width <= SIZE.0 - 24.0 + 0.5
                        && y >= 24.0
                        && y + height <= SIZE.1 - 24.0 + 0.5,
                    "{key} is clipped: {:?}",
                    element.rect
                );
            }
            assert_eq!(rt.find("agent-approve").is_some(), selected);
            assert_eq!(rt.find("agent-discard").is_some(), selected);
            assert_eq!(rt.find("agent-stop").is_some(), !selected);
            let text: Vec<_> = elements
                .iter()
                .filter_map(|element| element.text.as_deref())
                .collect();
            assert!(text.iter().any(|text| text.contains("Task 42")));
            if selected {
                assert!(text.contains(&"Approve window action"));
                assert!(text.iter().any(|text| text.ends_with(" · selected")));
                assert!(text.iter().any(|text| text.contains("before approving")));
            }
        }
    }
}

#[test]
fn a_keyboard_answer_is_cleared_when_its_question_or_task_changes() {
    let mut rt = runtime(true, false);
    let reached = (0..15).any(|_| {
        rt.key(KeyEvent {
            key: Key::Tab,
            mods: Mods::default(),
        });
        rt.frame(0.0);
        rt.elements()
            .iter()
            .any(|element| element.focused && element.path.contains("window-answer/"))
    });
    assert!(reached, "the question answer must be reachable by Tab");
    rt.ime(Ime::Commit("the other one".into()));
    rt.frame(0.0);
    assert_eq!(rt.state.agent.dialogue.answer.text, "the other one");

    let mut snapshot = rt.state.agent.snapshot.clone().unwrap();
    snapshot.window_question.as_mut().unwrap().expires_in_ms = 20_000;
    assert!(!rt.state.agent.dialogue.poll(&snapshot));
    assert_eq!(rt.state.agent.dialogue.answer.text, "the other one");

    snapshot.window_question.as_mut().unwrap().question_id = "review-token".into();
    assert!(rt.state.agent.dialogue.poll(&snapshot));
    assert!(rt.state.agent.dialogue.answer.text.is_empty());
    rt.state.agent.dialogue.answer = TextEdit::new("previous");
    rt.state.agent.dialogue.error = Some("An old answer was refused".into());
    snapshot.window_question.as_mut().unwrap().task_id += 1;
    assert!(rt.state.agent.dialogue.poll(&snapshot));
    assert!(rt.state.agent.dialogue.answer.text.is_empty());
    assert!(rt.state.agent.dialogue.error.is_none());
}

/// Render the real task card; synthetic identities are never dispatched.
#[test]
#[ignore = "writes light/dark native window-question renders for visual inspection"]
fn render_native_window_question_light_and_dark() {
    render_pair("window-question", |dark| runtime(dark, true));
}

fn render_pair(name: &str, scene: impl Fn(bool) -> Runtime<Ui>) {
    let gpu = rune::render::Gpu::try_new(rune::render::Gpu::instance(), None)
        .expect("a GPU or software Vulkan adapter is needed to render native window questions");
    let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    for dark in [false, true] {
        let mut rt = scene(dark);
        let mut renderer = crate::rune_ui::offscreen::renderer(&gpu.device);
        let size = (SIZE.0 as u32, SIZE.1 as u32);
        let target = crate::rune_ui::offscreen::target(&gpu.device, size.0, size.1);
        renderer.prepare(
            &gpu.device,
            &gpu.queue,
            &mut rt.text.atlas,
            &rt.instances,
            size,
        );
        renderer.upload_images(&gpu.device, &gpu.queue, &mut rt.images);
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        rune::compose::Compositor::render(
            &mut crate::rune_ui::WindowCompositor::default(),
            &mut rune::compose::Frame {
                device: &gpu.device,
                queue: &gpu.queue,
                encoder: &mut encoder,
                target: &target.view,
                format: crate::rune_ui::offscreen::format(),
                size,
                dt: 0.0,
                update_ms: 0.0,
                rt: &mut rt,
                ui: &mut renderer,
            },
        );
        target.copy(&mut encoder);
        renderer.submit(&gpu.queue, encoder);
        let path = folder.join(format!(
            "rune-agent-{name}-{}.png",
            if dark { "dark" } else { "light" }
        ));
        rune::render::write_png(&path, size.0, size.1, &target.read(&gpu.device)).unwrap();
    }
}

fn permission_fixture() -> Ui {
    let mut ui = fixture(false);
    let snapshot = ui.agent.snapshot.as_mut().unwrap();
    snapshot.phase = Phase::Running;
    snapshot.window_question = None;
    let task = snapshot.task.as_mut().unwrap();
    task.phase = Phase::Running;
    task.mode = Mode::Work;
    task.text = "Review the exact queued test".into();
    task.steps[0].status = "waiting_for_permission".into();
    task.steps[0].title = "Test the committed workspace".into();
    task.steps[0].risk = "Runs Cargo tests in the owned checkout".into();
    snapshot.cancellable_coding_jobs = vec![0];
    snapshot
        .permission_waits
        .push(::x_agent::service::PermissionWait {
            step_index: 0,
            review_token: "64cb70a8-5a75-4faa-8efa-213d31f54f40".into(),
        });
    ui
}

fn permission_runtime(dark: bool) -> Runtime<Ui> {
    let mut rt = crate::rune_ui::offscreen::runtime(
        permission_fixture(),
        drawn,
        SIZE,
        Env::new(dark, 1.0),
        TextSystem::new(rune_theme_opencode::FONTS, rune_theme_opencode::FAMILIES),
    );
    rt.frame(0.0);
    rt
}

#[test]
fn exact_permission_renewal_is_reachable_by_keyboard_and_stale_reviews_have_no_button() {
    let mut rt = permission_runtime(false);
    assert!(
        rt.elements()
            .iter()
            .any(|el| el.path.contains("renew-job-42-0"))
    );
    let reached = (0..10).any(|_| {
        rt.key(KeyEvent {
            key: Key::Tab,
            mods: Mods::default(),
        });
        rt.frame(0.0);
        rt.elements()
            .iter()
            .any(|el| el.focused && el.path.contains("renew-job-42-0"))
    });
    assert!(reached);
    rt.update(|ui, _| ui.agent.snapshot.as_mut().unwrap().permission_waits.clear());
    for _ in 0..30 {
        rt.frame(1.0 / 60.0);
    }
    let elements = rt.elements();
    assert!(
        elements
            .iter()
            .all(|el| !el.path.contains("renew-job-42-0"))
    );
    assert!(
        elements
            .iter()
            .any(|el| el.path.contains("stop-coding-job-0"))
    );
}

#[test]
#[ignore = "writes light/dark native permission wait renders for visual inspection"]
fn render_native_permission_wait_light_and_dark() {
    render_pair("permission-wait", permission_runtime);
}

fn work_result_runtime(dark: bool, stale: bool, size: (f32, f32)) -> Runtime<Ui> {
    fn view(ui: &Ui, cx: &mut Cx<Ui>) -> El {
        let p = Paint::new(cx.system_dark(), cx.env().scale);
        col()
            .key("root")
            .w_full()
            .h_full()
            .padding(24.0)
            .fill(p.k.c.bg_base)
            .child(super::super::results::panel(p, &ui.agent.results))
    }
    let mut ui = fixture(false);
    ui.agent.results = super::super::results::fixture(stale);
    let mut rt = crate::rune_ui::offscreen::runtime(
        ui,
        view,
        size,
        Env::new(dark, 1.0),
        TextSystem::new(rune_theme_opencode::FONTS, rune_theme_opencode::FAMILIES),
    );
    rt.frame(0.0);
    rt
}

#[test]
fn native_work_result_reviews_exact_artifacts_with_narrow_keyboard_controls() {
    let mut rt = work_result_runtime(false, false, (410.0, 700.0));
    let reached = (0..12).any(|_| {
        rt.key(KeyEvent {
            key: Key::Tab,
            mods: Mods::default(),
        });
        rt.frame(0.0);
        rt.elements()
            .iter()
            .any(|el| el.focused && el.path.contains("work-result-integrate"))
    });
    assert!(reached);
    rt.update(|ui, _| ui.agent.results = super::super::results::fixture(true));
    for _ in 0..30 {
        rt.frame(1.0 / 60.0);
    }
    assert!(
        rt.elements()
            .iter()
            .all(|el| !el.path.contains("work-result-integrate"))
    );
    assert!(
        rt.elements()
            .iter()
            .any(|el| el.path.contains("work-result-open"))
    );
}

#[test]
#[ignore = "writes light/dark native Work result renders for visual inspection"]
fn render_native_work_results_light_and_dark() {
    render_pair("work-result", |dark| work_result_runtime(dark, false, SIZE));
    render_pair("work-result-stale", |dark| {
        work_result_runtime(dark, true, SIZE)
    });
}

fn run_activity_runtime(dark: bool) -> Runtime<Ui> {
    use ::x_agent::service::{GraphJobPhase, GraphJobView};

    let mut ui = permission_fixture();
    let snapshot = ui.agent.snapshot.as_mut().unwrap();
    snapshot.permission_waits.clear();
    snapshot.cancellable_coding_jobs = vec![0, 1, 2];
    let task = snapshot.task.as_mut().unwrap();
    task.text = "Review two independent changes and run the fixed suite.".into();
    let first = task.steps.remove(0);
    let run = "7e7b7f09-8fc7-48ac-a990-19ad8a69eee1".parse().unwrap();
    for (index, (name, phase, status, seconds, dependencies)) in [
        ("Catalog", GraphJobPhase::Running, "running", 28, Vec::new()),
        ("Index", GraphJobPhase::Preparing, "running", 12, Vec::new()),
        (
            "Tests",
            GraphJobPhase::Queued,
            "pending",
            28,
            vec!["Catalog".into(), "Index".into()],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        task.steps.push(Step {
            title: name.into(),
            status: status.into(),
            risk: String::new(),
            ..first.clone()
        });
        snapshot.graph_jobs.push(GraphJobView {
            run,
            index,
            node: Arc::from(name),
            dependencies: Arc::from(dependencies),
            phase,
            elapsed_seconds: seconds,
            has_checkout: false,
        });
    }
    let mut rt = crate::rune_ui::offscreen::runtime(
        ui,
        drawn,
        SIZE,
        Env::new(dark, 1.0),
        TextSystem::new(rune_theme_opencode::FONTS, rune_theme_opencode::FAMILIES),
    );
    rt.frame(0.0);
    rt
}

#[test]
fn native_run_activity_keeps_exact_checkout_links_and_scoped_stop() {
    let mut rt = run_activity_runtime(false);
    assert!(
        rt.elements()
            .iter()
            .any(|el| el.path.contains("stop-coding-job-1"))
    );
    assert!(
        rt.elements()
            .iter()
            .all(|el| !el.path.contains("inspect-job-result"))
    );
    rt.update(|ui, _| {
        let snapshot = ui.agent.snapshot.as_mut().unwrap();
        snapshot.graph_jobs[0].phase = ::x_agent::service::GraphJobPhase::Verified;
        snapshot.graph_jobs[0].has_checkout = true;
    });
    rt.frame(0.0);
    assert!(
        rt.elements()
            .iter()
            .any(|el| el.path.contains("inspect-job-result-42-0"))
    );
}

#[test]
#[ignore = "writes light/dark native run activity renders"]
fn render_native_run_activity_light_and_dark() {
    render_pair("run-activity", run_activity_runtime);
}

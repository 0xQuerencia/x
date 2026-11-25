use std::sync::Arc;

use rune::text::TextSystem;
use rune::{Cx, Env, Runtime};

use super::*;
use ::x_agent::service::Step;
use ::x_agent::tools::{EffectEvidence, ExecutionOutcome};

const SIZE: (f32, f32) = (600.0, 520.0);

fn fixture() -> Ui {
    let service = ::x_agent::service::Service::new(
        Arc::new(::x_agent::hexcli::HexCli::default()),
        Arc::new(::x_agent::service::RealLauncher),
        None,
    );
    let mut snapshot = service.snapshot(0);
    let step = |title: &str, status: &str, execution: ExecutionOutcome| Step {
        title: title.into(),
        status: status.into(),
        risk: String::new(),
        request: None,
        detail: execution.message.clone(),
        execution: Some(execution),
    };
    let task = Task {
        id: 1,
        mode: Mode::Actions,
        text: "Open the project documentation, copy its address, and read the page title so I can confirm that the right project opened.".into(),
        steps: vec![
            step("Copy reviewed project address", "done", ExecutionOutcome::confirmed(
                "The clipboard was read back and contains the reviewed address.",
                EffectEvidence::ClipboardReadback,
            )),
            step("Open documentation in the selected browser", "launched", ExecutionOutcome::launched(
                "The launch request was dispatched, but no native readback establishes which page the browser displayed. Check the selected browser before relying on the requested page being open.",
            )),
            step("Read the browser title", "launched", ExecutionOutcome::observed(
                "The visible title was read from accessibility. This observation does not confirm that the requested page navigation succeeded.",
                EffectEvidence::AccessibilityObservation,
            )),
        ],
        phase: Phase::Completed,
        error: None,
        summary: Some("1 action confirmed, 1 observed, 1 launched. Observations and launches do not independently verify the requested effect.".into()),
        changed_files: None,
        workspace: None,
        session: None,
        accepted_session: None,
        work_access: Default::default(),
        work_harness: None,
        blocked: Vec::new(),
        edit: None,
    };
    snapshot.phase = task.phase;
    snapshot.action_outcome = task.action_outcome();
    snapshot.task = Some(task);
    let mut ui = Ui::with(
        crate::app_settings::AppSettings::default(),
        crate::rune_ui::Service::Fixture(Vec::new()),
    );
    ui.agent.snapshot = Some(snapshot);
    ui
}

fn drawn(ui: &Ui, cx: &mut Cx<Ui>) -> El {
    let p = Paint::new(cx.system_dark(), cx.env().scale);
    let snapshot = ui.agent.snapshot.as_ref().unwrap();
    let task = snapshot.task.as_ref().unwrap();
    col()
        .key("root")
        .w_full()
        .h_full()
        .padding(24.0)
        .gap(12.0)
        .fill(p.k.c.bg_base)
        .child(
            text(task_label(task))
                .key("completion")
                .font(DESCRIPTION)
                .color(p.warning()),
        )
        .child(task_card(p, &ui.agent, snapshot, task))
}

fn runtime(dark: bool) -> Runtime<Ui> {
    let mut rt = crate::rune_ui::offscreen::runtime(
        fixture(),
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
fn evidence_stays_readable_in_a_narrow_task_card() {
    for dark in [false, true] {
        let rt = runtime(dark);
        let elements = rt.elements();
        for key in ["task-text", "step0", "step1", "step2", "task-summary"] {
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
        let text: Vec<_> = elements
            .iter()
            .filter_map(|element| element.text.as_deref())
            .collect();
        assert!(text.contains(&"Finished · some effects unverified"));
        assert!(
            text.iter()
                .any(|text| text.contains("Launched; effect unverified"))
        );
        assert!(
            text.iter()
                .any(|text| text.contains("Observed; goal unverified"))
        );
    }
}

/// This renders the production task card. It never dispatches a native action.
#[test]
#[ignore = "writes light/dark typed-outcome card renders for visual inspection"]
fn render_native_action_outcomes_light_and_dark() {
    let gpu = rune::render::Gpu::try_new(rune::render::Gpu::instance(), None)
        .expect("a GPU or software Vulkan adapter is needed to render native outcome cards");
    let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    for dark in [false, true] {
        let mut rt = runtime(dark);
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
            "rune-agent-action-outcomes-{}.png",
            if dark { "dark" } else { "light" }
        ));
        rune::render::write_png(&path, size.0, size.1, &target.read(&gpu.device)).unwrap();
    }
}

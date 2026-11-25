use std::sync::Arc;

use rune::text::TextSystem;
use rune::{Cx, Env, Ime, Key, KeyEvent, Mods, Runtime};

use super::*;

const SIZE: (f32, f32) = (900.0, 1500.0);

fn fixture() -> Ui {
    // These executable paths are labels for a render fixture. No provider is
    // probed or started, and no preview button is pressed by these UI tests.
    let service = ::x_agent::service::Service::new(
        Arc::new(::x_agent::hexcli::HexCli::default()),
        Arc::new(::x_agent::service::RealLauncher),
        Some("/fixture/opencode".into()),
    )
    .with_claude(Some("/fixture/claude".into()));
    let mut snapshot = service.snapshot(0);
    snapshot.workspace = Some("/home/person/Projects/Example".into());
    let mut ui = Ui::with(
        crate::app_settings::AppSettings::default(),
        crate::rune_ui::Service::Fixture(Vec::new()),
    );
    ui.agent.snapshot = Some(snapshot);
    let composer = &mut ui.agent.composer;
    composer.expanded = true;
    composer.project = Some(
        serde_json::from_value(serde_json::json!({
            "id": "c7371a26-c87f-4d9e-a2ba-873d7a2c40e3",
            "name": "Example app",
            "root": "/home/person/Projects/Example",
            "git_common_dir": "/home/person/Projects/Example/.git"
        }))
        .unwrap(),
    );
    composer.first.goal = TextEdit::new("Implement the account settings form.");
    composer.first.acceptance =
        TextEdit::new("Every field has a label and keyboard navigation works.");
    composer.second.goal = TextEdit::new("Implement the account settings endpoint.");
    composer.second.acceptance = TextEdit::new("Validate the request and return a useful error.");
    composer.test_suite = Some(1);
    ui
}

fn drawn(ui: &Ui, cx: &mut Cx<Ui>) -> El {
    let p = Paint::new(cx.system_dark(), cx.env().scale);
    col()
        .key("root")
        .w_full()
        .h_full()
        .padding(24.0)
        .fill(p.k.c.bg_base)
        .child(super::view(p, ui))
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

fn tap(rt: &mut Runtime<Ui>, key: &str) {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("missing {key}"));
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.settle_all();
    rt.frame(0.0);
}

#[test]
fn pointer_choices_build_a_typed_two_worker_request() {
    let mut rt = runtime(false);
    tap(&mut rt, "composer-worker-limit/segment0");
    tap(&mut rt, "composer-suite-4");
    let request = rt
        .state
        .agent
        .composer
        .request(rt.state.agent.snapshot.as_ref().unwrap())
        .unwrap();
    assert_eq!(request.worker_limit, 1);
    assert_eq!(
        request.test_suite,
        Some(::x_agent::verification::test_runner::Suite::Pytest)
    );
    assert_eq!(request.dependency, Dependency::Independent);
    tap(&mut rt, "composer-dependency-choice/segment1");
    let request = rt
        .state
        .agent
        .composer
        .request(rt.state.agent.snapshot.as_ref().unwrap())
        .unwrap();
    assert_eq!(request.dependency, Dependency::FirstArtifact);
    assert!(request.second.is_some());
    assert!(matches!(request.session, SessionChoice::Fresh));
}

#[test]
fn keyboard_can_reach_and_edit_the_goal() {
    let mut rt = runtime(true);
    let reached = (0..50).any(|_| {
        rt.key(KeyEvent {
            key: Key::Tab,
            mods: Mods::default(),
        });
        rt.frame(0.0);
        rt.elements()
            .iter()
            .any(|element| element.focused && element.path.contains("composer-goal-0/"))
    });
    assert!(reached, "the first goal must be reachable by Tab");
    rt.key(KeyEvent {
        key: Key::Command('a'),
        mods: Mods {
            cmd: true,
            ..Mods::default()
        },
    });
    rt.ime(Ime::Commit(
        "Implement an accessible preferences panel".into(),
    ));
    rt.frame(0.0);
    assert_eq!(
        rt.state.agent.composer.first.goal.text,
        "Implement an accessible preferences panel"
    );
}

#[test]
fn tool_free_provider_only_appears_for_single_analysis() {
    let mut rt = runtime(false);
    assert!(rt.find("composer-provider-0-ClaudeCode").is_none());
    tap(&mut rt, "composer-size/segment0");
    tap(&mut rt, "composer-provider-0-ClaudeCode");
    let request = rt
        .state
        .agent
        .composer
        .request(rt.state.agent.snapshot.as_ref().unwrap())
        .unwrap();
    assert_eq!(request.first.role, Role::Analysis);
    assert!(request.second.is_none());
    assert_eq!(request.test_suite, None);
    assert_eq!(request.worker_limit, 1);
}

/// Run explicitly when changing this form. A missing GPU is an error, so an
/// unavailable render cannot be reported as a successful visual check.
#[test]
#[ignore = "writes light/dark form renders for visual inspection"]
fn render_native_work_composer_light_and_dark() {
    let gpu = rune::render::Gpu::try_new(rune::render::Gpu::instance(), None)
        .expect("a GPU or software Vulkan adapter is needed to render the native form");
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
            "rune-work-composer-{}.png",
            if dark { "dark" } else { "light" }
        ));
        rune::render::write_png(&path, size.0, size.1, &target.read(&gpu.device)).unwrap();
    }
}

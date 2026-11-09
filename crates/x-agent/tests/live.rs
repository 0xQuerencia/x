//! Live checks against the real desktop. They need a running Hyprland session, AT-SPI,
//! python-gobject with GTK 3, and they briefly open a test window and a terminal, so they are
//! ignored by default:
//!
//!     cargo test -p x-agent --test live -- --ignored --test-threads=1 --nocapture

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use x_agent::context::commands;
use x_agent::context::{ContextError, Engine};
use x_agent::edit::Editor;
use x_agent::edit::commands::Scope;
use x_agent::tools::Action;
use x_agent::wm;
use x_agent::workflows::model::{OnFailure, Step, StepAction, Workflow};
use x_agent::workflows::system::SystemHost;
use x_agent::workflows::{Config, RunState, StepStatus, run, undo};

const ERROR: &str = "TypeError: Cannot read properties of undefined (reading 'map')";

const GTK_APP: &str = r#"
import sys, gi
gi.require_version("Gtk", "3.0")
from gi.repository import Gtk, GLib
mode = sys.argv[1]
w = Gtk.Window(title="x-live-" + mode); w.set_default_size(420, 80)
e = Gtk.Entry()
if mode == "password":
    e.set_visibility(False); e.set_text("hunter2-secret")
else:
    e.set_text(sys.argv[2])
w.add(e); w.show_all(); e.grab_focus(); e.select_region(0, -1)
def done():
    print(e.get_text(), flush=True); Gtk.main_quit()
GLib.timeout_add_seconds(int(sys.argv[3]), done); Gtk.main()
"#;

struct TestApp {
    child: Child,
    title: String,
}

impl TestApp {
    fn start(mode: &str, text: &str, seconds: u32) -> Self {
        let child = Command::new("python3")
            .args(["-c", GTK_APP, mode, text, &seconds.to_string()])
            .stdout(Stdio::piped())
            .spawn()
            .expect("python3 with GTK 3 is needed");
        Self {
            child,
            title: format!("x-live-{mode}"),
        }
    }

    fn window(&self) -> wm::WinInfo {
        let end = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(w) = wm::view()
                .unwrap()
                .windows
                .into_iter()
                .find(|w| w.title == self.title)
            {
                return w;
            }
            assert!(Instant::now() < end, "the test window never appeared");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn focus(&self) {
        let w = self.window();
        wm::focus_exact(&w.id, w.pid, &w.class).expect("focus the test window");
        std::thread::sleep(Duration::from_millis(900)); // let GTK publish its focus
    }

    fn finish(mut self) -> String {
        let mut out = String::new();
        if let Some(mut so) = self.child.stdout.take() {
            std::io::Read::read_to_string(&mut so, &mut out).ok();
        }
        let _ = self.child.wait();
        out.trim().to_string()
    }
}

fn restore(prev: Option<wm::WinInfo>) {
    if let Some(p) = prev {
        let _ = wm::focus_exact(&p.id, p.pid, &p.class);
    }
}

#[test]
#[ignore = "needs a live Hyprland + AT-SPI session"]
fn selection_context_search_and_verified_edit_on_a_real_gtk_field() {
    let prev = wm::view().unwrap().focused;
    let app = TestApp::start("text", ERROR, 14);
    let engine = Engine::system();

    // Context: the focused window and the real selection, through the real helper. The
    // person is using this desktop too, so focus is retried if it drifts away.
    let mut ctx = None;
    for _ in 0..8 {
        app.focus();
        let started = Instant::now();
        let got = engine.capture(true);
        println!("context captured in {:?}", started.elapsed());
        if got
            .window
            .as_ref()
            .is_some_and(|w| w.value.title == "x-live-text")
        {
            ctx = Some(got);
            break;
        }
    }
    let ctx = ctx.expect("the test window could not be kept focused");
    assert_eq!(ctx.window.as_ref().unwrap().value.title, "x-live-text");
    assert_eq!(ctx.selected_text().unwrap(), ERROR);

    // "X, search this on GitHub" resolves to the exact selected error.
    let resolved =
        commands::resolve(&commands::parse("search this on github").unwrap(), &ctx).unwrap();
    let Action::Search { target, query, .. } = &resolved.actions[0] else {
        panic!()
    };
    assert_eq!((target.as_str(), query.as_str()), ("github", ERROR));
    let argv = resolved.actions[0]
        .argv(&|n| Some(format!("/usr/bin/{n}")))
        .unwrap();
    assert!(argv[1].starts_with("https://github.com/search?q=TypeError%3A%20Cannot"));

    // A verified replacement through accessibility, never keystrokes.
    let editor = Editor::default();
    let mut target = None;
    for _ in 0..8 {
        app.focus();
        if let Ok(t) = editor.resolve(&engine, Scope::Selection) {
            target = Some(t);
            break;
        }
    }
    let target = target.expect("the selection could not be read");
    assert_eq!(target.text, ERROR);
    editor.apply(&engine, &target, "TypeError on map").unwrap();
    // The same text must be gone if someone changed it first: a stale target is refused.
    let stale = editor.apply(&engine, &target, "should not land");
    assert!(stale.is_err(), "{stale:?}");

    // Undo puts the original back, verified.
    app.focus();
    editor.undo(&engine).unwrap();
    restore(prev);
    assert_eq!(
        app.finish(),
        ERROR,
        "the app's own text is back to the original"
    );
}

#[test]
#[ignore = "needs a live Hyprland + AT-SPI session"]
fn a_real_password_field_is_never_read() {
    let prev = wm::view().unwrap().focused;
    let app = TestApp::start("password", "", 10);
    let engine = Engine::system();
    let mut err = None;
    for _ in 0..8 {
        app.focus();
        let ctx = engine.capture(true);
        if ctx
            .window
            .as_ref()
            .is_some_and(|w| w.value.title == "x-live-password")
        {
            err = Some(ctx.selected_text().unwrap_err());
            break;
        }
    }
    let err = err.expect("the test window could not be kept focused");
    restore(prev);
    app.finish();
    assert_eq!(err, ContextError::Sensitive);
}

#[test]
#[ignore = "opens a terminal on the live desktop and moves it to workspace 10"]
fn a_real_workflow_opens_places_verifies_and_undoes() {
    let home = dirs::home_dir().unwrap();
    let dir = home.join("x-live-workflow-test");
    std::fs::create_dir_all(&dir).unwrap();
    let wf = Workflow {
        id: "live".into(),
        name: "Live test".into(),
        triggers: vec![],
        steps: vec![Step {
            id: "terminal".into(),
            action: StepAction::OpenTerminal {
                directory: "~/x-live-workflow-test".into(),
                workspace: Some(10),
            },
            requires: vec![],
            timeout_secs: Some(15),
            on_failure: OnFailure::Stop,
        }],
    };
    wf.validate(&home).unwrap();
    let cfg = Config {
        poll: Duration::from_millis(150),
        home,
    };
    let before = wm::view().unwrap();
    let out = run(&wf, &SystemHost, &cfg);
    println!("{}", out.report.summary);
    for s in &out.report.steps {
        println!("  {} {:?}: {}", s.id, s.status, s.detail);
    }
    let after = wm::view().unwrap();
    let opened: Vec<_> = after
        .windows
        .iter()
        .filter(|w| !before.windows.iter().any(|b| b.id == w.id))
        .collect();
    let (done, problems) = undo(&out.reversals, &SystemHost);
    println!("undo: {done} done, problems: {problems:?}");
    let _ = std::fs::remove_dir(dir);
    assert_eq!(
        out.report.state,
        RunState::Completed,
        "{}",
        out.report.summary
    );
    assert_eq!(out.report.steps[0].status, StepStatus::Done);
    assert_eq!(opened.len(), 1, "exactly one new window");
    assert_eq!(
        opened[0].workspace, 10,
        "the window really is on workspace 10"
    );
    assert!(problems.is_empty(), "{problems:?}");
    let final_view = wm::view().unwrap();
    assert!(
        !final_view.windows.iter().any(|w| w.id == opened[0].id),
        "undo closed it again"
    );
}

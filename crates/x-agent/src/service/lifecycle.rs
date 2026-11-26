//! Task completion, scoped cancellation and voice lifetime.

use super::*;

impl Service {
    /// Every task worker owns its lease until it returns or unwinds. A panic
    /// cannot strand an active task or leave a queued grant usable.
    pub(super) fn spawn_owned(
        &self,
        task_id: u64,
        lease: coordinator::Lease,
        run: impl FnOnce(Service) + Send + 'static,
    ) -> Result<(), String> {
        let service = self.clone();
        thread::Builder::new()
            .name(format!("x-task-{task_id}"))
            .spawn(move || {
                let _lease = lease;
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(service.clone())))
                    .is_err()
                {
                    service.fail_owned(task_id,
                        "The owned task worker stopped unexpectedly; review its effects before retrying".into());
                }
            })
            .map(|_| ())
            .map_err(|error| {
                let message = format!("Could not start the owned task worker: {error}");
                self.fail_owned(task_id, message.clone());
                message
            })
    }

    fn fail_owned(&self, task_id: u64, message: String) {
        let state = self.lock();
        // Completion may already have released the foreground for another task.
        // Never let an old worker's unwind alter that newer task or its grants.
        if !state.phase.is_busy() || state.task.as_ref().is_none_or(|task| task.id != task_id) {
            return;
        }
        state.stop.request();
        let _ = self.scoped_grants.revoke_all();
        finish_locked(state, Phase::Failed, Some(message), None, None);
    }

    pub(super) fn set_step(&self, idx: usize, status: &str) {
        let mut s = self.lock();
        let Some(step) = s.task.as_mut().and_then(|t| t.steps.get_mut(idx)) else {
            return;
        };
        if step.status == status
            || (step.status == "stopping" && matches!(status, "running" | "waiting_for_permission"))
        {
            return;
        }
        step.status = status.into();
        let kind = match status {
            "done" => AuditKind::StepDone,
            "launched" => AuditKind::Launched,
            "failed" => AuditKind::StepFailed,
            _ => return,
        };
        audit_best_effort(&mut s, kind, Some(idx));
    }

    pub(super) fn finish(
        &self,
        phase: Phase,
        error: Option<String>,
        summary: Option<String>,
        changed: Option<Vec<String>>,
    ) {
        finish_locked(self.lock(), phase, error, summary, changed);
    }

    /// Emergency Stop: discard foreground work and request every owned run to stop.
    pub fn cancel(&self) {
        self.cancel_foreground();
        self.cancel_background_runs();
    }

    /// Stop only the foreground plan, recording or owned runner.
    pub fn cancel_foreground(&self) {
        let _ = self.cancel_foreground_matching(None);
    }

    /// Stop only the displayed foreground task. A stale card or CLI response
    /// cannot revoke a replacement task's approvals or cancel a new recording.
    pub fn cancel_foreground_task(&self, task_id: u64) -> Result<(), String> {
        self.cancel_foreground_matching(Some(task_id))
    }

    fn cancel_foreground_matching(&self, task_id: Option<u64>) -> Result<(), String> {
        let _voice_control = self.lock_voice_control();
        let mut s = self.lock();
        if let Some(id) = task_id
            && (s.task.as_ref().is_none_or(|task| task.id != id)
                || !matches!(
                    s.phase,
                    Phase::AwaitingApproval | Phase::WaitingForInput | Phase::Running
                ))
        {
            return Err("That task is no longer the active foreground task".into());
        }
        if let Err(error) = self.scoped_grants().revoke_all() {
            emit(
                &mut s,
                "error",
                &format!("Could not revoke pending job approvals: {error}"),
            );
        }
        if s.model_stop.as_ref().is_some_and(Cancellation::request) {
            emit(&mut s, "info", "Stop requested for the pending plan");
        }
        match s.phase {
            Phase::AwaitingApproval | Phase::WaitingForInput => {
                s.stop.request();
                let id = s.task.as_ref().map(|task| task.id).unwrap_or_default();
                set_phase(&mut s, Phase::Cancelled);
                s.edit = None;
                s.computer_dialogue = None;
                s.window_dialogue = None;
                s.workflow = None;
                s.graph_workflow = None;
                s.active_graph_stops = None;
                s.pending_tests = None;
                s.pending_integration = None;
                s.project_command = None;
                s.actions.clear();
                s.guard = None;
                s.grants.clear();
                s.create_workspace = false;
                if let Some(t) = s.task.as_mut() {
                    t.edit = None;
                }
                audit_best_effort(&mut s, AuditKind::Discarded, None);
                emit(&mut s, "info", "Plan discarded");
                emit(&mut s, "approval", &format!("discarded task {id}"));
            }
            Phase::Running => {
                if s.stop.request() {
                    audit_best_effort(&mut s, AuditKind::StopRequested, None);
                    emit(&mut s, "info", "Stop requested");
                }
            }
            Phase::Listening | Phase::Transcribing => {
                self.discard_voice(s);
            }
            _ => {}
        }
        Ok(())
    }

    // ---- voice -------------------------------------------------------------------------

    fn lock_voice_control(&self) -> MutexGuard<'_, ()> {
        self.voice_control.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn voice_start(&self, mode: Mode) -> Result<(), String> {
        let _voice_control = self.lock_voice_control();
        {
            let mut s = self.lock();
            if s.phase.is_busy() {
                return Err(busy(s.phase));
            }
            if s.voice_draining {
                return Err("The previous recording is still stopping".into());
            }
            s.voice_mode = mode;
            set_phase(&mut s, Phase::Listening);
        }
        if let Err(e) = self.stt.start() {
            set_phase(&mut self.lock(), Phase::Idle);
            return Err(e.to_string());
        }
        self.emit("info", "Listening (Hex microphone)");
        Ok(())
    }

    pub fn voice_stop(&self) -> Result<(), String> {
        let _voice_control = self.lock_voice_control();
        let mode = {
            let mut s = self.lock();
            if s.phase != Phase::Listening {
                return Err("Not listening".into());
            }
            set_phase(&mut s, Phase::Transcribing);
            s.voice_draining = true;
            s.voice_mode
        };
        let me = self.clone();
        if let Err(error) = thread::Builder::new()
            .name("x-transcribe".into())
            .spawn(move || me.transcribe(mode))
        {
            let mut s = self.lock();
            s.voice_draining = false;
            self.discard_voice(s);
            return Err(format!("Could not finish the recording: {error}"));
        }
        Ok(())
    }

    fn transcribe(&self, mode: Mode) {
        let result = self.stt.stop_print();
        {
            let mut s = self.lock();
            s.voice_draining = false;
            if s.phase != Phase::Transcribing {
                return; // Only this worker can release its stop request.
            }
            match &result {
                Ok(_) => emit(&mut s, "heard", "Finished local transcription"),
                Err(SttError::NoSpeech) => emit(&mut s, "info", "No speech heard"),
                Err(_) => emit(&mut s, "error", "Local transcription failed"),
            }
            set_phase(&mut s, Phase::Idle);
        }
        if let Ok(text) = result
            && let Err(error) = self.plan_spoken(&text, mode)
        {
            self.emit("error", &error);
        }
    }

    // Called with voice control held, so the CLI cannot cancel a newer capture.
    fn discard_voice(&self, mut s: MutexGuard<'_, State>) {
        set_phase(&mut s, Phase::Idle);
        drop(s);
        match self.stt.cancel() {
            Ok(()) => self.emit("info", "Recording discarded"),
            Err(_) => self.emit("error", "Could not cancel recording"),
        }
    }

    /// App exit: stop any run and drop any recording. Blocks until the run is reaped.
    pub fn shutdown(&self) {
        self.forget_edits();
        self.cancel();
        for _ in 0..200 {
            if {
                let state = self.lock();
                !state.phase.is_busy() && !state.voice_draining
            } && self.workers_finished()
            {
                return;
            }
            thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

fn finish_locked(
    mut s: MutexGuard<'_, State>,
    phase: Phase,
    error: Option<String>,
    summary: Option<String>,
    changed: Option<Vec<String>>,
) {
    let phase = if s.stop.requested() && phase == Phase::Completed {
        Phase::Cancelled
    } else {
        phase
    };
    if let Some(t) = s.task.as_mut() {
        t.error = error.clone();
        t.summary = summary;
        t.changed_files = changed;
        t.edit = None;
    }
    s.edit = None;
    s.computer_dialogue = None;
    s.window_dialogue = None;
    s.workflow = None;
    s.graph_workflow = None;
    s.active_graph_stops = None;
    s.pending_tests = None;
    s.pending_integration = None;
    s.project_command = None;
    set_phase(&mut s, phase);
    audit_best_effort(&mut s, AuditKind::Finished, None);
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

//! The owned Work runner and its result projection.

use super::*;

impl Service {
    pub(super) fn run_work(&self, task: &Task, stop: &Cancellation) {
        let kind = task.work_harness.unwrap_or_default();
        let (Some(adapter), Some(ws)) = (self.work_adapter(kind), task.workspace.clone()) else {
            return self.finish(
                Phase::Failed,
                Some(format!("{} or workspace missing", kind.label())),
                None,
                None,
            );
        };
        let create_workspace = {
            let mut state = self.lock();
            if stop.requested() {
                drop(state);
                return self.finish(
                    Phase::Cancelled,
                    None,
                    Some("Stopped before start".into()),
                    None,
                );
            }
            std::mem::take(&mut state.create_workspace)
        };
        let ws = std::path::Path::new(&ws);
        if create_workspace {
            let created = ws
                .parent()
                .ok_or_else(|| std::io::Error::other("Missing session root"))
                .and_then(std::fs::create_dir_all)
                .and_then(|()| std::fs::create_dir(ws));
            if let Err(error) = created {
                self.set_step(0, "failed");
                return self.finish(
                    Phase::Failed,
                    Some(format!("Could not create session folder: {error}")),
                    None,
                    None,
                );
            }
        }
        let Ok(ws) = opencode::validate_workspace(ws) else {
            self.set_step(0, "failed");
            return self.finish(
                Phase::Failed,
                Some("Workspace is no longer valid".into()),
                None,
                None,
            );
        };
        if kind == harness::Kind::OpenCode
            && let Some(session) = &task.session
        {
            let verification = self
                .session_verifier
                .as_ref()
                .ok_or_else(|| "OpenCode session verification is unavailable".to_string())
                .and_then(|verify| verify(session));
            if stop.requested() {
                return self.finish(
                    Phase::Cancelled,
                    None,
                    Some("Stopped before send".into()),
                    None,
                );
            }
            if let Err(error) = verification {
                self.set_step(0, "failed");
                return self.finish(Phase::Failed, Some(error), None, None);
            }
        }
        if let Err(why) = self.recheck_guard() {
            self.set_step(0, "failed");
            return self.finish(Phase::Failed, Some(format!("Stopped: {why}")), None, None);
        }
        if create_workspace {
            let mut s = self.lock();
            s.workspace = Some(ws.clone());
            s.selected_session = None;
            s.session_list = None;
            s.claude_session = None;
        }
        self.set_step(0, "running");
        let me = self.clone();
        let expected = task.session.as_ref().map(|session| session.id.clone());
        let active_task_id = task.id;
        let on_event: harness::EventSink = Arc::new(move |event| {
            let mut state = me.lock();
            let Some(task) = state.task.as_mut().filter(|task| task.id == active_task_id) else {
                return;
            };
            if task.accepted_session.is_none()
                && let Some(id) = event.accepted_session().or(
                    if kind == harness::Kind::ClaudeCode
                        && event.kind == harness::EventKind::StepFinish
                    {
                        event.session.as_ref()
                    } else {
                        None
                    },
                )
                && expected.as_ref().is_none_or(|wanted| wanted == id)
            {
                task.accepted_session = Some(id.clone());
                emit(
                    &mut state,
                    "delivery",
                    &format!("{} acknowledged session {}", kind.label(), id.as_str()),
                );
            }
            if let Some(command) = event.blocked_command
                && let Some(task) = state.task.as_mut()
            {
                task.blocked.push(command);
            }
            emit(&mut state, event.kind.as_str(), &event.text);
        });
        let request = harness::Request {
            workspace: ws.clone(),
            prompt: task.text.clone(),
            session: task.session.as_ref().map(|session| session.id.clone()),
            access: task.work_access,
            allow_commands: std::mem::take(&mut self.lock().grants),
        };
        let run = match adapter.start(request, on_event, stop) {
            Ok(r) => r,
            Err(_) if stop.requested() => {
                return self.finish(
                    Phase::Cancelled,
                    None,
                    Some("Stopped before start".into()),
                    None,
                );
            }
            Err(e) => {
                self.set_step(0, "failed");
                return self.finish(Phase::Failed, Some(e), None, None);
            }
        };
        let res = run.join();
        if let Some(id) = res.accepted_session.as_ref().filter(|id| id.kind() == kind) {
            self.record_provider_session(&harness::Session {
                id: id.clone(),
                title: String::new(),
                workspace: ws.clone(),
                updated_at: now(),
            });
        }
        if kind == harness::Kind::ClaudeCode
            && res.status == RunStatus::Completed
            && !stop.requested()
            && let Some(id) = res.accepted_session.as_ref().filter(|id| id.kind() == kind)
        {
            self.lock().claude_session = Some(harness::Session {
                id: id.clone(),
                title: "Claude Code task".into(),
                workspace: ws,
                updated_at: now(),
            });
        }

        let ok = res.status == RunStatus::Completed;
        self.set_step(1, if ok { "done" } else { "failed" });
        self.set_step(2, if ok { "done" } else { "failed" });
        let closure = res.termination.map(|evidence| evidence.summary());
        match res.status {
            RunStatus::Cancelled => self.finish(
                Phase::Cancelled,
                None,
                Some(format!(
                    "Stopped. {}Changes may already exist on disk; nothing was rolled back.",
                    closure.map_or(String::new(), |proof| format!("{proof} ")),
                )),
                res.changed_files,
            ),
            RunStatus::Completed => self.finish(
                Phase::Completed,
                None,
                Some(format!(
                    "{} exited 0 with no error events.{}",
                    kind.label(),
                    closure.map_or(String::new(), |proof| format!(" {proof}")),
                )),
                res.changed_files,
            ),
            RunStatus::Failed => {
                let why = if res.errors.is_empty() {
                    format!(
                        "{} exited with status {}",
                        kind.label(),
                        res.exit_code.map_or("unknown".into(), |c| c.to_string())
                    )
                } else {
                    res.errors.join("; ")
                };
                self.finish(
                    Phase::Failed,
                    Some(why),
                    closure.map(str::to_owned),
                    res.changed_files,
                )
            }
        }
    }
}

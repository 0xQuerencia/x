//! Deterministic routing and reviewable task construction.

use super::*;

impl Service {
    /// "search this on GitHub": resolve the reference against what is on screen right
    /// now, then plan the resulting action like any other. The plan shows the exact text
    /// that will be used, so approving it is approving that text.
    pub(super) fn plan_contextual(
        &self,
        text: &str,
        cmd: &context::commands::Command,
        source: InputSource,
        voice_route: Option<&Cancellation>,
    ) -> Result<Task, String> {
        {
            let s = self.lock();
            if s.phase.is_busy() {
                return Err(busy(s.phase));
            }
        }
        let engine = self.context_engine();
        let ctx = engine.capture(cmd.needs_selection());
        for note in &ctx.notes {
            self.emit("info", note);
        }
        let resolved = context::commands::resolve(cmd, &ctx).map_err(|e| e.to_string())?;
        for note in &resolved.notes {
            self.emit("info", note);
        }
        // A guessed primary selection or data sent off-device needs explicit review.
        let private_context = ctx.selection.source == context::Source::PrimarySelection
            || matches!(
                cmd,
                context::commands::Command::Search { .. } | context::commands::Command::OpenLink
            );
        let task = self.plan_task(
            text,
            Mode::Actions,
            Some(resolved.actions),
            PlanOptions {
                source,
                guard: Some(Guard {
                    ctx,
                    strict_focus: false,
                    browser_share: false,
                }),
                require_review: private_context,
                voice_route: voice_route.cloned(),
                ..PlanOptions::default()
            },
        )?;
        self.approve_if_automatic(task.id)?;
        Ok(task)
    }

    /// Apps the person lets X read and operate. Anything else asks first, every time.
    pub fn set_computer_apps(&self, apps: Vec<String>) {
        {
            let mut state = self.lock();
            let revoked = state.actions.iter().any(|request| {
                let Action::Computer { app, .. } = &request.action else {
                    return false;
                };
                let id = request.metadata.idempotency_key;
                state.computer_apps.contains(app)
                    && !apps.contains(app)
                    && state.task.as_ref().is_some_and(|task| {
                        task.id == id.task
                            && task.steps.get(id.step).is_some_and(|step| {
                                matches!(
                                    step.status.as_str(),
                                    "pending" | "running" | "waiting_input" | "waiting_review"
                                )
                            })
                    })
            });
            state.computer_apps.clone_from(&apps);
            if state.phase.is_busy() && revoked {
                super::lifecycle::cancel_task_locked(&mut state);
                emit(&mut state, "info", "Computer app grant revoked");
            }
        }
        self.sync_background_computer_policy(Some(&apps), None);
    }

    /// Whether on-screen text may be sent to a model that is not on this machine.
    pub fn set_computer_remote(&self, on: bool) {
        self.lock().computer_remote = on;
        self.sync_background_computer_policy(None, Some(on));
    }

    pub fn set_planner(&self, planner: Option<Arc<dyn Planner>>) {
        if let Ok(mut slot) = self.planner.lock() {
            *slot = planner;
        }
    }

    /// Rules first: they are instant and never wrong about what they accept. Only speech
    /// they do not understand goes to the model, outside the state lock.
    pub(super) fn model_plan(
        &self,
        text: &str,
        smart: bool,
        voice_route: Option<&Cancellation>,
    ) -> Result<Option<(Vec<Action>, ModelPlanning)>, String> {
        let Some(planner) = self.planner.lock().ok().and_then(|p| p.clone()) else {
            return Ok(None);
        };
        let stop = Cancellation::default();
        {
            let mut state = self.lock();
            if state.phase.is_busy() || state.model_stop.is_some() {
                return Ok(None);
            }
            if let Some(route) = voice_route {
                route.check()?;
            }
            state.model_stop = Some(stop.clone());
        }
        let planning = ModelPlanning {
            stop,
            service: self.clone(),
        };
        self.emit("info", &format!("Asking {} to plan", planner.name()));
        let result = planner.plan(text, smart);
        planning.stop.check()?;
        match result {
            Ok(tools::Reply::Steps(actions)) if !actions.is_empty() => {
                Ok(Some((actions, planning)))
            }
            Ok(tools::Reply::Dictation) => Err(tools::NOT_A_COMMAND.into()),
            Ok(_) => Err(format!("X can't do that yet: {}", text.trim())),
            Err(error) => {
                self.emit("error", &format!("Planner: {error}"));
                Err(format!("Planner failed: {error}"))
            }
        }
    }

    /// True when this request would go to the model (the rules do not know it).
    pub fn needs_model(&self, text: &str) -> bool {
        if mixed_proposal::recognizes(text)
            || project_commands::parse(text).is_some()
            || session_targets::spoken_target(text).is_some()
            || session_targets::spoken_work(text).is_some()
        {
            return false;
        }
        let spoken_edit =
            self.is_edit_command(text) || context::commands::parse_utterance(text).is_some();
        spoken_edit
            || tools::plan_rules(text).is_none() && self.planner.lock().is_ok_and(|p| p.is_some())
    }

    /// Recognize explicit Stop requests locally before invoking a planner.
    pub fn request_stop_from_text(&self, text: &str) -> bool {
        let words = tools::strip_wake_word(text);
        let command = words
            .trim_end_matches(['.', '!', '?'])
            .trim()
            .to_ascii_lowercase();
        if !matches!(
            command.as_str(),
            "stop" | "stop task" | "stop the task" | "stop all" | "stop all tasks"
        ) {
            return false;
        }
        let active = {
            let state = self.lock();
            state.phase.is_busy() || state.model_stop.is_some()
        } || self.background_busy();
        if active {
            self.cancel();
        }
        active
    }

    pub fn plan(&self, text: &str, mode: Mode) -> Result<Task, String> {
        self.plan_with(text, mode, false, InputSource::Text, None)
    }

    /// Voice recorded by an explicit push-to-talk gesture.
    pub fn plan_spoken(&self, text: &str, mode: Mode) -> Result<Task, String> {
        self.plan_with(text, mode, false, InputSource::PushToTalk, None)
    }

    /// Ambient audio after the wake word is never treated like push-to-talk.
    pub fn plan_hands_free(&self, text: &str, mode: Mode) -> Result<Task, String> {
        self.plan_with(text, mode, false, InputSource::HandsFree, None)
    }

    /// Wake-free audio can propose one local desktop action for owner review.
    /// It cannot use model routing, workflow triggers, or automatic approval.
    pub fn preview_ambient_command(&self, text: &str) -> Result<Option<Task>, String> {
        let Some(actions) = tools::plan_rules(text) else {
            return Ok(None);
        };
        if !matches!(
            actions.as_slice(),
            [Action::App { .. }
                | Action::Window {
                    op: crate::wm::Op::Focus
                        | crate::wm::Op::Back
                        | crate::wm::Op::Workspace
                        | crate::wm::Op::Maximize
                        | crate::wm::Op::Minimize,
                    ..
                }]
        ) {
            return Ok(None);
        }
        self.plan_task(
            text,
            Mode::Actions,
            Some(actions),
            PlanOptions {
                origin: Origin::Rules,
                source: InputSource::HandsFree,
                require_review: true,
                ..PlanOptions::default()
            },
        )
        .map(Some)
    }

    /// Like `plan`, but the model also decides whether the speech was a command at all;
    /// `Err(NOT_A_COMMAND)` means it was dictation and nothing was planned.
    pub fn plan_smart(&self, text: &str) -> Result<Task, String> {
        self.plan_with(text, Mode::Actions, true, InputSource::PushToTalk, None)
    }

    pub(super) fn plan_with(
        &self,
        text: &str,
        mode: Mode,
        smart: bool,
        source: InputSource,
        voice_route: Option<&Cancellation>,
    ) -> Result<Task, String> {
        if let Some(route) = voice_route {
            route.check()?;
        }
        if self.request_stop_from_text(text) {
            return self
                .lock()
                .task
                .clone()
                .ok_or_else(|| "Stop requested".into());
        }
        if mode == Mode::Actions
            && let Some(result) = self.answer_current_window_reference(text)
        {
            return result;
        }
        if mode == Mode::Actions
            && let Some(command) = project_commands::parse(text)
        {
            return self.plan_project_command(command);
        }
        if mode == Mode::Actions && mixed_proposal::recognizes(text) {
            return self.plan_mixed_proposal(text);
        }
        let planning_started = Instant::now();
        if mode == Mode::Actions
            && let Some((work_harness, goal)) = session_targets::spoken_work(text)
        {
            let session = if work_harness == harness::Kind::OpenCode {
                self.lock().selected_session.clone()
            } else {
                None
            };
            return self.plan_task(
                &goal,
                Mode::Work,
                None,
                PlanOptions {
                    source,
                    session,
                    work_harness,
                    voice_route: voice_route.cloned(),
                    ..PlanOptions::default()
                },
            );
        }
        if mode == Mode::Actions
            && let Some(selector) = session_targets::spoken_target(text)
        {
            return self.plan_spoken_session(&selector, source);
        }
        if mode == Mode::Actions {
            if let Some(cmd) = context::commands::parse_utterance(text) {
                if cmd == context::commands::Command::Explain {
                    use crate::edit::commands::{Command, Scope, Transform};
                    let explain = Command::Transform(Transform::Explain, Scope::Selection);
                    return self.plan_edit(text.trim(), explain);
                }
                return self.plan_contextual(text.trim(), &cmd, source, voice_route);
            }
            if let Some(cmd) = crate::edit::commands::parse(&tools::strip_wake_word(text)) {
                use crate::edit::commands::Command;
                let reply = matches!(
                    cmd,
                    Command::Accept | Command::Retry | Command::Cancel | Command::SendToModel
                );
                let waiting = {
                    let s = self.lock();
                    s.phase == Phase::AwaitingApproval && s.edit.is_some()
                };
                // Answers like "retry" only mean something while a preview waits;
                // otherwise they are ordinary speech.
                if !reply || waiting {
                    return self.plan_edit(text.trim(), cmd);
                }
            }
        }
        if mode == Mode::Actions && workflow::is_undo_phrase(text) {
            self.begin_instant(text.trim())?;
            return self.end_instant(self.undo_workflow_inner());
        }
        if mode == Mode::Actions
            && tools::plan_rules(text).is_none()
            && let Some(found) = self.match_workflow(text)
        {
            return self.plan_workflow_from(text.trim(), found?, source, voice_route);
        }
        let mut preset = None;
        let mut model_plan = None;
        let mut origin = Origin::Quick;
        if mode == Mode::Actions {
            preset = tools::plan_rules(text);
            origin = if preset.is_some() {
                Origin::Rules
            } else {
                Origin::Decider
            };
            if preset.is_none()
                && let Some((actions, planning)) =
                    self.model_plan(text.trim(), smart, voice_route)?
            {
                preset = Some(actions);
                model_plan = Some(planning);
            }
            if preset.is_none() && smart {
                return Err(tools::NOT_A_COMMAND.into());
            }
        }
        let options = PlanOptions {
            origin,
            source,
            model_plan,
            voice_route: voice_route.cloned(),
            ..PlanOptions::default()
        };
        if let Some([Action::Session { goal }]) = preset.as_deref() {
            return self.plan_session(goal, options);
        }
        let task = self.plan_task(text, mode, preset, options)?;
        self.emit(
            "timing",
            &format!(
                "phase=plan elapsed_ms={}",
                planning_started.elapsed().as_millis()
            ),
        );
        self.approve_if_automatic(task.id)?;
        Ok(task)
    }

    pub(super) fn check_screen_may_leave(&self) -> Result<(), String> {
        let planner = self.planner.lock().ok().and_then(|p| p.clone());
        match planner {
            None => Err("Operating apps needs a model: set one with `agent planner`".into()),
            Some(p) if !p.is_local() && !self.lock().computer_remote => Err(format!(
                "Operating an app sends on-screen text to {}. Use a local model, or allow it with `agent computer remote on`. Nothing was done.",
                p.name()
            )),
            Some(_) => Ok(()),
        }
    }

    /// Revalidate the exact model selected for a running computer task.
    pub(super) fn screen_decision_permission(
        &self,
        planner: &Arc<dyn Planner>,
        app: &str,
    ) -> Result<bool, String> {
        let current = self.planner.lock().ok().and_then(|p| p.clone());
        if !current.as_ref().is_some_and(|p| Arc::ptr_eq(p, planner)) {
            return Err("The selected model changed during computer use".into());
        }
        let state = self.lock();
        state.stop.check()?;
        if !planner.is_local() && !state.computer_remote {
            return Err("Remote computer consent was revoked".into());
        }
        Ok(state.computer_remote && state.computer_apps.iter().any(|name| name == app))
    }

    /// Preview a fresh project folder before creating it or starting Work.
    pub(super) fn plan_session(&self, goal: &str, options: PlanOptions) -> Result<Task, String> {
        let root = match self.session_root.lock().ok().and_then(|r| r.clone()) {
            Some(root) => root,
            None => dirs::home_dir()
                .ok_or("No home folder for the session")?
                .join("x-sessions"),
        };
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = root.join(format!("session-{stamp}"));
        self.plan_task(
            goal,
            Mode::Work,
            None,
            PlanOptions {
                new_workspace: Some(dir),
                ..options
            },
        )
    }

    pub(super) fn plan_task(
        &self,
        text: &str,
        mode: Mode,
        preset: Option<Vec<Action>>,
        options: PlanOptions,
    ) -> Result<Task, String> {
        let text = text.trim();
        if text.is_empty() || text.chars().count() > MAX_INPUT {
            return Err("Enter a task of 1-1,500 characters".into());
        }
        let phase = self.lock().phase;
        if phase.is_busy() {
            return Err(busy(phase));
        }
        let prepared = if mode == Mode::Actions {
            let actions = preset.or_else(|| tools::plan_rules(text)).ok_or(
                "Not understood. Try 'open Firefox' or 'search YouTube for ...'. Nothing was run.",
            )?;
            tools::validate_sequence(&actions)?;
            if actions.iter().any(|a| matches!(a, Action::Computer { .. })) {
                self.check_screen_may_leave()?;
            }
            let standalone = actions.len() == 1;
            actions
                .into_iter()
                .map(|action| {
                    let action = action.validate()?;
                    let target = match &action {
                        Action::Window { op, target, ws } if standalone => {
                            self.launcher.prepare_window(&crate::wm::Request {
                                op: op.clone(),
                                target: target.clone(),
                                ws: ws.clone(),
                            })?
                        }
                        _ => None,
                    };
                    Ok((action, target))
                })
                .collect::<Result<Vec<_>, String>>()?
        } else {
            Vec::new()
        };
        let mut s = self.lock();
        if s.phase.is_busy() {
            return Err(busy(s.phase));
        }
        if let Some(route) = &options.voice_route {
            route.check()?;
        }
        if let Some(planning) = &options.model_plan {
            planning.stop.check()?;
        }
        let id = s.serial + 1;
        let (mut steps, actions, workspace) = match mode {
            Mode::Actions => {
                let actions: Vec<_> = prepared
                    .into_iter()
                    .enumerate()
                    .map(|(step, (action, target))| {
                        ActionRequest::new(
                            ActionId { task: id, step },
                            options.origin,
                            options.source,
                            action,
                            target,
                        )
                    })
                    .collect();
                let steps = actions
                    .iter()
                    .map(|request| Step {
                        title: request.title(),
                        status: "pending".into(),
                        risk: action_risk_label(&request.action).into(),
                        request: Some(request.metadata.clone()),
                        detail: String::new(),
                        execution: None,
                    })
                    .collect();
                (steps, actions, None)
            }
            Mode::Work => {
                if let Some(selection) = &options.composer_selection {
                    self.check_composer_selection(selection, &s)?;
                }
                let ws = options
                    .new_workspace
                    .as_ref()
                    .or(s.workspace.as_ref())
                    .ok_or("Choose a project workspace first")?;
                if let Some(review) = &options.artifact_review {
                    review.validate_workspace(ws)?;
                }
                if let Some(session) = &options.session
                    && (session.workspace != *ws
                        || session.id.kind() != options.work_harness
                        || (options.work_harness == harness::Kind::OpenCode
                            && self.session_verifier.is_none()))
                {
                    return Err("The selected session is unavailable in this project".into());
                }
                if self.work_adapter(options.work_harness).is_none() {
                    return Err(format!(
                        "{} CLI not found on PATH",
                        options.work_harness.label()
                    ));
                }
                let step = |t: &str, risk: &str| Step {
                    title: t.into(),
                    status: "pending".into(),
                    risk: risk.into(),
                    request: None,
                    detail: String::new(),
                    execution: None,
                };
                let description = match &options.session {
                    Some(session) => format!(
                        "Send to {} session {} in {}",
                        options.work_harness.label(),
                        session.id.as_str(),
                        ws.display()
                    ),
                    None => format!("Run {} in {}", options.work_harness.label(), ws.display()),
                };
                let access = if options.artifact_review.is_some() {
                    "sends the explicitly reviewed diff excerpt to Claude Code with tools disabled"
                } else if options.work_harness == harness::Kind::ClaudeCode
                    || options.work_access == harness::WorkAccess::SelectionOnly
                {
                    "tool-free analysis of the supplied prompt; no workspace edits"
                } else {
                    "can modify files in this folder"
                };
                let steps = vec![
                    step(&description, access),
                    step("Stream agent activity", ""),
                    step(
                        if options.artifact_review.is_some() {
                            "Report advisory findings without changing verification"
                        } else {
                            "Report exit status and changed files"
                        },
                        "",
                    ),
                ];
                (steps, vec![], Some(ws.display().to_string()))
            }
        };
        let window_dialogue = window_dialogue::PendingWindow::from_requests(&actions)?;
        let phase = if window_dialogue.is_some() {
            steps[0].status = "waiting_input".into();
            steps[0].detail = "Choose a displayed window, then review the exact target".into();
            Phase::WaitingForInput
        } else {
            Phase::AwaitingApproval
        };
        s.serial = id;
        s.review = if mode == Mode::Actions
            && window_dialogue.is_none()
            && !options.require_review
            && actions
                .iter()
                .all(|request| request.metadata.risk.is_low_risk())
        {
            Review::LowRisk
        } else {
            Review::Required
        };
        s.create_workspace = options.new_workspace.is_some();
        s.grants = options.allow_command.into_iter().collect();
        s.guard = options.guard;
        s.artifact_review = options.artifact_review;
        s.edit = None;
        s.computer_dialogue = None;
        s.window_dialogue = window_dialogue;
        s.workflow = None;
        s.graph_workflow = None;
        s.project_command = None;
        let task = Task {
            id,
            mode,
            text: text.into(),
            steps,
            phase,
            error: None,
            summary: None,
            changed_files: None,
            workspace,
            session: options.session,
            accepted_session: None,
            work_access: if options.work_harness == harness::Kind::ClaudeCode {
                harness::WorkAccess::SelectionOnly
            } else {
                options.work_access
            },
            work_harness: (mode == Mode::Work).then_some(options.work_harness),
            blocked: vec![],
            edit: None,
        };
        s.phase = phase;
        s.actions = actions;
        s.task = Some(task.clone());
        audit_best_effort(&mut s, AuditKind::Planned, None);
        emit(
            &mut s,
            "plan",
            if mode == Mode::Work {
                "Planned work"
            } else {
                "Planned actions"
            },
        );
        Ok(task)
    }
}

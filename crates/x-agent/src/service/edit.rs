//! Voice edits as service tasks: the preview is a task waiting for approval, so Stop, the
//! approval prompt and cancellation work exactly as they do for every other action.
//!
//! Nothing in the source text changes until the person accepts a replacement, and the
//! change is only applied after the target is focused, re-read and found unchanged.

use serde::Serialize;

use super::{Mode, Service, Step, Task, busy, emit};
use crate::edit::commands::{Command, Scope, Transform};
use crate::edit::diff::{self, Seg};
use crate::edit::text::{self, char_len};
use crate::edit::{EditFail, Origin, Target, model};
use crate::phase::Phase;

/// What the Rune pane shows for a pending edit.
#[derive(Clone, Debug, Serialize)]
pub struct Preview {
    /// "model" or "deterministic": whether a model produced the proposal.
    pub kind: &'static str,
    /// "consent" (asks before text leaves this machine), "replace", or "info".
    pub stage: &'static str,
    pub label: String,
    pub original: String,
    pub proposed: Option<String>,
    pub diff: Option<Vec<Seg>>,
    pub model: Option<String>,
    pub remote: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Stage {
    Consent,
    Replace,
    Info,
}

/// The private working state of one pending edit. Held in memory only; dropped the
/// moment the edit is applied, cancelled or fails.
#[derive(Clone)]
pub(super) struct Job {
    pub transform: Option<Transform>,
    pub target: Target,
    pub proposed: Option<String>,
    pub stage: Stage,
    pub consented: bool,
}

impl Service {
    /// True for text that is an edit command (so it should not run on the control thread).
    pub fn is_edit_command(&self, text: &str) -> bool {
        crate::edit::commands::parse(&crate::tools::strip_wake_word(text)).is_some()
    }

    /// Records dictation X just typed, so "that" can refer to it later.
    pub fn record_insertion(&self, text: &str) {
        let engine = self.context_engine();
        if let Some(window) = engine.capture(false).window_ref() {
            self.editor.record_insertion(window.clone(), text);
        }
    }

    pub fn forget_edits(&self) {
        self.editor.forget();
    }

    pub(super) fn plan_edit(&self, text: &str, cmd: Command) -> Result<Task, String> {
        match cmd {
            Command::Accept | Command::SendToModel => self.edit_answer(true),
            Command::Cancel => self.edit_answer(false),
            Command::Retry => self.retry_edit(text),
            Command::Cleanup(scope) => self.preview_cleanup(text, scope),
            Command::Transform(t, scope) => self.start_transform(text, t, scope, false),
            direct => self.run_direct(text, &direct),
        }
    }

    fn engine_and_idle(&self) -> Result<std::sync::Arc<crate::context::Engine>, String> {
        let s = self.lock();
        if s.phase.is_busy() {
            return Err(busy(s.phase));
        }
        drop(s);
        Ok(self.context_engine())
    }

    /// An edit that needs no model and no preview: applied once the target is verified.
    fn run_direct(&self, text: &str, cmd: &Command) -> Result<Task, String> {
        let engine = self.engine_and_idle()?;
        self.begin_instant(text)?;
        let result = self.direct(&engine, cmd);
        self.end_instant(result.map_err(|e| e.to_string()))
    }

    fn direct(&self, engine: &crate::context::Engine, cmd: &Command) -> Result<String, EditFail> {
        let ed = &self.editor;
        let recent = || -> Result<Target, EditFail> {
            let whole = ed.resolve(engine, Scope::Recent)?;
            if whole.origin == Origin::Selection {
                return Err(EditFail::Failed(
                    "Something is selected. This only works on text X just typed; use the selection instead".into(),
                ));
            }
            Ok(whole)
        };
        let within = |whole: &Target, rel: Option<(usize, usize)>| {
            rel.ok_or_else(|| EditFail::Changed("There is nothing there to remove".into()))
                .map(|r| (whole.clone(), r))
        };
        let message = match cmd {
            Command::ScratchThat => ed.scratch(engine)?.message,
            Command::RemoveLastSentence => {
                let whole = recent()?;
                let (whole, rel) = within(&whole, text::last_sentence_removal(&whole.text))?;
                ed.remove(engine, &whole, rel)?;
                "Removed the last sentence".into()
            }
            Command::DeleteLastWord => {
                let whole = recent()?;
                let (whole, rel) = within(&whole, text::last_word_removal(&whole.text))?;
                ed.remove(engine, &whole, rel)?;
                "Deleted the last word".into()
            }
            Command::NewParagraph => ed.insert_at_caret(engine, "\n\n")?.message,
            Command::SelectLastSentence => {
                let whole = recent()?;
                let (whole, rel) = within(&whole, text::last_sentence(&whole.text))?;
                ed.select(engine, &crate::edit::Editor::narrow(&whole, rel))?
                    .message
            }
            Command::UndoThat => ed.undo(engine)?.message,
            _ => return Err(EditFail::Failed("Not a direct edit".into())),
        };
        Ok(message)
    }

    /// Idle -> planning -> completed/failed, so a quick edit is a real task the UI can show.
    pub(super) fn begin_instant(&self, text: &str) -> Result<(), String> {
        let mut s = self.lock();
        if s.phase.is_busy() {
            return Err(busy(s.phase));
        }
        s.grants.clear();
        s.guard = None;
        s.edit = None;
        super::set_phase(&mut s, Phase::Planning);
        s.serial += 1;
        let id = s.serial;
        s.task = Some(task(id, text, vec![], None));
        emit(&mut s, "plan", &format!("Edit: {text}"));
        Ok(())
    }

    pub(super) fn end_instant(&self, result: Result<String, String>) -> Result<Task, String> {
        let mut s = self.lock();
        let (phase, error, summary) = match result {
            Ok(m) => (Phase::Completed, None, Some(m)),
            Err(e) => (Phase::Failed, Some(e), None),
        };
        super::set_phase(&mut s, phase);
        if let Some(t) = s.task.as_mut() {
            t.error = error.clone();
            t.summary = summary.clone();
        }
        let line = error.clone().or(summary).unwrap_or_default();
        emit(
            &mut s,
            if error.is_some() { "error" } else { "result" },
            &line,
        );
        let done = s.task.clone().ok_or("no task")?;
        match error {
            Some(e) => Err(e),
            None => Ok(done),
        }
    }

    fn preview_cleanup(&self, text: &str, scope: Scope) -> Result<Task, String> {
        let engine = self.engine_and_idle()?;
        let target = self
            .editor
            .resolve(&engine, scope)
            .map_err(|e| e.to_string())?;
        let proposed = text::cleanup(&target.text)
            .ok_or("The text already has clean spacing and punctuation")?;
        self.begin_preview(
            text,
            "Clean up spacing and punctuation",
            target,
            None,
            "deterministic",
            proposed,
            None,
        )
    }

    fn start_transform(
        &self,
        text: &str,
        t: Transform,
        scope: Scope,
        consented: bool,
    ) -> Result<Task, String> {
        let engine = self.engine_and_idle()?;
        let target = self
            .editor
            .resolve(&engine, scope)
            .map_err(|e| e.to_string())?;
        self.transform_target(text, t, target, consented)
    }

    fn transform_target(
        &self,
        text: &str,
        t: Transform,
        target: Target,
        consented: bool,
    ) -> Result<Task, String> {
        if t == Transform::Bullets
            && let Some(list) = text::bullets(&target.text)
        {
            return self.begin_preview(
                text,
                &t.label(),
                target,
                Some(t),
                "deterministic",
                list,
                None,
            );
        }
        let planner = self
            .planner
            .lock()
            .ok()
            .and_then(|p| p.clone())
            .ok_or("Rewriting needs a model: set one with `agent planner`")?;
        if !planner.is_local() && !consented {
            return self.begin_consent(text, t, target, &planner.name());
        }
        self.emit(
            "info",
            &format!(
                "Asking {} to edit {} characters",
                planner.name(),
                char_len(&target.text)
            ),
        );
        let raw = planner
            .complete(&model::prompt(&t, &target.text))
            .map_err(|e| format!("The model could not edit the text: {e}"))?;
        let proposed = model::parse_reply(&raw, &target.text, &t)?;
        let remote = !planner.is_local();
        if t == Transform::Explain {
            return self.explain(text, &t, target, proposed, &planner.name(), remote);
        }
        self.begin_preview(
            text,
            &t.label(),
            target,
            Some(t),
            "model",
            proposed,
            Some((planner.name(), remote)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_preview(
        &self,
        text: &str,
        label: &str,
        target: Target,
        transform: Option<Transform>,
        kind: &'static str,
        proposed: String,
        model: Option<(String, bool)>,
    ) -> Result<Task, String> {
        let preview = Preview {
            kind,
            stage: "replace",
            label: label.into(),
            original: target.text.clone(),
            diff: diff::words(&target.text, &proposed),
            proposed: Some(proposed.clone()),
            remote: model.as_ref().is_some_and(|m| m.1),
            model: model.map(|m| m.0),
        };
        let job = Job {
            transform,
            target,
            proposed: Some(proposed),
            stage: Stage::Replace,
            consented: preview.remote,
        };
        self.begin_task(
            text,
            vec![step(
                &format!("Replace the text: {label}"),
                "changes text in the focused app, only after you accept",
            )],
            preview,
            job,
        )
    }

    fn begin_consent(
        &self,
        text: &str,
        t: Transform,
        target: Target,
        model: &str,
    ) -> Result<Task, String> {
        let preview = Preview {
            kind: "model",
            stage: "consent",
            label: t.label(),
            original: target.text.clone(),
            proposed: None,
            diff: None,
            model: Some(model.into()),
            remote: true,
        };
        let job = Job {
            transform: Some(t),
            target,
            proposed: None,
            stage: Stage::Consent,
            consented: false,
        };
        self.begin_task(
            text,
            vec![step(
                &format!("Send the selected text to {model}"),
                "this text leaves your machine",
            )],
            preview,
            job,
        )
    }

    fn explain(
        &self,
        text: &str,
        t: &Transform,
        target: Target,
        answer: String,
        model: &str,
        remote: bool,
    ) -> Result<Task, String> {
        let preview = Preview {
            kind: "model",
            stage: "info",
            label: t.label(),
            original: target.text.clone(),
            proposed: Some(answer.clone()),
            diff: None,
            model: Some(model.into()),
            remote,
        };
        let job = Job {
            transform: Some(t.clone()),
            target,
            proposed: Some(answer),
            stage: Stage::Info,
            consented: true,
        };
        let task = self.begin_task(text, vec![step("Show the explanation", "")], preview, job)?;
        self.approve(task.id)?; // nothing to change: showing it is the whole job
        Ok(task)
    }

    fn begin_task(
        &self,
        text: &str,
        steps: Vec<Step>,
        preview: Preview,
        job: Job,
    ) -> Result<Task, String> {
        let mut s = self.lock();
        if s.phase.is_busy() {
            return Err(busy(s.phase));
        }
        s.grants.clear();
        s.guard = None;
        s.serial += 1;
        let id = s.serial;
        let mut t = task(id, text, steps, Some(preview));
        t.phase = Phase::AwaitingApproval;
        s.phase = Phase::AwaitingApproval;
        s.actions = vec![];
        s.edit = Some(job);
        s.task = Some(t.clone());
        emit(&mut s, "plan", &format!("Edit ready to review: {text}"));
        Ok(t)
    }

    /// "replace it" (accept), "send it" (consent) or "cancel the edit".
    fn edit_answer(&self, accept: bool) -> Result<Task, String> {
        let (id, waiting) = {
            let s = self.lock();
            let id = s.task.as_ref().map(|t| t.id);
            (id, s.phase == Phase::AwaitingApproval && s.edit.is_some())
        };
        let id = id
            .filter(|_| waiting)
            .ok_or("There is no edit waiting for an answer")?;
        if accept {
            self.approve(id)?;
        } else {
            self.cancel();
        }
        self.lock().task.clone().ok_or_else(|| "no task".into())
    }

    fn retry_edit(&self, text: &str) -> Result<Task, String> {
        let job = {
            let s = self.lock();
            let waiting = s.phase == Phase::AwaitingApproval;
            s.edit
                .clone()
                .filter(|_| waiting)
                .ok_or("There is no edit to retry")?
        };
        let transform = job.transform.clone().ok_or("That edit cannot be retried")?;
        self.cancel(); // drops the old proposal first
        self.transform_target(text, transform, job.target, job.consented)
    }

    /// The approved half of an edit: consent -> ask the model, replace -> apply, info -> show.
    pub(super) fn run_edit(&self, task: &Task) {
        let Some(job) = self.lock().edit.clone() else {
            return self.finish(
                Phase::Failed,
                Some("The edit is no longer pending".into()),
                None,
                None,
            );
        };
        match job.stage {
            Stage::Consent => self.run_consented(task, job),
            Stage::Info => {
                self.set_step(0, "done");
                self.finish(Phase::Completed, None, job.proposed, None);
            }
            Stage::Replace => self.run_replace(job),
        }
    }

    fn run_consented(&self, task: &Task, job: Job) {
        let Some(transform) = job.transform.clone() else {
            return self.finish(Phase::Failed, Some("Nothing to send".into()), None, None);
        };
        self.set_step(0, "done");
        self.finish(
            Phase::Completed,
            None,
            Some("Sent to the model with your approval".into()),
            None,
        );
        if let Err(e) = self.transform_target(&task.text, transform, job.target, true) {
            self.emit("error", &e);
        }
    }

    fn run_replace(&self, job: Job) {
        let (Some(new), engine) = (job.proposed.clone(), self.context_engine()) else {
            return self.finish(Phase::Failed, Some("Nothing to apply".into()), None, None);
        };
        if self.lock().stop_requested {
            return self.finish(
                Phase::Cancelled,
                None,
                Some("Stopped; nothing was changed".into()),
                None,
            );
        }
        self.set_step(0, "running");
        match self.editor.apply(&engine, &job.target, &new) {
            Ok(_) => {
                self.set_step(0, "done");
                let note = format!(
                    "Replaced {} characters with {} in {}. Say undo that to revert.",
                    char_len(&job.target.text),
                    char_len(&new),
                    job.target.window.class
                );
                self.finish(Phase::Completed, None, Some(note), None);
            }
            Err(e) => {
                self.set_step(0, "failed");
                self.finish(Phase::Failed, Some(e.to_string()), None, None);
            }
        }
    }
}

pub(super) fn step(title: &str, risk: &str) -> Step {
    Step {
        title: title.into(),
        status: "pending".into(),
        risk: risk.into(),
        detail: String::new(),
    }
}

pub(super) fn task(id: u64, text: &str, steps: Vec<Step>, edit: Option<Preview>) -> Task {
    Task {
        id,
        mode: Mode::Actions,
        text: text.into(),
        steps,
        phase: Phase::Planning,
        error: None,
        summary: None,
        changed_files: None,
        workspace: None,
        blocked: vec![],
        edit,
    }
}
